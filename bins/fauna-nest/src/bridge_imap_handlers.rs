//! WS-RPC handlers for the I2b IMAP metadata surface (Phase C).
//!
//! All handlers in this module require `BridgeMda` caller class — they
//! serve the MDA (IMAP-facing bridge), not the MTA (submission-facing
//! bridge).
//!
//! Phase C tasks add to this file:
//!   C.1 — `list_mailboxes`, `select_mailbox`
//!   C.2 — `list_messages`, `fetch_message_metadata`
//!   C.3–C.6 — body-fetch, flag-store, expunge, search, idle (future)

use std::time::Duration;

use fauna_protocol::{
    PushEvent,
    bridge_routing::{
        AppendMessageReply, AppendMessageRequest, BridgeSpamModelResetPush,
        BridgeSpamModelUpdatedPush, CopyMessagesReply, CopyMessagesRequest, CopyPair,
        CreateMailboxReply, CreateMailboxRequest, DeleteMailboxReply, DeleteMailboxRequest,
        ExpungeReply, ExpungeRequest, FetchIndexSegmentsSinceReply, FetchIndexSegmentsSinceRequest,
        FetchMessageCiphertextReply, FetchMessageCiphertextRequest, FetchMessageMetadataReply,
        FetchMessageMetadataRequest, GetQuotaReply, GetQuotaRequest, GetSpamBaselineStateReply,
        GetSpamBaselineStateRequest, HeaderField, IndexSegment, ListMailboxesReply,
        ListMailboxesRequest, ListMessagesReply, ListMessagesRequest, ListSpamTrainingHistoryReply,
        ListSpamTrainingHistoryRequest, MailboxEntry, MailboxStateEvent, MessageMeta,
        MoveMessagesReply, MoveMessagesRequest, MoveSide, PublishSpamBaselineReply,
        PublishSpamBaselineRequest, RenameMailboxReply, RenameMailboxRequest, ResetSpamModelReply,
        ResetSpamModelRequest, SearchMessagesReply, SearchMessagesRequest, SearchTerm,
        SelectMailboxReply, SelectMailboxRequest, SetBaselineContributionReply,
        SetBaselineContributionRequest, SpamHistoryOp, SpamLabel, SpamTrainingHistoryRow,
        StoreFlagsOp, StoreFlagsReply, StoreFlagsRequest, StoreFlagsResultEntry,
        SubscribeMailboxReply, SubscribeMailboxRequest, SubscribeMailboxStateReply,
        SubscribeMailboxStateRequest, TrainingSource, UnsubscribeMailboxReply,
        UnsubscribeMailboxRequest,
    },
    decode_strict as decode,
    wrapped_blob::{
        FetchSpamModelReply, FetchSpamModelRequest, GetSpamScoringPolicyReply,
        GetSpamScoringPolicyRequest, HolderSealTarget, PutSpamModelOutcome, PutSpamModelReply,
        PutSpamModelRequest,
    },
};
use serde_bytes::ByteBuf;

use crate::bridge_method_allowlist::CallerClass;
use crate::db::now_epoch_secs;

use crate::bridge_routing_handlers::{
    encode_reply, ensure_actor_mail_serving_enabled, held_for_review, internal, malformed,
    over_quota, permission_denied, placement_journal_diverged, pure_backup_destination,
    refuse_if_pure_backup, require_class, seal_recipient_blob,
};
use crate::email_handlers::{invalid_params, message_too_large};

/// RFC 9208 over-quota decision for the per-actor `user/<handle>` quota
/// root. Returns the resource a post-write total would exceed, or `None`
/// if the write fits. Quota is a ceiling: a write is rejected only when the
/// projected total *strictly* exceeds the limit (`used + added > limit`) —
/// exactly filling the quota is allowed. STORAGE (RES-STORAGE) is checked
/// before MESSAGE (RES-MESSAGE); either trips independently. Pure so the
/// arithmetic + boundary are unit-testable without a DB. See
/// `docs/goal/behavior/imap-server.md` § Quota root model / § Quota
/// enforcement points.
pub(crate) fn imap_quota_overage(
    used_bytes: u64,
    used_count: u32,
    added_bytes: u64,
    added_count: u32,
    storage_limit: u64,
    count_limit: u32,
) -> Option<&'static str> {
    if storage_overage(used_bytes, added_bytes, storage_limit) {
        return Some("storage");
    }
    if used_count.saturating_add(added_count) > count_limit {
        return Some("message");
    }
    None
}

/// The STORAGE half of [`imap_quota_overage`], alone — the one axis a DAV
/// write draws on ([`enforce_dav_write_quota`]).
fn storage_overage(used_bytes: u64, added_bytes: u64, storage_limit: u64) -> bool {
    used_bytes.saturating_add(added_bytes) > storage_limit
}

/// Pre-check a DAV write — an event or card PUT, a MOVE/COPY destination, an
/// emailed invitation's placement — against the shared storage quota
/// (`caldav-server.md` § QUOTA → § Enforcement points). `new_size` is the
/// incoming body's `ciphertext_size`; `replaced_size` the `ciphertext_size` of
/// the row the write replaces (`None` for a new event or card). A write that
/// does not grow is never checked at all: "shrinking writes always allowed"
/// must hold even for an account an admin has put over a lowered ceiling,
/// where a zero delta would still trip the `used + added > limit` test. Only
/// the STORAGE ceiling binds — an event or card is not a mailbox MESSAGE, so a
/// lowered message limit never refuses one. Same pre-check semantics as
/// [`enforce_imap_storage_quota`] (not a transactional reservation).
pub(crate) async fn enforce_dav_write_quota(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    new_size: u32,
    replaced_size: Option<u32>,
) -> Result<(), fauna_protocol::RpcError> {
    let added = u64::from(new_size).saturating_sub(u64::from(replaced_size.unwrap_or(0)));
    if added == 0 {
        return Ok(());
    }
    let policy = state
        .db
        .get_imap_policy()
        .await
        .map_err(internal)?
        .effective();
    let (used_bytes, _) = imap_quota_usage(state, actor).await?;
    if storage_overage(used_bytes, added, policy.storage_bytes_default) {
        return Err(over_quota("storage"));
    }
    Ok(())
}

/// Pre-check a pending mailbox write against the actor's quota root. Reads
/// live usage (`imap_quota_usage` — STORAGE = Σ record block length over the
/// actor's non-tombstoned mail placements and live calendar/card records,
/// sized through the CARv2 index by `record_cid`; MESSAGE = mail placement
/// count) and the deployment limits from
/// `policy`, then applies [`imap_quota_overage`]. On
/// over-quota returns the typed `fauna.bridges.over_quota` error the Go MDA
/// maps to `NO [OVERQUOTA]` and the Go MTA maps to `552 5.2.2`. Shared by
/// APPEND/COPY/MOVE here and inbound delivery in `bridge_routing_handlers`.
///
/// This is a pre-check, not a transactional reservation — under concurrent
/// writes it can slightly over-admit, matching Dovecot's quota semantics
/// and the goal doc's "pre-check on the nest RPC" language.
pub(crate) async fn enforce_imap_storage_quota(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    added_bytes: u64,
    added_count: u32,
) -> Result<(), fauna_protocol::RpcError> {
    // Read the *effective* deployment policy (catalog ⊕ admin
    // `put_imap_policy` override) once per call — matching `resolve_recipient`'s
    // per-call `get_alias_policy().effective()` (no hot-reload, no cache). This
    // is what makes the admin-configured quota ceiling actually bind; the
    // reporting handler (`get_quota_handler`) reads the *same* effective source
    // so `GETQUOTA` limits and the threshold this rejects at never disagree.
    // Per-tier / per-actor ceilings are Phase F+ (imap-server.md § Resources);
    // the single `mail_imap_policy` row is deployment-wide today.
    let policy = state
        .db
        .get_imap_policy()
        .await
        .map_err(internal)?
        .effective();
    let (used_bytes, used_count) = imap_quota_usage(state, actor).await?;
    if let Some(resource) = imap_quota_overage(
        used_bytes,
        used_count,
        added_bytes,
        added_count,
        policy.storage_bytes_default,
        policy.message_count_default,
    ) {
        return Err(over_quota(resource));
    }
    Ok(())
}

/// Refuses to serve IMAP for `target` when either gate fails. Called at the top
/// of every IMAP-serving handler in this module (all `BridgeMda`-only, so this
/// only ever runs for the MDA serving an MUA — never for a user's own client):
///
/// 1. **Pure-backup destination** — this nest holds opaque chunks only for
///    `target`'s `__mail` folder; IMAP cannot serve from opaque chunks
///    (`docs/goal/architecture/message-segment-store.md` § Destination
///    capability gate 1).
/// 2. **Per-actor serving opt-out** — `target` has turned IMAP/CalDAV serving
///    OFF on this nest (user-set, default ON; they read their mail elsewhere,
///    e.g. a paired residential nest). Other actors are unaffected
///    (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
///    § MUA reach).
pub(crate) async fn require_local_mail_serving(
    state: &std::sync::Arc<crate::routes::AppState>,
    target: &[u8; 32],
) -> Result<(), fauna_protocol::RpcError> {
    refuse_if_pure_backup(state, "mail", target, pure_backup_destination).await?;
    ensure_actor_mail_serving_enabled(state, target).await?;
    Ok(())
}
use crate::db::bridge_imap::{
    CreateMailboxDbOutcome, DeleteMailboxDbOutcome, GUARDIAN_HELD_MAILBOX, IndexSegmentRow,
    MessageMetaRow, NewlySeededMailbox, RenameMailboxDbOutcome, SearchHeaderFieldDb, SearchTermDb,
    StoreFlagsDbOp, is_protected_mailbox, standard_mailbox_attrs, validate_mailbox_name,
};
use crate::db::bridge_routing::InboundMailFields;
use crate::db::moderation::SpamHistoryDbOp;
use crate::rpc_router::{RpcKindMeta, RpcRouterBuilder};
use fauna_mail::segments::placement::MailPlacementRecord;

/// Emit one `MailPlacementRecord::Create` per entry returned by
/// `ensure_bridge_imap_mailboxes`. Every IMAP-serving handler that
/// calls `ensure_bridge_imap_mailboxes` to bootstrap the six standard
/// mailboxes on a fresh actor must funnel the returned Vec through
/// here BEFORE emitting any other placement event in the same handler.
/// Spec § D2 (Create record shape); spec § D6 (ε) atomic-with-SQL
/// note — the SQLite `INSERT OR IGNORE` inside
/// `ensure_bridge_imap_mailboxes` has already committed by the time
/// this runs, so a crash here leaves the DB rows present but the
/// placement journal missing the Create entries. Plan 2 T9's
/// divergence detection at SELECT / QRESYNC time is the repair
/// mechanism (same crash-window deferral as T7's APPEND / STORE /
/// EXPUNGE wiring and T8's MOVE / COPY wiring).
///
/// Idempotency: when every standard mailbox is already present (the
/// `Vec` is empty), this function is a no-op — no duplicate Create
/// events emitted on repeat calls to `ensure_bridge_imap_mailboxes`.
pub(crate) async fn emit_bootstrap_create_records(
    state: &std::sync::Arc<crate::routes::AppState>,
    target: &[u8; 32],
    newly_seeded: Vec<NewlySeededMailbox>,
) -> Result<(), fauna_protocol::RpcError> {
    for seeded in newly_seeded {
        let record = MailPlacementRecord::Create {
            mailbox: seeded.name,
            uid_validity: seeded.uid_validity,
            attrs: seeded.attrs,
        };
        state
            .mail_placement
            .append_event(target, &record)
            .await
            .map_err(placement_journal_diverged)?;
    }
    Ok(())
}

/// Split a space-separated flags column value into a Vec of IMAP flag
/// tokens. Empty / whitespace-only input → empty Vec. Used to build the
/// `before_flags` / `after_flags` / `flags` fields of placement records
/// from the DB's space-separated representation.
pub(crate) fn split_flags(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

/// Map DB-layer metadata rows into wire-shape `MessageMeta`s, filling
/// RFC822.SIZE for each by sizing the record through the CARv2 segment index by
/// `record_cid` (`segments::record_sizes`) — size is **not** a SQL mirror
/// column (`imap-server.md` § SEARCH). Flags are stored space-separated; the
/// wire surface exposes them as a `Vec<String>` (empty Vec when whitespace). A
/// record whose segment can't be opened reports size 0 (mirror/disk divergence,
/// logged by `record_sizes`) rather than failing the whole listing.
async fn rows_to_message_metas(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    rows: Vec<MessageMetaRow>,
) -> Result<Vec<MessageMeta>, fauna_protocol::RpcError> {
    let refs: Vec<(u32, fauna_cbor::Cid)> =
        rows.iter().map(|r| (r.segment_id, r.record_cid)).collect();
    let sizes = crate::segments::record_sizes(&state.mail_segments, actor, &refs)
        .await
        .map_err(internal)?;
    Ok(rows
        .into_iter()
        .zip(sizes)
        .map(|(r, size)| MessageMeta {
            uid: r.uid,
            message_id: r.message_id.to_vec(),
            modseq: r.modseq,
            flags: split_flags(&r.flags),
            internal_date: r.internal_date,
            ciphertext_size: size.unwrap_or(0) as u32,
            seq_num: r.seq_num,
        })
        .collect())
}

/// Σ record block length over `refs` (`(segment_id, record_cid)`), each sized
/// through the CARv2 index by `record_cid`. Divergent records whose segment
/// can't be opened contribute 0 (`segments::record_sizes` logs them).
async fn sum_record_sizes(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    refs: &[(u32, fauna_cbor::Cid)],
) -> Result<u64, fauna_protocol::RpcError> {
    let sizes = crate::segments::record_sizes(&state.mail_segments, actor, refs)
        .await
        .map_err(internal)?;
    Ok(sizes.into_iter().map(|s| s.unwrap_or(0)).sum())
}

/// Live STORAGE/MESSAGE usage for `actor`'s quota root. STORAGE is the ONE
/// number across mail, calendar and contacts (`caldav-server.md` § QUOTA —
/// shared with IMAP): Σ record block length over the actor's non-tombstoned
/// mail placements plus its live calendar-event and card records, each sized
/// through its own kind's CARv2 index by `record_cid` (`imap-server.md`
/// § QUOTA — never a SQL byte column). MESSAGE is the mail placement count
/// alone — RFC 9208's MESSAGE resource counts mailbox messages, and an event is
/// not one. Shared by `enforce_imap_storage_quota` and `get_quota_handler` so
/// reporting and enforcement read the same usage.
pub(crate) async fn imap_quota_usage(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
) -> Result<(u64, u32), fauna_protocol::RpcError> {
    let refs = state
        .db
        .list_bridge_imap_quota_size_refs(actor)
        .await
        .map_err(internal)?;
    let count = refs.len() as u32;
    let mail_bytes = sum_record_sizes(state, actor, &refs).await?;
    let dav = state
        .db
        .list_bridge_dav_quota_size_refs(actor)
        .await
        .map_err(internal)?;
    let mut dav_bytes = 0u64;
    for (mgr, refs) in [
        (&state.cal_segments, &dav.calendar),
        (&state.card_segments, &dav.card),
    ] {
        let sizes = crate::segments::record_sizes(mgr, actor, refs)
            .await
            .map_err(internal)?;
        dav_bytes += sizes.into_iter().map(|s| s.unwrap_or(0)).sum::<u64>();
    }
    Ok((mail_bytes + dav_bytes, count))
}

// ── list_mailboxes ────────────────────────────────────────────────

fn list_mailboxes_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_mailboxes").await?;
            let req: ListMailboxesRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;
            // D.7: `LSUB` / `LIST (SUBSCRIBED)` set `subscribed_only =
            // true`; plain LIST leaves it false and gets the full
            // mailbox-state set. The subscribed-only path joins through
            // `bridge_imap_subscriptions`.
            let state_rows = if req.subscribed_only {
                state
                    .db
                    .list_bridge_imap_subscribed_mailbox_state(&target)
                    .await
                    .map_err(internal)?
            } else {
                state
                    .db
                    .list_bridge_imap_mailbox_state(&target)
                    .await
                    .map_err(internal)?
            };
            let mut mailboxes = Vec::with_capacity(state_rows.len());
            for row in state_rows {
                let (exists, unseen) = state
                    .db
                    .count_bridge_imap_mailbox(&target, &row.name)
                    .await
                    .map_err(internal)?;
                mailboxes.push(MailboxEntry {
                    name: row.name,
                    uid_validity: row.uid_validity,
                    uid_next: row.uid_next,
                    highestmodseq: row.highestmodseq,
                    exists,
                    unseen,
                });
            }
            encode_reply(&ListMailboxesReply { mailboxes })
        })
    })
}

// ── select_mailbox ────────────────────────────────────────────────

fn select_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.select_mailbox").await?;
            let req: SelectMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;
            let row = state
                .db
                .get_bridge_imap_mailbox_state(&target, &req.mailbox)
                .await
                .map_err(internal)?;
            let reply = match row {
                None => SelectMailboxReply::NoSuchMailbox,
                Some(row) => {
                    // Placement-journal-vs-SQLite (ε) reconciliation check
                    // (spec § D6 (ε)). The handler that committed this RPC's
                    // state to SQLite then appended a placement event to the
                    // open segment — but a crash between commit and append
                    // leaves the manifest's per-mailbox highestmodseq lagging
                    // SQLite's. We can't synthesize the missing record from
                    // SQLite alone (StoreFlags.before_flags isn't preserved,
                    // Create-vs-Rename is ambiguous), so v1 just detects and
                    // WARN-logs the gap for admin visibility — see
                    // imap-server.md § Architectural rules. QRESYNC clients
                    // with sync-tokens in the gap fall through to full resync
                    // per RFC 7162 §3.2.5.2 stale-modseq (safe by default).
                    match state.mail_placement.load_manifest(&target).await {
                        Ok(manifest) => {
                            if let Some(mb) = manifest.mailboxes.iter().find(|m| m.name == row.name)
                                && (mb.highestmodseq as i64) < row.highestmodseq
                            {
                                let gap = row.highestmodseq - (mb.highestmodseq as i64);
                                tracing::warn!(
                                    actor_id = %hex::encode(target),
                                    mailbox = %row.name,
                                    manifest_hms = mb.highestmodseq,
                                    sqlite_hms = row.highestmodseq,
                                    gap,
                                    "placement journal lags SQLite (crash-window divergence); \
                                     synthesis deferred — see imap-server.md § Architectural rules"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                actor_id = %hex::encode(target),
                                error = %e,
                                "load_manifest failed during (ε) reconciliation check; skipping",
                            );
                        }
                    }
                    // Restore-divergence detection (spec § D6 (γ)). If the MUA
                    // supplied a QRESYNC last_modseq via SELECT (QRESYNC ...)
                    // and it's ahead of the server's highestmodseq for this
                    // mailbox, the most plausible cause is a DR restore. Log
                    // the divergence + still return Selected normally — server
                    // state wins; RFC 7162 §3.2.5.2 stale-modseq handling at
                    // the MDA emits OK [HIGHESTMODSEQ <restored>] which causes
                    // the client to fall through to full resync.
                    //
                    // The Go bridge's fork parses the QRESYNC SELECT
                    // parameter and forwards it here (`internal/mda/imap
                    // /select.go`); client_qresync is nil only when the
                    // client didn't supply QRESYNC on this SELECT.
                    if let Some(qr) = &req.client_qresync
                        && qr.last_modseq > row.highestmodseq
                    {
                        let now = fauna_core::data::Timestamp::now_secs_or_zero();
                        {
                            let conn = state.db.conn().await;
                            let tx = conn.unchecked_transaction().map_err(internal)?;
                            crate::restore::divergence::write_divergence_row(
                                &tx,
                                &target,
                                "imap",
                                &row.name,
                                req.mua_id.as_deref(),
                                qr.last_modseq,
                                row.highestmodseq,
                                now,
                            )
                            .map_err(internal)?;
                            tx.commit().map_err(internal)?;
                        }
                    }
                    let (exists, unseen) = state
                        .db
                        .count_bridge_imap_mailbox(&target, &row.name)
                        .await
                        .map_err(internal)?;
                    let first_unseen_uid = state
                        .db
                        .first_unseen_uid_in_mailbox(&target, &row.name)
                        .await
                        .map_err(internal)?;
                    SelectMailboxReply::Selected {
                        uid_validity: row.uid_validity,
                        uid_next: row.uid_next,
                        highestmodseq: row.highestmodseq,
                        exists,
                        recent: 0,
                        unseen,
                        first_unseen_uid,
                    }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── list_messages ─────────────────────────────────────────────────

fn list_messages_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_messages").await?;
            let req: ListMessagesRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;

            // Fetch one extra row to detect whether there are more pages.
            let limit_opt = if req.limit == 0 {
                None
            } else {
                Some(req.limit + 1)
            };
            let mut rows = state
                .db
                .query_bridge_imap_messages(
                    &target,
                    &req.mailbox,
                    req.since_modseq,
                    req.after_uid,
                    None,
                    limit_opt,
                )
                .await
                .map_err(internal)?;

            // Determine whether there are more pages by checking for the
            // sentinel extra row.
            let more = if limit_opt.is_some() && rows.len() > req.limit as usize {
                rows.truncate(req.limit as usize);
                true
            } else {
                false
            };

            let messages = rows_to_message_metas(&state, &target, rows).await?;

            let expunged_uids = if let Some(since) = req.since_modseq {
                state
                    .db
                    .list_bridge_imap_expunged_since(&target, &req.mailbox, since)
                    .await
                    .map_err(internal)?
            } else {
                vec![]
            };

            let highestmodseq = state
                .db
                .get_bridge_imap_mailbox_state(&target, &req.mailbox)
                .await
                .map_err(internal)?
                .map(|s| s.highestmodseq)
                .unwrap_or(1);

            encode_reply(&ListMessagesReply {
                messages,
                expunged_uids,
                highestmodseq,
                more,
            })
        })
    })
}

// ── fetch_message_metadata ────────────────────────────────────────

fn fetch_message_metadata_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_message_metadata").await?;
            let req: FetchMessageMetadataRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;

            // Empty uids = "all in mailbox" (pass None → no UID filter).
            let uid_filter: Option<&[u32]> = if req.uids.is_empty() {
                None
            } else {
                Some(&req.uids)
            };

            let rows = state
                .db
                .query_bridge_imap_messages(&target, &req.mailbox, None, None, uid_filter, None)
                .await
                .map_err(internal)?;

            // EXISTS total is independent of the UID filter (a UID-subset fetch
            // still needs the whole-mailbox count for the IDLE Append/Move-dst
            // path), so count separately rather than off `rows.len()`.
            let mailbox_total = state
                .db
                .count_bridge_imap_live_messages(&target, &req.mailbox)
                .await
                .map_err(internal)?;

            let messages = rows_to_message_metas(&state, &target, rows).await?;

            encode_reply(&FetchMessageMetadataReply {
                messages,
                mailbox_total,
            })
        })
    })
}

/// Map a DB-side `IndexSegmentRow` to the wire-shape `IndexSegment`.
fn row_to_index_segment(r: IndexSegmentRow) -> IndexSegment {
    IndexSegment {
        message_id: r.message_id.to_vec(),
        mailbox: r.mailbox,
        modseq: r.modseq,
        encrypted_index_hint: r.encrypted_index_hint,
        // ms → unix seconds; the unknown `0` stays `0` — the hint is then
        // standing-sealed and the reader's epoch chain ends on the standing
        // arm.
        stored_at: r.stored_at.max(0) / 1000,
    }
}

// ── fetch_message_ciphertext ──────────────────────────────────────

fn fetch_message_ciphertext_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_message_ciphertext").await?;
            let req: FetchMessageCiphertextRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let message_id: [u8; 32] =
                crate::rpc_errors::require_bytes32("message_id", req.message_id.as_slice())
                    .map_err(malformed)?;

            // Scope-check via the segment_records mirror, SCOPED to the target
            // actor — never the scope-agnostic actor-for-record lookup: under
            // content-hash identity a byte replay can file the same cid in two
            // scopes, and a LIMIT-1 owner pick could then deny the legitimate
            // owner. Unknown-in-this-scope surfaces as NotFound — never leak
            // cross-actor existence.
            let held = state
                .db
                .segment_records_lookup_record(
                    &target,
                    "mail",
                    &fauna_cbor::Cid::from_digest_dag_cbor(message_id),
                )
                .await
                .map_err(internal)?;
            if held.is_none() {
                return encode_reply(&FetchMessageCiphertextReply::NotFound);
            }

            // Owner matches: pull the full sealed body + floor through the
            // segment store. `read_sealed_body_with_floor` transparently resolves
            // a v3 continuation head into the concatenation of its part records,
            // so this serve leg is identical whether the message rested inline or
            // as parts (message-segment-store.md § Continuation records — "the
            // nest concatenates parts before the existing inline-or-body_ref
            // split"). The floor carries the inner `timestamp` and
            // `ciphertext_size` the wire reply needs.
            let Some((body, _hint, floor)) = crate::segments::mail::read_sealed_body_with_floor(
                &state.mail_segments,
                &state.db,
                &target,
                &message_id,
            )
            .await
            .map_err(internal)?
            else {
                return encode_reply(&FetchMessageCiphertextReply::NotFound);
            };
            // A body that fits the frame rides the reply inline, exactly as every
            // message has until now. One that does not cannot cross the RPC plane at
            // all (the 2 MiB cap is permanent — `transport.md` § Max frame), so nest
            // stages it on the byte plane and hands back a reference; the MDA GETs
            // the chunks over the open download route and rejoins them. This is the
            // only reason a `FETCH` of a large attachment can succeed at all.
            // The content-sealing-epochs classification basis:
            // the floor's append-instant, ms → unix seconds. 0 = unknown (the
            // append-time clock read failed): a standing-sealed record, which
            // the reader's epoch chain ends on.
            let stored_at = floor.stored_at.max(0) / 1000;
            let reply = if fauna_mail::body_ref::mail_body_needs_reference(body.len() as u64, 0) {
                let body_ref = crate::mail_body_plane::stage_sealed_body(&state, &body).await?;
                FetchMessageCiphertextReply::Found {
                    encrypted_body: Vec::new(),
                    ciphertext_size: floor.ciphertext_size,
                    internal_date: floor.timestamp,
                    body_ref: Some(body_ref),
                    stored_at,
                }
            } else {
                FetchMessageCiphertextReply::Found {
                    encrypted_body: body,
                    ciphertext_size: floor.ciphertext_size,
                    internal_date: floor.timestamp,
                    body_ref: None,
                    stored_at,
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── fetch_index_segments_since ────────────────────────────────────

fn fetch_index_segments_since_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.fetch_index_segments_since",
            )
            .await?;
            let req: FetchIndexSegmentsSinceRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // Ensure default mailboxes exist so probing a fresh actor doesn't error.
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;

            // Fetch limit+1 rows for page-detection (same trick as C.2).
            let limit_opt = if req.limit == 0 {
                None
            } else {
                Some(req.limit + 1)
            };

            let mut rows = state
                .db
                .query_bridge_imap_index_segments(
                    &state.mail_segments,
                    &target,
                    req.mailbox.as_deref(),
                    req.since_modseq,
                    limit_opt,
                )
                .await
                .map_err(internal)?;

            let more = if req.limit > 0 && rows.len() > req.limit as usize {
                rows.truncate(req.limit as usize);
                true
            } else {
                false
            };

            let segments: Vec<IndexSegment> = rows.into_iter().map(row_to_index_segment).collect();

            let highestmodseq = state
                .db
                .max_highestmodseq_for_actor(&target, req.mailbox.as_deref())
                .await
                .map_err(internal)?;

            encode_reply(&FetchIndexSegmentsSinceReply {
                segments,
                highestmodseq,
                more,
            })
        })
    })
}

// ── store_flags ───────────────────────────────────────────────────

fn store_flags_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.store_flags").await?;
            let req: StoreFlagsRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // Validate: uids must be non-empty.
            if req.uids.is_empty() {
                return Err(malformed("uids must be non-empty"));
            }
            // Validate: \Recent cannot be stored (RFC 3501 §2.3.2).
            if req.flags.iter().any(|f| f == "\\Recent") {
                return Err(malformed("\\Recent cannot be stored"));
            }

            // Map protocol op → DB op.
            let db_op = match req.op {
                StoreFlagsOp::Set => StoreFlagsDbOp::Set,
                StoreFlagsOp::Add => StoreFlagsDbOp::Add,
                StoreFlagsOp::Remove => StoreFlagsDbOp::Remove,
            };

            let outcome = state
                .db
                .apply_store_flags(
                    &target,
                    &req.mailbox,
                    &req.uids,
                    db_op,
                    &req.flags,
                    req.unchanged_since,
                )
                .await
                .map_err(internal)?;

            // Journal, IDLE fan-out and the app wake — one owner, shared with
            // the app's `fauna.email.inbox.mark_seen`.
            record_flag_writes(&state, &target, &req.mailbox, &outcome.updated).await?;

            // Build wire reply: split each new-flags string into Vec<String>.
            let updated: Vec<StoreFlagsResultEntry> = outcome
                .updated
                .into_iter()
                .map(|(uid, _before, flags_str, modseq)| StoreFlagsResultEntry {
                    uid,
                    flags: split_flags(&flags_str),
                    modseq,
                })
                .collect();

            encode_reply(&StoreFlagsReply {
                updated,
                highestmodseq: outcome.highestmodseq,
                modified: outcome.modified,
            })
        })
    })
}

/// Everything a committed flag write owes beyond its SQL rows, for every
/// flag writer: `fauna.bridges.store_flags` (a mail client
/// through the MDA) and `fauna.email.inbox.mark_seen` (a Fauna app). `updated`
/// is `StoreFlagsDbOutcome::updated`; entries whose flags did not change are
/// skipped throughout. Returns how many rows actually changed.
pub(crate) async fn record_flag_writes(
    state: &std::sync::Arc<crate::routes::AppState>,
    target: &[u8; 32],
    mailbox: &str,
    updated: &[(u32, String, String, i64)],
) -> Result<u32, fauna_protocol::RpcError> {
    // Spec § D6 (ε): placement-journal append. The SQLite UPDATEs
    // already committed inside the caller's `apply_store_flags` (it holds
    // the conn-mutex for the SELECT-then-UPDATE so before_flags
    // is consistent with after_flags). The placement append below
    // happens after that commit; the crash window between the two
    // is closed by Plan 2 T9's divergence detection at SELECT /
    // QRESYNC time.
    //
    // One record per UID — for `Set` the after_flags is uniform
    // across UIDs but before_flags varies per UID; for `Add` /
    // `Remove` both vary. Emitting per-UID is the simplest
    // correct shape (spec § D2: uid_set field carries one or many
    // UIDs that share the same (before_flags, after_flags) pair).
    //
    // PERFORMANCE TODO: `STORE 1:1000 +FLAGS \Seen` produces 1000
    // per-actor-mutex acquisitions, 1000 segment appends, and
    // 1000 manifest atomic-rename + fsync calls — multi-second
    // latency for a common IMAP op. Spec § D2's `uid_set` field
    // is cardinality ≥1, so UIDs that share a `(before_flags,
    // after_flags)` pair can be batched into one record. For
    // `Set` ops the after is uniform, so batching collapses
    // N→K records where K is the distinct count of prior
    // flag-strings (typically 1 or 2). Deferred — correctness
    // first, throughput second; revisit if real workloads show
    // STORE-batch tail latency hurting.
    //
    // ERROR CATEGORY: `.map_err(placement_journal_diverged)?` below
    // maps the post-commit journal-append failure to its own
    // `fauna.bridges.placement_journal_diverged` (`rpc_errors.rs`),
    // distinct from the generic `fauna.protocol.internal` — the
    // underlying SQL UPDATE already committed, so this is not "the
    // mutation didn't happen"; Plan 2 T9 repairs the divergence on
    // the next SELECT/QRESYNC. Same treatment at every other
    // `append_event` call site in this file, and in the CalDAV/
    // CardDAV siblings (`bridge_caldav_handlers.rs`,
    // `bridge_carddav_handlers.rs`).
    //
    // Entries whose flags did not actually change (`before == after` —
    // `apply_store_flags` reports an already-satisfied UID without
    // writing it) are skipped: there is no state transition to journal,
    // and emitting one would grow the manifest on every no-op STORE.
    for (uid, before_str, after_str, modseq) in updated {
        if before_str == after_str {
            continue;
        }
        let record = MailPlacementRecord::StoreFlags {
            mailbox: mailbox.to_string(),
            uid_set: vec![*uid],
            modseq: *modseq as u64,
            before_flags: split_flags(before_str),
            after_flags: split_flags(after_str),
        };
        state
            .mail_placement
            .append_event(target, &record)
            .await
            .map_err(placement_journal_diverged)?;
    }

    // I5 Phase F.1 — emit per-UID `Flags` push to any IDLE/NOTIFY
    // subscribers. Fires AFTER the SQLite UPDATE has committed
    // (above) — Spec § D6 (ε) atomicity rule. Per-UID rather
    // than batched because each subscribed MUA's translation
    // path (claim 2 in TODO § Load-bearing claims) emits one
    // `* <seq> FETCH (UID … FLAGS … MODSEQ …)` per UID
    // regardless of how the underlying STORE was issued.
    // Same `before == after` suppression as the journal loop above: a
    // no-op STORE is not a mailbox state change, so no subscriber is
    // woken for it.
    for (uid, before_str, after_str, modseq) in updated {
        if before_str == after_str {
            continue;
        }
        emit_mailbox_state_event(
            state,
            target,
            mailbox,
            MailboxStateEvent::Flags {
                uid: *uid,
                flags: split_flags(after_str),
                modseq: *modseq,
            },
        );
    }

    let changed = updated.iter().filter(|(_, b, a, _)| b != a).count() as u32;
    // The app-facing wake (`mail-app-surface.md` § Read state): an app syncs
    // INBOX read state only, so a flag write elsewhere is not its business.
    if changed > 0 && mailbox == "INBOX" {
        crate::segments::notify_mail_flags_changed(&state.ws, target);
    }
    Ok(changed)
}

// ── expunge ───────────────────────────────────────────────────────

fn expunge_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.expunge").await?;
            let req: ExpungeRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // Guardian-hold gate (`family-safety.md` § The mail gate): a live
            // hold survives the ward's expunge. Skipped, not refused — plain
            // EXPUNGE and CLOSE must keep working for the ward's own mail;
            // the held message simply stays (and is released to INBOX or
            // discarded by the guardian, never by the ward's MUA).
            let held = if req.mailbox == GUARDIAN_HELD_MAILBOX {
                state
                    .db
                    .list_held_uids(&target, &req.uids)
                    .await
                    .map_err(internal)?
            } else {
                Vec::new()
            };

            let now = now_epoch_secs();
            let outcome = state
                .db
                .apply_expunge(&target, &req.mailbox, &req.uids, &held, now)
                .await
                .map_err(internal)?;

            // Spec § D6 (ε): placement-journal append. SQLite committed
            // inside `apply_expunge`; the placement append below happens
            // after that commit. Crash window deferred to Plan 2 T9.
            //
            // Skip when nothing was expunged so we don't litter the
            // journal with no-op records (the manifest's tombstone list
            // would otherwise grow even though nothing was deleted).
            if !outcome.expunged_uids.is_empty() {
                let record = MailPlacementRecord::Expunge {
                    mailbox: req.mailbox.clone(),
                    uid_set: outcome.expunged_uids.clone(),
                    modseq: outcome.highestmodseq as u64,
                    // v2: the delete time that makes the tombstone
                    // retention prune below possible (imap-server.md §
                    // Tombstone retention).
                    deleted_at: now,
                };
                state
                    .mail_placement
                    .append_event(&target, &record)
                    .await
                    .map_err(placement_journal_diverged)?;
                // Age out tombstones deleted long ago, below the same
                // effective retention window the CalDAV/CardDAV siblings'
                // S6.8d2 `prune_tombstones` wiring uses. Piggybacked on
                // EXPUNGE because it is the operation that grows the
                // tombstone set (imap-server.md § Tombstone retention).
                let retention_days = i64::from(
                    state
                        .db
                        .get_imap_policy()
                        .await
                        .map_err(internal)?
                        .effective()
                        .tombstone_retention_days
                        .max(7),
                );
                let pruned = state
                    .mail_placement
                    .prune_tombstones(&target, now - retention_days * 86_400)
                    .await
                    .map_err(internal)?;
                if pruned > 0 {
                    tracing::debug!(
                        target: "nest_metrics",
                        metric = "bridge_imap_tombstones_total",
                        pruned,
                        "IMAP placement tombstones pruned"
                    );
                }
            }

            // I5 Phase F.1 — emit per-UID `Expunge` push to any IDLE/NOTIFY
            // subscribers. Fires after the SQLite DELETE has committed
            // (above). Per-UID is the only correct shape because claim
            // 4 in TODO § Load-bearing claims requires the MDA to emit
            // `* <seq> EXPUNGE` per UID (no `* VANISHED` upstream
            // support) — the wire-side translation needs one push per
            // UID so the per-seq EXPUNGE renumbering math is correct.
            // All expunged UIDs share the same post-commit
            // `highestmodseq`.
            for uid in &outcome.expunged_uids {
                emit_mailbox_state_event(
                    &state,
                    &target,
                    &req.mailbox,
                    MailboxStateEvent::Expunge {
                        uid: *uid,
                        modseq: outcome.highestmodseq,
                    },
                );
            }

            encode_reply(&ExpungeReply {
                expunged_uids: outcome.expunged_uids,
                highestmodseq: outcome.highestmodseq,
            })
        })
    })
}

// ── copy_messages ─────────────────────────────────────────────────

fn copy_messages_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.copy").await?;
            let req: CopyMessagesRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            if req.uids.is_empty() {
                return Err(malformed("uids must be non-empty"));
            }

            // RFC 9208 quota pre-check (imap-server.md § Quota enforcement
            // points): each copied placement re-counts its content bytes
            // against the actor's `user/<handle>` quota root. Over-quota →
            // typed error the MDA maps to `NO [OVERQUOTA]`. Checked before
            // `apply_copy` so a rejected COPY leaves the mailbox untouched.
            let refs = state
                .db
                .list_bridge_imap_uid_size_refs(&target, &req.source_mailbox, &req.uids)
                .await
                .map_err(internal)?;
            let added_count = refs.len() as u32;
            let added_bytes = sum_record_sizes(&state, &target, &refs).await?;
            enforce_imap_storage_quota(&state, &target, added_bytes, added_count).await?;

            let outcome = state
                .db
                .apply_copy(&target, &req.source_mailbox, &req.uids, &req.dest_mailbox)
                .await
                .map_err(internal)?;

            // Spec § D6 (ε): placement-journal append. The SQLite INSERTs
            // committed inside `apply_copy` above (under a single
            // `conn.lock()` scope so the per-row INSERTs and the single
            // dest state-row bump are mutually consistent). The placement
            // append below happens after that commit; the crash window
            // between the two is closed by Plan 2 T9's divergence
            // detection at SELECT / QRESYNC time.
            //
            // Per spec § D2, `Copy` does NOT bump the source mailbox's
            // modseq (the source rows are untouched, per IMAP COPY
            // semantics). Only `modseq_dst` is recorded.
            //
            // Skip when nothing was copied (every source UID was missing,
            // per `apply_copy`'s silent-skip behaviour) so we don't
            // litter the journal with no-op records.
            if !outcome.copied.is_empty() {
                let (src_uid_set, dst_uid_set): (Vec<u32>, Vec<u32>) =
                    outcome.copied.iter().copied().unzip();
                let record = MailPlacementRecord::Copy {
                    src_mailbox: req.source_mailbox.clone(),
                    src_uid_set,
                    dst_mailbox: req.dest_mailbox.clone(),
                    dst_uid_set,
                    modseq_dst: outcome.dest_highestmodseq as u64,
                };
                state
                    .mail_placement
                    .append_event(&target, &record)
                    .await
                    .map_err(placement_journal_diverged)?;
            }

            // I5 Phase F.1 — emit one `Append` push per dest UID to any
            // IDLE/NOTIFY subscribers on the destination mailbox. COPY
            // never modifies the source side (IMAP-COPY semantics: src
            // rows untouched, no modseq bump), so we don't emit on
            // `source_mailbox`.
            //
            // Flags are intentionally empty here: `apply_copy` doesn't
            // return per-UID flag strings, and the MDA's IDLE handler
            // (F.2) issues a follow-up `fetch_message_metadata` for
            // the FETCH-response detail (UID + MODSEQ on the wire are
            // sufficient under RFC 9051 §7.4.2; FLAGS is optional in
            // the unsolicited FETCH). When F.2 needs flags inline for
            // a NOTIFY-with-FETCH client request, this push payload is
            // additive: extend `MailboxStateEvent::Append`'s flags
            // field (Phase F.4 drift fix territory).
            for (_src_uid, dst_uid) in &outcome.copied {
                emit_mailbox_state_event(
                    &state,
                    &target,
                    &req.dest_mailbox,
                    MailboxStateEvent::Append {
                        uid: *dst_uid,
                        flags: Vec::new(),
                        modseq: outcome.dest_highestmodseq,
                    },
                );
            }

            let copied: Vec<CopyPair> = outcome
                .copied
                .into_iter()
                .map(|(source_uid, dest_uid)| CopyPair {
                    source_uid,
                    dest_uid,
                })
                .collect();
            encode_reply(&CopyMessagesReply {
                dest_uid_validity: outcome.dest_uid_validity,
                copied,
                dest_highestmodseq: outcome.dest_highestmodseq,
            })
        })
    })
}

// ── move_messages ─────────────────────────────────────────────────

fn move_messages_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.move").await?;
            let req: MoveMessagesRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            if req.uids.is_empty() {
                return Err(malformed("uids must be non-empty"));
            }

            // Guardian-hold gate (`family-safety.md` § The mail gate): a
            // message with a live `guardian_mail_holds` sidecar row must not
            // leave the held mailbox over IMAP — only the guardian's release/
            // discard (server-side, `family_handlers`) relocates it. MOVE is
            // atomic (RFC 6851), so one held UID refuses the whole command
            // rather than moving a partial set. Keyed on the sidecar, not the
            // mailbox: a non-held message parked here moves freely.
            if req.source_mailbox == GUARDIAN_HELD_MAILBOX {
                let held = state
                    .db
                    .list_held_uids(&target, &req.uids)
                    .await
                    .map_err(internal)?;
                if !held.is_empty() {
                    return Err(held_for_review());
                }
            }

            // RFC 9208 quota pre-check (imap-server.md § Quota enforcement
            // points): MOVE = COPY-then-EXPUNGE, and the doc applies the same
            // pre-check as COPY against the destination — the copy half adds
            // placements that re-count their bytes. Conservative by design:
            // the source rows are still counted at pre-check time (they are
            // tombstoned only inside `apply_move`'s transaction), so a
            // near-cap same-root reshuffle can be rejected; the transaction
            // never starts and the source stays, exactly as the doc specifies
            // ("Over-quota on dst_mailbox → NO [OVERQUOTA] and the source rows
            // stay").
            let refs = state
                .db
                .list_bridge_imap_uid_size_refs(&target, &req.source_mailbox, &req.uids)
                .await
                .map_err(internal)?;
            let added_count = refs.len() as u32;
            let added_bytes = sum_record_sizes(&state, &target, &refs).await?;
            enforce_imap_storage_quota(&state, &target, added_bytes, added_count).await?;

            let outcome = state
                .db
                .apply_move(&target, &req.source_mailbox, &req.uids, &req.dest_mailbox)
                .await
                .map_err(internal)?;

            // Spec § D6 (ε): placement-journal append. The SQLite copy +
            // expunge committed inside `apply_move` above (under a
            // single `conn.lock()` scope, so the dest INSERTs, source
            // expunge-log INSERTs, source DELETEs, and both state-row
            // bumps are mutually consistent). The placement append below
            // happens after that commit; the crash window between the
            // two is closed by Plan 2 T9's divergence detection at
            // SELECT / QRESYNC time.
            //
            // Per spec § D2, `Move` records both modseqs: `modseq_src`
            // for the source-side tombstones the manifest synthesises,
            // and `modseq_dst` for the freshly-allocated destination
            // placements.
            //
            // Skip when nothing was moved (every source UID was missing,
            // per `apply_move`'s silent-skip behaviour) so we don't
            // litter the journal with no-op records.
            if !outcome.moved.is_empty() {
                let (src_uid_set, dst_uid_set): (Vec<u32>, Vec<u32>) =
                    outcome.moved.iter().copied().unzip();
                let record = MailPlacementRecord::Move {
                    src_mailbox: req.source_mailbox.clone(),
                    src_uid_set,
                    dst_mailbox: req.dest_mailbox.clone(),
                    dst_uid_set,
                    modseq_src: outcome.source_highestmodseq as u64,
                    modseq_dst: outcome.dest_highestmodseq as u64,
                    deleted_at: outcome.moved_at,
                };
                state
                    .mail_placement
                    .append_event(&target, &record)
                    .await
                    .map_err(placement_journal_diverged)?;
            }

            // Emit one `Move` push per UID pair to subscribers on BOTH
            // the source AND the destination mailbox, each naming its
            // side (`imap-server.md` § Push wiring): the MDA writes the
            // disappearance on the source's IDLE channel and
            // `* EXISTS` + FETCH on the destination's. IMAP UIDs are
            // per-mailbox, so the side travels on the wire — the MDA
            // cannot recover it from the UIDs.
            for (src_uid, dst_uid) in &outcome.moved {
                let event = |side| MailboxStateEvent::Move {
                    src_uid: *src_uid,
                    dst_uid: *dst_uid,
                    modseq_src: outcome.source_highestmodseq,
                    modseq_dst: outcome.dest_highestmodseq,
                    side,
                };
                emit_mailbox_state_event(
                    &state,
                    &target,
                    &req.source_mailbox,
                    event(MoveSide::Source),
                );
                emit_mailbox_state_event(
                    &state,
                    &target,
                    &req.dest_mailbox,
                    event(MoveSide::Destination),
                );
            }

            let moved: Vec<CopyPair> = outcome
                .moved
                .into_iter()
                .map(|(source_uid, dest_uid)| CopyPair {
                    source_uid,
                    dest_uid,
                })
                .collect();
            encode_reply(&MoveMessagesReply {
                dest_uid_validity: outcome.dest_uid_validity,
                moved,
                source_highestmodseq: outcome.source_highestmodseq,
                dest_highestmodseq: outcome.dest_highestmodseq,
            })
        })
    })
}

// ── append_message ────────────────────────────────────────────────

fn append_message_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.append").await?;
            let mut req: AppendMessageRequest = decode(&payload).map_err(malformed)?;

            // Parse target actor_id.
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // Message-size ceiling (`smtp-server.md` § Message size limits). Since
            // ceiling retirement the product ceiling is `max_message_bytes` alone,
            // and — unlike the SMTP legs — IMAP APPEND has no perimeter clamp of
            // its own, so nest is its authoritative enforcement point (the bridge
            // is untrusted), mirroring `import_one`. The handler sees only the
            // sealed body, so it bounds the declared `ciphertext_size` at the
            // product ceiling plus the seal-envelope allowance — admitting any raw
            // message within the ceiling whose seal grew it by at most that much.
            // Checked on the *declared* size before resolving a `body_ref`, so an
            // over-ceiling APPEND never forces nest to read the staged body; the
            // `ciphertext_size == body_bytes.len()` check below keeps the
            // declaration honest. Over → the shared typed `message_too_large` the
            // MDA maps to an IMAP `BAD`.
            let max_raw = fauna_mail::transport_limits::effective_max_raw_message_bytes(
                state
                    .db
                    .get_spam_policy()
                    .await
                    .map_err(internal)?
                    .effective()
                    .max_message_bytes,
            );
            let sealed_ceiling =
                max_raw as u64 + fauna_mail::transport_limits::SEAL_ENVELOPE_ALLOWANCE_BYTES as u64;
            if req.ciphertext_size as u64 > sealed_ceiling {
                return Err(message_too_large(
                    req.ciphertext_size as usize,
                    sealed_ceiling,
                ));
            }

            // The sealed body either rode inline, or — being over the inline
            // budget — crossed on the bulk-byte plane and is named here by
            // reference (`smtp-server.md` § Message size limits — the MDA-APPEND
            // upward leg). The APPEND stages *already-sealed* bytes, so it rides a
            // plain `MailBodyRef`, never the plaintext staged-envelope. Resolving
            // the reference yields the identical byte string the MDA sealed, which
            // then takes exactly the inline path below: same seal gate, same
            // `append_record`, same quota charge. Only the transport differed.
            //
            // Exactly one of the two must be present. Rejecting "neither" is what
            // stops a version-skewed MDA — a new MDA staging a reference at an
            // older nest that drops the unknown key — from ever storing an *empty*
            // message: the failure surfaces as a typed error the MUA is told
            // about, not as silent mail loss. (Mirrors
            // `persist_inbound_mail_request` exactly.)
            let body_bytes = match req.body_ref.take() {
                Some(r) => {
                    if !req.encrypted_body.is_empty() {
                        return Err(malformed(
                            "encrypted_body must be empty when body_ref is set",
                        ));
                    }
                    // The declared `ciphertext_size` was checked above, but the
                    // reference's own `total_bytes` and chunk list are separate
                    // claims — bounded here, before any chunk is read.
                    crate::mail_body_plane::resolve_body_ref(&state, &r, sealed_ceiling).await?
                }
                None => {
                    if req.encrypted_body.is_empty() {
                        return Err(malformed(
                            "APPEND carries neither an inline encrypted_body nor a body_ref",
                        ));
                    }
                    std::mem::take(&mut req.encrypted_body)
                }
            };
            if req.encrypted_index_hint.is_empty() {
                return Err(malformed("encrypted_index_hint must not be empty"));
            }
            // `ciphertext_size` is the sealed body length whichever way it arrived
            // — the MDA captures it pre-staging, so it stays the true sealed size
            // when the body leaves the request.
            if req.ciphertext_size as usize != body_bytes.len() {
                return Err(malformed(format!(
                    "ciphertext_size mismatch: metadata={} body_bytes={}",
                    req.ciphertext_size,
                    body_bytes.len(),
                )));
            }
            // Validate: \Recent cannot be appended (RFC 3501 §2.3.2).
            if req.flags.iter().any(|f| f == "\\Recent") {
                return Err(malformed("\\Recent cannot be appended"));
            }
            // There is no absent key (`mailbox-migration.md` § The envelope key
            // confirms a Message-ID hit): refused before anything is stored.
            fauna_mail::require_dedup_pair(&req.dedup_key, &req.envelope_key).map_err(malformed)?;

            // RFC 9208 quota pre-check (imap-server.md § Quota enforcement
            // points): APPEND adds one message of `ciphertext_size` bytes to
            // the actor's `user/<handle>` quota root. Over-quota → typed
            // error the MDA maps to `NO [OVERQUOTA] Mailbox quota exceeded`.
            // Checked before any DB write so a rejected APPEND is a no-op.
            enforce_imap_storage_quota(&state, &target, req.ciphertext_size as u64, 1).await?;

            // Build InboundMailFields for storage. APPEND is a client upload,
            // not an SMTP delivery — there is no envelope to verify, so the
            // auth verdicts are all "none" and the message is never spam-scored
            // (disposition "accept"). is_own_submission=true since the message
            // came from the user's own MUA via the MDA bridge.
            // S6.12b structural seal gate: the MDA seals both halves before
            // the APPEND RPC (imap/append.go), unconditionally, both modes —
            // prove it at the wire edge rather than trust the caller class.
            let sealed_body = fauna_mls::wrapped_blob::SealedRecordBytes::verify(body_bytes)
                .map_err(|_| malformed("encrypted_body is not a sealed recipient envelope"))?;
            let sealed_hint = fauna_mls::wrapped_blob::SealedRecordBytes::verify(
                req.encrypted_index_hint.clone(),
            )
            .map_err(|_| malformed("encrypted_index_hint is not a sealed recipient envelope"))?;
            let fields = InboundMailFields {
                actor_id: target,
                timestamp: req.timestamp,
                ciphertext_size: req.ciphertext_size,
                encrypted_body: sealed_body,
                encrypted_index_hint: sealed_hint,
                sender_domain: req.sender_domain.clone(),
                spf: "none".into(),
                dkim: "none".into(),
                dmarc: "none".into(),
                dmarc_policy: "none".into(),
                arc: "none".into(),
                spam_score: 0,
                spam_disposition: "accept".into(),
                is_own_submission: true,
                // Client upload — nothing scored it, so no bus rows (same
                // rationale as the never-spam-scored disposition above).
                scores: vec![],
                // No perimeter pass either — an APPENDed message carries no
                // report-hash and cannot aggregate (report-sharing.md
                // § Content identity).
                report_hash: vec![],
            };

            let insert_outcome = state
                .db
                .insert_appended_mail(&state.mail_segments, &fields)
                .await
                .map_err(internal)?;
            let message_id = insert_outcome.message_id;
            let inserted = insert_outcome.inserted;
            // Plan 5 T6: emit `fauna.segments.changed { Finalized }` if
            // this APPEND rotated a previously-open segment closed.
            // Same wake path as the inbound-mail handler — the data
            // owner's client treats either source identically.
            if let Some(closed_seg_id) = insert_outcome.finalized {
                crate::segments::notify_segments_changed(
                    &state.ws,
                    &target,
                    "mail",
                    closed_seg_id,
                    fauna_protocol::push_events::SegmentChange::Finalized,
                );
            }

            // Ensure standard mailboxes exist for the target actor.
            // Bootstrap Create records emit BEFORE the Append below so a
            // fresh actor's first APPEND finds the seeded mailboxes in
            // the manifest by the time the Append record applies (spec
            // § D2 record-table ordering invariant).
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&target)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &target, newly_seeded).await?;

            // Build canonical (sorted, \Recent-filtered) initial flags string.
            let flags_string: String = {
                use std::collections::BTreeSet;
                let set: BTreeSet<&str> = req
                    .flags
                    .iter()
                    .map(String::as_str)
                    .filter(|&f| f != "\\Recent")
                    .collect();
                set.into_iter().collect::<Vec<_>>().join(" ")
            };

            // Place (or recover the existing placement uid).  `sender_domain`
            // populates `from_norm` for the SEARCH header axis; APPEND
            // bodies are sealed so this is the only header substring nest
            // can index (subject/to/cc remain empty per the C.7 encrypted-
            // mode degradation in imap-server.md § SEARCH).
            //
            // `placement_modseq` is `Some(modseq)` when a fresh placement
            // row was created; `None` when an existing placement was
            // returned (idempotent retry of the same APPEND). We only
            // emit a placement event in the fresh case — re-emitting on
            // an idempotent retry would duplicate the journal entry.
            let (uid, placement_modseq) = state
                .db
                .place_or_get_existing_placement(
                    &target,
                    &message_id,
                    &req.mailbox,
                    req.timestamp,
                    &flags_string,
                    &req.sender_domain,
                    inserted,
                )
                .await
                .map_err(internal)?;

            // Look up uid_validity for the destination mailbox (guaranteed to
            // exist because place_or_get_existing_placement ensures the state row).
            let state_row = state
                .db
                .get_bridge_imap_mailbox_state(&target, &req.mailbox)
                .await
                .map_err(internal)?
                .ok_or_else(|| internal("mailbox state row missing after placement"))?;

            // Spec § D6 (ε): placement-journal append. The SQLite INSERT
            // committed inside `place_or_get_existing_placement` above
            // (it holds the conn-mutex around the ensure_mailbox_state +
            // allocate_uid + INSERT sequence). The placement append below
            // happens after that commit; the crash window between the
            // two is closed by Plan 2 T9's divergence detection at
            // SELECT / QRESYNC time.
            if let Some(modseq) = placement_modseq {
                let record = MailPlacementRecord::Append {
                    mailbox: req.mailbox.clone(),
                    uid,
                    modseq: modseq as u64,
                    flags: split_flags(&flags_string),
                    content_record_id: message_id.to_vec(),
                    internal_date: req.timestamp,
                };
                state
                    .mail_placement
                    .append_event(&target, &record)
                    .await
                    .map_err(placement_journal_diverged)?;

                // I5 Phase F.1 — emit `Append` push to IDLE/NOTIFY
                // subscribers on this mailbox. Only fires when a fresh
                // placement row was created (`placement_modseq.is_some()`);
                // on the idempotent-retry branch (placement already
                // present, `place_or_get_existing_placement` returned
                // None for the modseq) we'd duplicate-emit the journal
                // entry and the push — guarded together. Fires AFTER
                // the placement-journal append commits.
                emit_mailbox_state_event(
                    &state,
                    &target,
                    &req.mailbox,
                    MailboxStateEvent::Append {
                        uid,
                        flags: split_flags(&flags_string),
                        modseq,
                    },
                );
            }

            // `mailbox-migration.md` § Dedup key persistence: every regular
            // APPEND populates the index with the (dedup, envelope) key pair,
            // so a later import can dedup against mail the user's MUA filed
            // here. Record-only — an APPEND is never skipped on a hit (that is
            // the import path's contract alone), and `insert_dedup_key` is
            // INSERT OR IGNORE, so the first writer's row stands on an
            // idempotent APPEND retry.
            state
                .db
                .insert_dedup_key(
                    &target,
                    &req.dedup_key,
                    &req.envelope_key,
                    &hex::encode(message_id),
                )
                .await
                .map_err(internal)?;

            encode_reply(&AppendMessageReply {
                message_id: message_id.to_vec(),
                uid,
                uid_validity: state_row.uid_validity,
            })
        })
    })
}

// ── search_messages ───────────────────────────────────────────────

/// Translate a wire `SearchTerm` into the DB-internal mirror. Header
/// `value` is case-folded the same way `norm_for_storage` folds the
/// column on write so `instr(column, value)` is case-insensitive.
fn protocol_term_to_db(term: SearchTerm) -> SearchTermDb {
    match term {
        SearchTerm::HasFlag { flag } => SearchTermDb::HasFlag(flag),
        SearchTerm::LacksFlag { flag } => SearchTermDb::LacksFlag(flag),
        SearchTerm::HeaderContains { field, value } => SearchTermDb::HeaderContains(
            match field {
                HeaderField::From => SearchHeaderFieldDb::From,
                HeaderField::To => SearchHeaderFieldDb::To,
                HeaderField::Cc => SearchHeaderFieldDb::Cc,
                HeaderField::Subject => SearchHeaderFieldDb::Subject,
            },
            value.to_ascii_lowercase(),
        ),
        SearchTerm::SinceInternalDate { ts } => SearchTermDb::SinceInternalDate(ts),
        SearchTerm::BeforeInternalDate { ts } => SearchTermDb::BeforeInternalDate(ts),
        SearchTerm::Larger { size } => SearchTermDb::Larger(size),
        SearchTerm::Smaller { size } => SearchTermDb::Smaller(size),
    }
}

fn search_messages_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.search_messages").await?;
            let req: SearchMessagesRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            // Translate wire terms → DB-internal terms. Folds header
            // values case-insensitively.
            let db_terms: Vec<SearchTermDb> =
                req.terms.into_iter().map(protocol_term_to_db).collect();
            // Non-size axes run in SQL; each hit carries (segment_id,
            // record_cid) so size axes (LARGER/SMALLER) can be applied
            // post-query against the CARv2 index by record_cid — record block
            // length is not a SQL column (imap-server.md § SEARCH). The Db
            // method ignores the size terms; we filter them here.
            let hits = state
                .db
                .search_bridge_imap_messages(&target, &req.mailbox, &db_terms)
                .await
                .map_err(internal)?;
            let size_terms: Vec<&SearchTermDb> = db_terms
                .iter()
                .filter(|t| matches!(t, SearchTermDb::Larger(_) | SearchTermDb::Smaller(_)))
                .collect();
            let uids: Vec<u32> = if size_terms.is_empty() {
                hits.into_iter().map(|h| h.uid).collect()
            } else {
                let refs: Vec<(u32, fauna_cbor::Cid)> =
                    hits.iter().map(|h| (h.segment_id, h.record_cid)).collect();
                let sizes = crate::segments::record_sizes(&state.mail_segments, &target, &refs)
                    .await
                    .map_err(internal)?;
                // Hits stay in ascending-uid order (SQL ORDER BY uid); the
                // post-filter preserves it. A divergent record with no readable
                // segment reports size 0 — it can satisfy SMALLER, never LARGER,
                // identical to a genuine 0-byte record.
                hits.iter()
                    .zip(sizes)
                    .filter_map(|(h, size)| {
                        let sz = size.unwrap_or(0);
                        let pass = size_terms.iter().all(|t| match t {
                            SearchTermDb::Larger(n) => sz > *n as u64,
                            SearchTermDb::Smaller(n) => sz < *n as u64,
                            _ => true,
                        });
                        pass.then_some(h.uid)
                    })
                    .collect()
            };
            encode_reply(&SearchMessagesReply { uids })
        })
    })
}

// ── get_quota ─────────────────────────────────────────────────────

fn get_quota_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_quota").await?;
            let req: GetQuotaRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;
            let (storage_bytes_used, message_count_used) =
                imap_quota_usage(&state, &target).await?;
            // The reported limit is the *effective* deployment policy
            // (catalog ⊕ admin `put_imap_policy` override) — the same source
            // `enforce_imap_storage_quota` rejects against, so a client's
            // `GETQUOTA` limit always matches the threshold APPEND/COPY/MOVE
            // enforce. Per-actor tier override is Phase F+ (imap-server.md
            // § Resources); the single `mail_imap_policy` row is deployment-wide.
            let policy = state
                .db
                .get_imap_policy()
                .await
                .map_err(internal)?
                .effective();
            encode_reply(&GetQuotaReply {
                storage_bytes_used,
                message_count_used,
                storage_bytes_limit: policy.storage_bytes_default,
                message_count_limit: policy.message_count_default,
            })
        })
    })
}

// ── fetch_spam_model ───────────────────────────────────────────────
//
// Return the actor's per-user Bayesian model, **sealed to their MSEK-derived
// key**, so a capability holder — the MDA on an AUTH'd session, or the user's
// own client — scores inbound mail on-device at the search-equivalent position
// (`mail-spam.md` § Scoring placement, § Encrypted-mode interaction) and trains
// it (open → mutate → re-seal → `put_spam_model`).
//
// The stored model rests ONLY sealed (its one writer, `put_spam_model`,
// refuses a plaintext blob), so a stored model is returned verbatim. With no
// stored model the nest may hand out the **cold-start seed**: the published
// deployment baseline — deployment-readable aggregate data, never user data —
// folded onto a fresh model and sealed on read to the actor
// (`seal_recipient_blob`; the nest holds only the recipient's *public* half,
// so it seals *to* the actor but never reads back what it sealed). Nothing is
// persisted. The raw model never crosses the wire in the clear
// (`mail-spam.md` "Don't expose the model file's raw bytes…").

/// The cold-start seed `fetch_spam_model` seals on read for an actor with no
/// stored model (`mail-spam.md` § Cold start, Path 2 step 4): the published
/// deployment baseline folded onto a FRESH model through the shared faded fold
/// (`SpamModel::fold_baseline_faded` — at zero own samples the fade fraction is
/// `(full − 0)/full = 1`, so a fresh actor inherits the full baseline). `None`
/// when no baseline is published, it does not parse, or it carries no samples
/// (rspamd-only cold start). Read-time only: never written to `spam_models`,
/// and every train position starts from an empty model rather than from this
/// seed (`FetchSpamModelReply::stored_sealed == false`).
fn cold_start_seed(baseline: Option<Vec<u8>>, full_confidence: u32) -> Option<Vec<u8>> {
    use fauna_mail::spam::SpamModel;
    let baseline = SpamModel::from_bytes(&baseline?)?;
    let mut seed = SpamModel::new();
    seed.fold_baseline_faded(&baseline, full_confidence);
    (seed.sample_count() > 0).then(|| seed.to_bytes())
}

/// Resolve **this box's** aggregation-holder seal target for the
/// deployment-baseline contribute path (piece (b), `mail-spam.md` § Wire shapes /
/// § Encrypted-mode interaction): the nest *volunteers* its own content-processor
/// holder so an opted-in contributor can seal a `SpamModelCopyBlob` to it without
/// the Admin-gated roster (`list_service_users`) — the holder's X25519 pubkey is
/// already `User`-fetchable via `fetch_bridge_pubkey`.
///
/// Picks the first approved `ContentProcessor` service user that has attested an
/// X25519 pubkey **and is not in-process**. Rows are ordered `created_at ASC`,
/// so the choice is deterministic and stable. Returns `None` when no such holder
/// is enrolled (the client then attaches no copy, and no publish run is opened —
/// fail closed). Mirrors the existing MTA-role seal-target resolution in
/// `bridge_routing_handlers.rs` (DKIM rotation-mint) — same shape, different role.
///
/// **The in-process exclusion is load-bearing** (`BridgeServiceUser::in_process`):
/// the nest's own web-paywall holder (`web_content::holder`) self-approves a
/// `ContentProcessor` row with an attested X25519 at boot — the earliest
/// `created_at` on any blob-store deploy, so without the exclusion it always won
/// this resolution. That told opted-in contributors to seal their spam models to
/// a key whose secret rests in the nest's data dir beside the ciphertext
/// (voiding `mail-spam.md` § Encrypted-mode interaction — "aggregation runs at
/// the granted holder, never nest in-process" — and `encryption-at-rest.md`
/// § Don't do these), and bound every publish run to a holder that never dials
/// in over WS, so the sealed half never merged. An in-process holder stays a
/// legitimate grant target for its own ratified readable class (web-paywalled
/// content); it is only this nest-opaque seal-target resolution it can never win.
///
/// **This is the box's single aggregation-holder identity, and every leg of the
/// drain must resolve it through here** — the contributor copies are sealed to
/// it, the publish run is bound to it, and the publish poke goes to it alone
/// (`mail-spam.md` § Encrypted-mode interaction: aggregation runs at *the*
/// granted holder, singular). The identity is resolved over the
/// content-processor **family** by deterministic preference — a dedicated
/// off-box `ContentProcessor`-role holder when one is enrolled (always a
/// manual-approval trust decision, `mail-bridge-lifecycle.md` § Onboarding
/// auto-approval), else the off-box MDA (the family holder the 2026-07-12
/// ratification named; holder ruling 2026-08-03, `mail-spam.md`
/// § Encrypted-mode interaction). The empty-worklist
/// race is dead structurally — the run is bound
/// to the one resolved holder and only it is poked — so admitting the MDA
/// here does not reopen it; what stays iron-clad is `!in_process` (the nest's
/// own disk-resident key is never a seal target) and the attested-x25519
/// requirement. On a holder switch (a dedicated holder approved later),
/// contributor copies migrate lazily: each sealed write re-attaches a copy to
/// the then-current seal target, and stale copies surface honestly in
/// `skipped_contributors`.
pub(crate) async fn resolve_content_processor_holder(
    state: &std::sync::Arc<crate::routes::AppState>,
) -> anyhow::Result<Option<([u8; 32], HolderSealTarget)>> {
    use crate::db::bridge_service_users::BridgeRole;
    let candidates = state.db.list_approved_bridge_service_users().await?;
    let eligible = |b: &&crate::db::bridge_service_users::BridgeServiceUser| {
        !b.in_process && b.x25519_pubkey.is_some()
    };
    let holder = candidates
        .iter()
        .filter(eligible)
        .find(|b| b.role == BridgeRole::ContentProcessor)
        .or_else(|| {
            candidates
                .iter()
                .filter(eligible)
                .find(|b| b.role == BridgeRole::Mda)
        })
        .cloned();
    let Some(holder) = holder else {
        return Ok(None);
    };
    let x25519 = holder
        .x25519_pubkey
        .expect("holder was filtered on x25519_pubkey.is_some()");
    // The `BridgeServiceUser` row does not carry the ML-KEM ek — a second lookup
    // resolves it (X-Wing holders publish one; classical-only holders → `None`).
    let mlkem_ek = state.db.bridge_mlkem_ek(&holder.ed25519_pubkey).await?;
    Ok(Some((
        holder.ed25519_pubkey,
        HolderSealTarget {
            x25519_pubkey: x25519.to_vec(),
            mlkem_ek: mlkem_ek.map(ByteBuf::from),
            extra: Default::default(),
        },
    )))
}

/// The seal target a contributing client seals its holder copy to — the X25519
/// half of [`resolve_content_processor_holder`]. The client needs no roster
/// enumeration to learn it (`mail-spam.md` § Wire shapes, `holder_seal_target`).
pub(crate) async fn resolve_content_processor_holder_seal_target(
    state: &std::sync::Arc<crate::routes::AppState>,
) -> anyhow::Result<Option<HolderSealTarget>> {
    Ok(resolve_content_processor_holder(state)
        .await?
        .map(|(_holder, target)| target))
}

pub(crate) fn fetch_spam_model_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // The allowlist gates this to `BridgeMda | User | Admin`;
            // `require_class` rejects any other caller class before we touch the
            // model, and returns the caller's class for the caller-scope check.
            let class = require_class(&state, &actor_id, "fauna.bridges.fetch_spam_model").await?;
            let req: FetchSpamModelRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            //
            // A `User`/`Admin` caller may fetch only their OWN model
            // (`target == caller`) — even an admin cannot read another user's
            // model (`mail-spam.md` § Cross-actor isolation). `BridgeMda` keeps
            // trusted-naming: it serves an actor whose AUTH'd session it didn't
            // authenticate as (the same model as `put_spam_model`), bounded by
            // `require_local_mail_serving` + the bridge approval gate.
            // Without this, any User could probe `fetch_spam_model(target = any
            // local-mail actor)` — a cross-user trained-state / mail-serving
            // existence oracle.
            if !matches!(class, CallerClass::BridgeMda) && target != actor_id {
                return Err(permission_denied("may fetch only your own spam model"));
            }
            require_local_mail_serving(&state, &target).await?;

            // The actor's stored model — sealed at rest, or absent.
            let own = state.db.get_spam_model(&target).await.map_err(internal)?;

            // Deployment-baseline write signal (piece (b), `mail-spam.md` § Wire
            // shapes): whether the target opted in
            // (`spam_preferences.contribute_baseline`) and — only then — the box's
            // volunteered content-processor holder seal target, so the writing
            // agent (client or MDA) seals a `SpamModelCopyBlob` to it on its next
            // write. Gated on the opt-in bit so the holder lookup stays off the
            // hot MDA-scoring path for the common (non-contributing) caller; the
            // holder pubkey is public regardless (already `User`-fetchable via
            // `fetch_bridge_pubkey`), so gating is a cost choice, not a
            // confidentiality one.
            let contribute_baseline = state
                .db
                .get_spam_preferences(&target)
                .await
                .map_err(internal)?
                .contribute_baseline;
            let holder_seal_target = if contribute_baseline {
                resolve_content_processor_holder_seal_target(&state)
                    .await
                    .map_err(internal)?
            } else {
                None
            };

            if let Some((bytes, ..)) = own {
                // A stored model is always sealed — `put_spam_model` is its one
                // writer and refuses anything else. A blob that is NOT sealed
                // cannot have been written by this binary; fail closed rather
                // than decode it or hand it out.
                if !crate::spam_model_seal::is_sealed_model_blob(&bytes) {
                    tracing::error!("fetch_spam_model: a stored spam model is not sealed");
                    return Err(internal("stored spam model is not sealed"));
                }
                // The nest cannot fold the published deployment baseline into a
                // sealed model (no read), so the fold moves to the agent: attach
                // the plaintext aggregate and the agent applies the faded
                // `merge_scaled` locally against its own true `sample_count`
                // (the no-double-fold rule — the field is present exactly when
                // no server-side fold happened; `mail-spam.md` § Encrypted-mode
                // interaction, ratified 2026-07-12). A withdrawn/empty or
                // unparseable baseline seeds nothing and is omitted. Never
                // withheld on the untrusted advisory sample_count — the agent's
                // fade contributes zero at full confidence anyway.
                let baseline = state
                    .db
                    .get_spam_baseline()
                    .await
                    .map_err(internal)?
                    .filter(|b| {
                        fauna_mail::spam::SpamModel::from_bytes(b)
                            .is_some_and(|m| m.sample_count() > 0)
                    });
                return encode_reply(&FetchSpamModelReply {
                    blob: Some(ByteBuf::from(bytes)),
                    // The blob is the STORED model: train on it.
                    stored_sealed: true,
                    baseline: baseline.map(ByteBuf::from),
                    contribute_baseline,
                    holder_seal_target,
                    extra: Default::default(),
                });
            }

            // No stored model: the cold-start seed, if a baseline is published.
            // The fade horizon is the admin-effective Tier-2
            // `mail.spam.bayesian_full_confidence_samples` (default 200) — the
            // same knob the off-nest scorer reads via `fetch_config`, so the
            // fade compensates the confidence ramp identically at every
            // position (`mail-spam.md` § Combined-score formula).
            let baseline = state.db.get_spam_baseline().await.map_err(internal)?;
            let full_confidence = state
                .db
                .get_spam_policy()
                .await
                .map_err(internal)?
                .effective()
                .bayesian_full_confidence_samples;
            let blob = match cold_start_seed(baseline, full_confidence) {
                Some(seed) => {
                    // Seal-on-read to the actor's MSEK-derived pubkey — the same
                    // seal `seal_and_persist_local` applies to the body + index
                    // hint, resolved through the single Phase-3 D2 seam. Hybrid
                    // X-Wing, as every seal to a recipient's standing key (the
                    // helper degrades to classical on a seal *error*, never
                    // failing closed).
                    let seal_key = state
                        .db
                        .get_recipient_seal_key(&target)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| invalid_params("actor has no encryption key on file"))?;
                    let sealed = seal_recipient_blob(
                        &seed,
                        &seal_key.mls_pubkey,
                        Some(seal_key.mlkem_ek.as_slice()),
                        "spam-model",
                    )?;
                    Some(ByteBuf::from(sealed))
                }
                // No model and no baseline ⇒ cold start (the scorer's weight is
                // 0 below `bayesian_min_samples`; the agent scores rspamd-only
                // until it trains or a baseline is published).
                None => None,
            };
            encode_reply(&FetchSpamModelReply {
                blob,
                // The blob (if any) is the read-time seed, never the actor's
                // model: every train position starts from an empty model.
                stored_sealed: false,
                // The seed already carries the fold (no-double-fold rule: the
                // field is present exactly when the nest did NOT fold).
                baseline: None,
                contribute_baseline,
                holder_seal_target,
                extra: Default::default(),
            })
        })
    })
}

/// The mail half of the report capture (`report-sharing.md` § Report capture):
/// resolve `message_id`'s stored canonical report-hash and emit
/// (`is_spam_flag`) or withdraw the reporter's `report:spam` row. A message with
/// no stored hash (an APPEND stores none) cannot aggregate and captures nothing.
/// Log-and-continue: a capture failure never fails the training write it
/// rides, and a successful capture nudges the federation exchange
/// originator's debounced push (the local aggregate may have transitioned).
async fn capture_mail_report(
    state: &std::sync::Arc<crate::routes::AppState>,
    reporter: &[u8; 32],
    message_id: &[u8],
    is_spam_flag: bool,
) {
    let Ok(message_id) = <[u8; 32]>::try_from(message_id) else {
        return;
    };
    match state.db.report_hash_for_message(&message_id).await {
        Ok(Some(hash)) => {
            let report_key = crate::db::reports::ReportKey {
                content_hash: hash,
                factor: fauna_core::scoring::factor::REPORT_SPAM.to_string(),
                content_kind: "mail".to_string(),
            };
            match crate::db::reports::capture_report(&state.db, reporter, &report_key, is_spam_flag)
                .await
            {
                Ok(()) => state.notify_exchange_transition(),
                Err(e) => tracing::warn!("report capture failed (the write succeeded): {e}"),
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("report-hash lookup failed (the write succeeded): {e}"),
    }
}

/// `fauna.bridges.put_spam_model` — the **opaque sealed-write-back** twin of
/// `fetch_spam_model` (leg 3 of the tier-1 spam-model client-write end-game,
/// `mail-spam.md` § Wire shapes / § Encrypted-mode interaction). The client
/// holds its OWN key: it unwraps its sealed model under the session MLS
/// capability, applies the training delta locally, **re-seals the whole model**,
/// and writes the sealed bytes back here. nest stores them **opaque** — no
/// decode, no merge, no count-from-blob — the ratified end-game shape
/// (`mail-spam.md:355`: "re-seals the whole model, writes it back — nest stores
/// the sealed blob opaque, never merging … no server-side merge").
///
/// **Two caller paths (leg 2 widened the allowlist to `BridgeMda`):**
/// - **`User`/`Admin` — caller-scoped by construction.** `req.actor_id` is
///   ignored; the connection's authenticated actor IS the subject (exactly like
///   `set_baseline_contribution`), so a `User`/`Admin` overwrites only their own
///   `spam_models` row and there is no cross-actor write surface (§ Cross-actor
///   isolation).
/// - **`BridgeMda` — trusted naming (the MDA agent-side `\Junk`-train re-seal).**
///   The MDA opens the served actor's sealed model under its session MLS
///   capability (or starts from an empty model), applies the training delta,
///   re-seals, and writes it back naming the served actor in `req.actor_id` —
///   the same trusted-naming model as `fetch_spam_model`, bounded by
///   `require_local_mail_serving`. This keeps ONE write RPC (priority #1/#3), the
///   opaque-write twin of the opaque-read `fetch_spam_model`.
///
/// It is the ONE per-user model writer — the nest never trains, undoes or
/// merges a model itself — so it refuses what would put plaintext at rest: a
/// `sealed_model` that decodes as a plaintext `SpamModel`, and a history
/// `Insert` whose `sealed_subject` is empty or whose `sealed_delta` is empty or
/// decodes as a plaintext n-gram set (`invalid_params`). After the write
/// commits it runs the mail report capture (`report-sharing.md` § Report
/// capture) for the lesson the history op added or removed.
pub(crate) fn put_spam_model_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // The allowlist gates this to `BridgeMda | User | Admin`;
            // `require_class` rejects any other caller class before we touch the
            // model, and returns the caller's class for the target resolution.
            let class = require_class(&state, &actor_id, "fauna.bridges.put_spam_model").await?;
            let req: PutSpamModelRequest = decode(&payload).map_err(malformed)?;
            // Resolve the write target. `BridgeMda` (the MDA agent-side re-seal)
            // names the served actor via `req.actor_id` — trusted-naming, the
            // same model as `fetch_spam_model_handler` (bounded by
            // `require_local_mail_serving` below). `User`/`Admin` stay
            // caller-scoped: `req.actor_id` must be empty (the pre-leg-2 client
            // shape) or name the caller; naming anyone else is REJECTED, never
            // silently written to self — the mirror (the read
            // twin rejects a mismatched target the same way), and fail-loud: a
            // buggy privileged client that names another actor must get an error,
            // not an `ok` that silently overwrote its OWN model (§ Cross-actor
            // isolation holds either way; even an admin writes only their own).
            let target: [u8; 32] = if matches!(class, CallerClass::BridgeMda) {
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?
            } else {
                if !req.actor_id.is_empty() && req.actor_id.as_slice() != actor_id {
                    return Err(permission_denied("may write only your own spam model"));
                }
                actor_id
            };
            // Bound the write to an actor with local mail serving enabled — the
            // same gate `fetch_spam_model_handler` applies (a model has no consumer
            // without mail serving), and the trust bound for the `BridgeMda`
            // trusted-naming path (symmetrically with the read twin).
            require_local_mail_serving(&state, &target).await?;
            // A valid re-sealed model is a non-empty inner `wrapped_blob`; reject
            // an empty write so it can't silently blank the model (a client that
            // means to clear its model uses `reset_spam_model`). The upper bound
            // is the inbound 2 MiB WS-frame cap (`MAX_RPC_WS_MESSAGE_SIZE`): the
            // blob is opaque, so the nest can neither decode nor `cap_to_bytes`
            // it; the writer caps the model before sealing
            // (`apply_model_write_op`).
            if req.sealed_model.is_empty() {
                return Err(malformed("sealed_model must not be empty"));
            }
            // The model rests ONLY sealed: a blob that decodes as a plaintext
            // `SpamModel` is refused, never stored (`mail-spam.md`
            // § Encrypted-mode interaction).
            if !crate::spam_model_seal::is_sealed_model_blob(&req.sealed_model) {
                return Err(invalid_params(
                    "sealed_model must be sealed; a plaintext model is never stored",
                ));
            }
            // A history row rests sealed too: its subject and its delta are
            // sealed to the actor's own key, so neither may be empty and the
            // delta may not be the plaintext n-gram set.
            if let Some(SpamHistoryOp::Insert {
                sealed_subject,
                sealed_delta,
                ..
            }) = &req.history_op
            {
                if sealed_subject.is_empty() {
                    return Err(invalid_params(
                        "history_op.sealed_subject must not be empty",
                    ));
                }
                if sealed_delta.is_empty()
                    || crate::spam_model_seal::is_plaintext_delta(sealed_delta)
                {
                    return Err(invalid_params(
                        "history_op.sealed_delta must be sealed; a plaintext delta is never stored",
                    ));
                }
            }
            // Map the optional wire `history_op` onto the nest-internal
            // `SpamHistoryDbOp` (borrowed from `req`; the snake_case label/source
            // strings via `spam_label_wire`/`training_source_wire`). `None` ⇒ a
            // model-only write.
            let history_db_op = match &req.history_op {
                Some(SpamHistoryOp::Insert {
                    message_id,
                    mailbox,
                    sealed_subject,
                    sealed_delta,
                    label,
                    source,
                }) => Some(SpamHistoryDbOp::Insert {
                    message_id: message_id.as_slice(),
                    mailbox: mailbox.as_str(),
                    sealed_subject: sealed_subject.as_slice(),
                    sealed_delta: sealed_delta.as_slice(),
                    // An unknown label or source (a newer client's) is never
                    // stored: the row's class decides what an undo decrements.
                    label: spam_label_wire(*label).ok_or_else(|| {
                        invalid_params("history_op.label is not a label this nest knows")
                    })?,
                    source: training_source_wire(*source).ok_or_else(|| {
                        invalid_params("history_op.source is not a source this nest knows")
                    })?,
                }),
                Some(SpamHistoryOp::Delete { history_id }) => Some(SpamHistoryDbOp::Delete {
                    history_id: history_id.as_slice(),
                }),
                None => None,
            };
            let had_history_op = history_db_op.is_some();
            // The lesson an `Insert` teaches, for the report capture below.
            let inserted_lesson = match &req.history_op {
                Some(SpamHistoryOp::Insert {
                    message_id, label, ..
                }) => Some((message_id.clone(), *label)),
                _ => None,
            };
            // Optional deployment-baseline holder copy (the keyless
            // content.read{spam-model} shape, ratified 2026-07-13): the
            // writing agent re-sealed the post-mutation model to the
            // aggregation holder's pubkey; replace the stored copy for
            // (target, holder) in the SAME transaction so the copy never
            // lags the model. Opaque verbatim — the nest can read neither
            // the model nor the copy. Absent ⇒ leave any stored copy alone
            // (a writer that opted out or has no holder leaves stale
            // weights, never drops the contributor).
            let holder_copy: Option<(&[u8], &[u8])> = match &req.holder_copy {
                Some(c) => {
                    if c.holder_pubkey.len() != 32 {
                        return Err(malformed("holder_copy.holder_pubkey must be 32 bytes"));
                    }
                    if c.sealed_copy.is_empty() {
                        return Err(malformed("holder_copy.sealed_copy must not be empty"));
                    }
                    Some((c.holder_pubkey.as_slice(), c.sealed_copy.as_slice()))
                }
                None => None,
            };
            // Store the holder's re-sealed blob VERBATIM — opaque; the nest never
            // decodes it (it holds only the actor's public half). Counts are
            // stored as 0: an opaque blob carries no nest-visible per-class
            // counts. The advisory `req.sample_count` is display-only and not
            // persisted (no column); it is accepted for forward-compat. The
            // model write and the history op (if any) commit in ONE
            // transaction, so a train/undo is crash-atomic (co-design § 3,
            // option (a) — `put_spam_model_with_history`).
            // The one-lesson rule (`mail-spam.md` § 3), evaluated INSIDE the
            // write's transaction: an `Insert` repeating the actor's newest
            // recorded lesson for that message rejects the WHOLE write — model,
            // row and holder copy — so the stored model stays byte-identical to
            // the last accepted write, and the reply says so rather than
            // acking a write that did not happen.
            let write = state
                .db
                .put_spam_model_with_history(&target, &req.sealed_model, history_db_op, holder_copy)
                .await
                .map_err(internal)?;
            let undone = match write {
                crate::db::moderation::SpamModelWrite::DuplicateSignal => {
                    // Nothing was written, so nothing is captured either.
                    return encode_reply(&PutSpamModelReply {
                        outcome: PutSpamModelOutcome::DuplicateSignal,
                        extra: Default::default(),
                    });
                }
                crate::db::moderation::SpamModelWrite::Written { undone, .. } => undone,
            };
            // Report capture (report-sharing.md § Report capture): the explicit
            // mark-as-spam lesson — the MDA `\Junk` STORE/MOVE or the Fauna app's
            // mail button, both of which land here — emits a k-anonymized report
            // row; a ham lesson withdraws it, and so does undoing a spam lesson
            // (a withdrawn judgment leaves no residue). Keyed by the message's
            // stored canonical report-hash; a hash-less message simply cannot
            // aggregate. Runs after the write committed and never fails it.
            if let Some((message_id, label)) = inserted_lesson {
                capture_mail_report(&state, &target, &message_id, label == SpamLabel::Spam).await;
            }
            if let Some(lesson) = undone
                && Some(lesson.label.as_str()) == spam_label_wire(SpamLabel::Spam)
            {
                capture_mail_report(&state, &target, &lesson.message_id, false).await;
            }
            // A history mutation changed the training-history list; nudge the
            // target actor's other open surfaces to refresh. A model-only write
            // emits nothing.
            if had_history_op {
                state.ws.notify_push(
                    &target,
                    PushEvent::BridgeSpamModelUpdated(BridgeSpamModelUpdatedPush {
                        actor_id: target.to_vec(),
                    }),
                );
            }
            encode_reply(&PutSpamModelReply {
                outcome: PutSpamModelOutcome::Written,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.get_spam_scoring_policy` — the User-reachable read of the
/// admin-effective spam-scoring policy the on-device Fauna-app scorer needs
/// so its INBOX→Junk placement is byte-identical to the MDA/nest at every
/// scoring position (`mail-spam.md` § Architectural rules "Scoring placement =
/// search placement", § Combined-score formula). Admin-only `fetch_config` /
/// `get_mail_config` is unreachable to a client, so this getter projects the
/// four scoring knobs a User may read from the effective `SpamPolicyThresholds`
/// (catalog defaults + the admin `put_spam_policy` override applied). The
/// values are deployment-wide (no per-user override today) and non-secret, so
/// there is no caller-scope check — any admitted caller reads the same policy.
fn get_spam_scoring_policy_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // The allowlist gates this to `BridgeMda | User | Admin` (the same
            // class set as `fetch_spam_model`); `require_class` rejects any
            // other caller. No caller-scope check — the policy is server-wide.
            require_class(&state, &actor_id, "fauna.bridges.get_spam_scoring_policy").await?;
            // Decode-then-ignore: the request carries only the forward-compat
            // catch-all (the caller is the connection's authenticated actor).
            let _req: GetSpamScoringPolicyRequest = decode(&payload).map_err(malformed)?;
            // The same effective policy `fetch_spam_model` reads for its fade
            // horizon and the MDA reads via `fetch_config` — catalog defaults
            // overlaid with the admin `put_spam_policy` override.
            let eff = state
                .db
                .get_spam_policy()
                .await
                .map_err(internal)?
                .effective();
            encode_reply(&GetSpamScoringPolicyReply {
                spam_folder_threshold: eff.max_score_before_spam_folder,
                bayesian_weight_milli: eff.bayesian_weight_milli,
                bayesian_min_samples: eff.bayesian_min_samples,
                bayesian_full_confidence_samples: eff.bayesian_full_confidence_samples,
                extra: Default::default(),
            })
        })
    })
}

// ── publish_spam_baseline + set_baseline_contribution (Slice 5) ────
//
// The admin-opt-in deployment spam baseline (`mail-spam.md` § Cold start,
// Path 2). `publish_spam_baseline` aggregates the per-user models of every
// user who opted in (`spam_preferences.contribute_baseline = 1`) into one
// deployment-wide baseline by additive n-gram merge, and stores it in the
// single-row `spam_baseline` table. `set_baseline_contribution` is the
// per-user opt-in toggle (`mail-spam-contribute-baseline-toggle`).
//
// Product invariants (never violated here): baseline publish is off by
// default (it runs only on an explicit admin click, or on the cadence once the
// admin turns standing publish on); per-user
// contribution is opt-in, default off (the `contribute_baseline` column
// defaults 0); the admin cannot view individual contributions — the merged
// n-gram weights are sums that don't identify a contributor, and the reply
// carries only an aggregate `contributors` count, never any actor id; and an
// opt-out (or model delete) removes that user from the *next* republish
// (the aggregation reads `contribute_baseline` live each publish).

/// `fauna.bridges.publish_spam_baseline` — admin-only "publish now". A thin
/// gate over [`crate::spam_baseline::run_spam_baseline_publish`], the one run
/// the standing-publish cadence also drives: every contributor's sealed model is
/// driven through the granted holder's pull drain (`mail-spam.md`
/// § Encrypted-mode interaction) — the nest merges none itself; the contributor
/// floor then
/// the delta floor decide whether it lands, withholds or defers
/// (`mail-spam.md` § Cold start Path 2). Works the same with standing publish
/// on or off.
pub(crate) fn publish_spam_baseline_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // Admin-only (allowlist); a deployment-wide action, not caller-
            // scoped to a model. `require_class` rejects any non-admin caller.
            require_class(&state, &actor_id, "fauna.bridges.publish_spam_baseline").await?;
            let _req: PublishSpamBaselineRequest = decode(&payload).map_err(malformed)?;
            let outcome = crate::spam_baseline::run_spam_baseline_publish(&state)
                .await
                .map_err(internal)?;
            encode_reply(&PublishSpamBaselineReply {
                contributors: outcome.contributors,
                sample_count: outcome.sample_count,
                published: outcome.published,
                skipped_contributors: outcome.skipped_contributors,
                deferred: outcome.deferred,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.get_spam_baseline_state` — admin-only. The baseline's
/// current state, never its history (`mail-spam.md` § Cold start Path 2 →
/// *Standing publish*): whether a real baseline is served, over how many
/// contributors and samples and since when, the last run's skipped count and
/// whether it was deferred, and whether standing publish is on. No withdrawal
/// time or reason exists to return — a withdrawn baseline reads as never
/// published.
fn get_spam_baseline_state_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_spam_baseline_state").await?;
            let _req: GetSpamBaselineStateRequest = decode(&payload).map_err(malformed)?;
            let (published, run) = state.db.get_spam_baseline_state().await.map_err(internal)?;
            let standing = state
                .db
                .get_spam_policy()
                .await
                .map_err(internal)?
                .effective()
                .baseline_standing_publish;
            encode_reply(&GetSpamBaselineStateReply {
                published: published.is_some(),
                contributors: published.map_or(0, |p| p.contributors),
                sample_count: published.map_or(0, |p| p.sample_count),
                published_at: published.map(|p| p.published_at),
                skipped_contributors: run.skipped_contributors,
                deferred: run.deferred,
                standing,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.set_baseline_contribution` — the per-user opt-in toggle.
/// Caller-scoped: the connection's actor is the subject (no `actor_id`
/// field), so a user opts only their own training in/out — there is no
/// cross-actor contribution surface (`mail-spam.md` § Cross-actor isolation).
pub(crate) fn set_baseline_contribution_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_baseline_contribution").await?;
            let req: SetBaselineContributionRequest = decode(&payload).map_err(malformed)?;
            // Read-modify-write so the other spam preferences (the
            // thresholds) are preserved (shared `spam_preferences`
            // row with `fauna.spam.set_preferences`).
            let mut prefs = state
                .db
                .get_spam_preferences(&actor_id)
                .await
                .map_err(internal)?;
            // Opting OUT as a summed contributor withdraws the published
            // baseline — the sum cannot give these counts back, so it stops
            // being served. Ahead of the write below, which clears the bit a
            // baseline older than the inclusion record is withdrawn on
            // (`mail-spam.md` § Cold start Path 2 → *A contributor's
            // departure withdraws the baseline*).
            if !req.contribute {
                state
                    .db
                    .withdraw_spam_baseline_if_contributor(&actor_id)
                    .await
                    .map_err(internal)?;
            }
            prefs.contribute_baseline = req.contribute;
            state
                .db
                .upsert_spam_preferences(&actor_id, &prefs)
                .await
                .map_err(internal)?;
            // Opting OUT deletes the actor's sealed-to-holder model copies —
            // the at-rest artifact of the contribution goes with the consent
            // (`mail-spam.md` § Encrypted-mode interaction: "toggle-OFF /
            // grant revoke / model reset deletes it"). The client separately
            // revokes the paired keyless grant; this covers the nest-resident
            // bytes even if that revoke never arrives. Recreatable on
            // re-opt-in (the agent seals a fresh copy).
            if !req.contribute {
                state
                    .db
                    .delete_spam_model_holder_copies(&actor_id)
                    .await
                    .map_err(internal)?;
            }
            encode_reply(&SetBaselineContributionReply {
                contribute: req.contribute,
                extra: Default::default(),
            })
        })
    })
}

// ── reset_spam_model / list_spam_training_history ──────────────────────────────
//
// The user-tier training-management RPCs that light up the `mail-spam` page
// (`mail-spam.md` §§ Reset, Training-sample retention, Undo — an undo is the
// client's own `put_spam_model` with a history `Delete`). Both are
// **caller-scoped**: the connection's actor is the subject (no target field),
// so a user — even an admin — manages only their own model + history
// (§ Cross-actor isolation). The two `BridgeSpamModel{Updated,Reset}` push events
// nudge the actor's *other* open clients to refresh their history list.

/// Default page size for `list_spam_training_history` (`mail-spam.md` § Undo —
/// "the most recent N rows, default 100").
const SPAM_HISTORY_DEFAULT_LIMIT: u32 = 100;
/// Hard cap so a client can't request an unbounded page.
const SPAM_HISTORY_MAX_LIMIT: u32 = 500;

/// Wire `SpamLabel` → its stored snake_case string. Kept an explicit match (not
/// `serde`) so the column value is stable and a new variant is a compile error
/// here, not a silent miss. `None` for `Unknown`: a label this nest does not
/// know is never stored.
fn spam_label_wire(label: SpamLabel) -> Option<&'static str> {
    match label {
        SpamLabel::Spam => Some("spam"),
        SpamLabel::Ham => Some("ham"),
        SpamLabel::Unknown => None,
    }
}

/// Inverse of [`spam_label_wire`] for reading a stored row back into the wire
/// enum. Anything but `spam`/`ham` reads as `Unknown`, never as a guessed class:
/// the client refuses to undo such a row, since the label picks the class an
/// undo decrements.
fn spam_label_from_wire(s: &str) -> SpamLabel {
    match s {
        "spam" => SpamLabel::Spam,
        "ham" => SpamLabel::Ham,
        _ => SpamLabel::Unknown,
    }
}

/// Wire `TrainingSource` → its stored snake_case string (see [`spam_label_wire`]
/// for why the mapping is explicit). `None` for `Unknown`.
fn training_source_wire(source: TrainingSource) -> Option<&'static str> {
    match source {
        TrainingSource::ImapJunkFlag => Some("imap_junk_flag"),
        TrainingSource::ImapJunkMove => Some("imap_junk_move"),
        TrainingSource::ManualOther => Some("manual_other"),
        TrainingSource::Unknown => None,
    }
}

/// Inverse of [`training_source_wire`]. A stored source this build does not
/// know reads as `Unknown` (a neutral badge), not as `ManualOther`.
fn training_source_from_wire(s: &str) -> TrainingSource {
    match s {
        "imap_junk_flag" => TrainingSource::ImapJunkFlag,
        "imap_junk_move" => TrainingSource::ImapJunkMove,
        "manual_other" => TrainingSource::ManualOther,
        _ => TrainingSource::Unknown,
    }
}

/// `fauna.bridges.reset_spam_model` — delete the caller's per-user model + all
/// their training history; emit `BridgeSpamModelReset`. Irreversible
/// (`mail-spam.md` § Reset). Idempotent (no model/history ⇒ a clean no-op ack).
fn reset_spam_model_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // Caller-scoped: `require_class` gates User|Admin; the subject is the
            // connection's actor — even an admin resets only their own model.
            require_class(&state, &actor_id, "fauna.bridges.reset_spam_model").await?;
            let _req: ResetSpamModelRequest = decode(&payload).map_err(malformed)?;

            // A summed contributor's reset deletes the model the published
            // baseline summed, so the baseline goes first — while the model row
            // still says this actor contributed, for a baseline older than the
            // inclusion record (`mail-spam.md` § Cold start Path 2 → *A
            // contributor's departure withdraws the baseline*).
            state
                .db
                .withdraw_spam_baseline_if_contributor(&actor_id)
                .await
                .map_err(internal)?;

            // Delete the model and the whole audit trail.
            state
                .db
                .delete_spam_model(&actor_id)
                .await
                .map_err(internal)?;
            state
                .db
                .delete_all_spam_training_history(&actor_id)
                .await
                .map_err(internal)?;
            // The sealed-to-holder baseline copies mirror the model that just
            // ceased to exist — delete them too (`mail-spam.md` § Encrypted-
            // mode interaction: "toggle-OFF / grant revoke / model reset
            // deletes it"). Recreatable on the next opted-in write.
            state
                .db
                .delete_spam_model_holder_copies(&actor_id)
                .await
                .map_err(internal)?;

            // Tell the actor's other open surfaces to clear their history list.
            state.ws.notify_push(
                &actor_id,
                PushEvent::BridgeSpamModelReset(BridgeSpamModelResetPush {
                    actor_id: actor_id.to_vec(),
                }),
            );
            encode_reply(&ResetSpamModelReply::default())
        })
    })
}

/// `fauna.bridges.list_spam_training_history` — the caller's recent training-
/// history rows (newest-first, paginated) plus the `contribute_baseline`
/// read-back the `mail-spam-contribute-baseline-toggle` renders (`mail-spam.md`
/// §§ Training-sample retention, Wire shapes).
fn list_spam_training_history_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.list_spam_training_history",
            )
            .await?;
            let req: ListSpamTrainingHistoryRequest = decode(&payload).map_err(malformed)?;
            let limit = req
                .limit
                .unwrap_or(SPAM_HISTORY_DEFAULT_LIMIT)
                .clamp(1, SPAM_HISTORY_MAX_LIMIT);
            let before_vec: Option<Vec<u8>> = req.before_history_id.map(|b| b.into_vec());

            let rows = state
                .db
                .list_spam_training_history(&actor_id, limit, before_vec.as_deref())
                .await
                .map_err(internal)?;
            // The toggle read-back rides this RPC (the setter
            // `set_baseline_contribution` had no read path until now).
            let prefs = state
                .db
                .get_spam_preferences(&actor_id)
                .await
                .map_err(internal)?;

            let events = rows
                .into_iter()
                .map(|r| {
                    // The subject rests only sealed (`sealed_subject`), so this
                    // handler cannot format it: `message` carries the mailbox
                    // alone, and a client renders `{unwrapped subject} ·
                    // {mailbox}` from `sealed_subject` + `mailbox`. The localized
                    // label/source badges are rendered client-side from the enums.
                    SpamTrainingHistoryRow {
                        history_id: r.history_id,
                        message: r.mailbox.clone(),
                        sealed_subject: r.sealed_subject,
                        mailbox: r.mailbox,
                        label: spam_label_from_wire(&r.label),
                        source: training_source_from_wire(&r.source),
                        created_at_ms: r.created_at,
                        // The stored event delta, returned verbatim (opaque to
                        // this handler) — sealed to the caller's own key, which
                        // unwraps it to replay the inverse for an undo (co-design
                        // § 3).
                        model_delta_applied: r.model_delta_applied,
                        extra: Default::default(),
                    }
                })
                .collect();
            encode_reply(&ListSpamTrainingHistoryReply {
                events,
                contribute_baseline: prefs.contribute_baseline,
                extra: Default::default(),
            })
        })
    })
}

// ── create_mailbox / delete_mailbox / rename_mailbox (I5 Phase D.6) ─

/// Compute a freshly-allocated `uid_validity` for a new mailbox row.
/// `now_millis as u32` truncates the 64-bit wall-clock to RFC 9051
/// §2.3.1.1's 32-bit width. The result is monotonically increasing in
/// real time (until u32 wraps, ~50 days from epoch alignment — not a
/// real concern: per-mailbox-lifetime uniqueness, not global, is the
/// requirement). Coerced to `≥ 2` so the value never collides with
/// `ensure_bridge_imap_mailboxes`'s default `uid_validity = 1` even
/// when the wall clock somehow lands at zero.
pub(crate) fn fresh_uid_validity() -> u32 {
    let millis = fauna_core::data::Timestamp::now_millis_or_zero();
    (millis as u32).max(2)
}

fn create_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.create_mailbox").await?;
            let req: CreateMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // 1. Protected-name guard. The six standard mailboxes are
            //    auto-seeded on first AUTH; CREATE against them is
            //    rejected at the wire layer with a distinct outcome
            //    so the bridge can surface "reserved" vs.
            //    "already_exists" separately. The guardian held mailbox
            //    is equally protected — only the hold path creates it.
            if is_protected_mailbox(&req.name) {
                return encode_reply(&CreateMailboxReply::Reserved);
            }

            // 2. Name validation per RFC 9051 §5.1.
            if let Err(reason) = validate_mailbox_name(&req.name) {
                return encode_reply(&CreateMailboxReply::InvalidName { reason });
            }

            // 3. Insert into the placement layer.
            let uid_validity = fresh_uid_validity();
            let outcome = state
                .db
                .create_bridge_imap_mailbox(&target, &req.name, uid_validity)
                .await
                .map_err(internal)?;
            match outcome {
                CreateMailboxDbOutcome::Created {
                    uid_validity: created_uid_validity,
                } => {
                    // Spec § D2 (Create record shape): mailbox name +
                    // its uid_validity + RFC 6154 SPECIAL-USE attrs
                    // (empty Vec for user-created mailboxes per the
                    // imap-server.md § Standard mailboxes rule —
                    // attrs other than the six are not assigned).
                    //
                    // Spec § D6 (ε) atomic-with-SQL: the SQLite
                    // `INSERT OR IGNORE` inside
                    // `create_bridge_imap_mailbox` has already
                    // committed; the placement append below runs
                    // after. The narrow crash window is closed by
                    // Plan 2 T9's divergence detection at SELECT /
                    // QRESYNC time — same pattern as T7's APPEND /
                    // STORE / EXPUNGE wiring and T8's MOVE / COPY.
                    let record = MailPlacementRecord::Create {
                        mailbox: req.name.clone(),
                        uid_validity: created_uid_validity,
                        attrs: Vec::new(),
                    };
                    state
                        .mail_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    encode_reply(&CreateMailboxReply::Created {
                        uid_validity: created_uid_validity,
                    })
                }
                CreateMailboxDbOutcome::AlreadyExists => {
                    // Idempotent-skip: no DB row was inserted (the
                    // INSERT OR IGNORE matched an existing row), so
                    // no placement event is emitted.
                    encode_reply(&CreateMailboxReply::AlreadyExists)
                }
            }
        })
    })
}

fn delete_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.delete_mailbox").await?;
            let req: DeleteMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // 1. Protected-name guard — all six are immovable on DELETE
            //    (INBOX is renamable per §6.3.6, but never deletable), and
            //    the guardian held mailbox must not be deletable out from
            //    under a live hold (`family-safety.md` § The mail gate).
            if is_protected_mailbox(&req.name) {
                return encode_reply(&DeleteMailboxReply::Reserved);
            }

            // 2. Resolve the *effective* deployment policy (catalog ⊕ admin
            //    `put_imap_policy` override) so an admin who sets
            //    `delete_nonempty: "allowed"` actually takes effect. Per-actor
            //    tiers are Phase F+; the single `mail_imap_policy` row is
            //    deployment-wide.
            let policy = state
                .db
                .get_imap_policy()
                .await
                .map_err(internal)?
                .effective();
            let allow_nonempty = policy.delete_nonempty.eq_ignore_ascii_case("allowed");

            let outcome = state
                .db
                .delete_bridge_imap_mailbox(&target, &req.name, allow_nonempty)
                .await
                .map_err(internal)?;
            match outcome {
                DeleteMailboxDbOutcome::Deleted => {
                    // Spec § D2 (Delete record shape): single
                    // `mailbox` field; the manifest-side apply
                    // cascades to drop the mailbox state row + all
                    // its placements + tombstones (per
                    // `apply_record_to_manifest` in
                    // `segments/mail_placement.rs`).
                    //
                    // Spec § D6 (ε) atomic-with-SQL: SQLite
                    // transaction inside `delete_bridge_imap_mailbox`
                    // (which writes its own tombstones + drops the
                    // mailbox-state row + deletes placements all
                    // in one tx) has committed by the time the
                    // placement append below runs. The crash window
                    // is closed by Plan 2 T9's divergence detection
                    // — same pattern as T7 / T8.
                    let record = MailPlacementRecord::Delete {
                        mailbox: req.name.clone(),
                    };
                    state
                        .mail_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    encode_reply(&DeleteMailboxReply::Deleted)
                }
                DeleteMailboxDbOutcome::NoSuchMailbox => {
                    // Idempotent-skip: nothing to delete, nothing to
                    // record.
                    encode_reply(&DeleteMailboxReply::NoSuchMailbox)
                }
                DeleteMailboxDbOutcome::NotEmpty => {
                    // Policy reject: the mailbox still exists, so no
                    // placement event is emitted.
                    encode_reply(&DeleteMailboxReply::NotEmpty)
                }
            }
        })
    })
}

fn rename_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.rename_mailbox").await?;
            let req: RenameMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // 1. Protected-source guard — every standard mailbox other
            //    than INBOX rejects with ReservedSource (INBOX gets
            //    the §6.3.6 special-case in the DB helper), and the
            //    guardian held mailbox must not be renamable away from
            //    the hold path (`family-safety.md` § The mail gate).
            if is_protected_mailbox(&req.old_name) && req.old_name != "INBOX" {
                return encode_reply(&RenameMailboxReply::ReservedSource);
            }

            // 2. Target-protected guard — RENAME into any protected
            //    name is rejected (distinct from TargetExists for
            //    clear user feedback). `INBOX` as a target is also
            //    reserved — the empty INBOX is re-seeded by the
            //    §6.3.6 special-case; the wire layer rejects callers
            //    asking for INBOX-as-target.
            if is_protected_mailbox(&req.new_name) {
                return encode_reply(&RenameMailboxReply::TargetReserved);
            }

            // 3. Name validation on the target.
            if let Err(reason) = validate_mailbox_name(&req.new_name) {
                return encode_reply(&RenameMailboxReply::InvalidName { reason });
            }

            // 4. Carry out the rename. INBOX-rename allocates two
            //    fresh uid_validity values: one for the migrated
            //    `new_name` mailbox, one for the re-seeded empty
            //    INBOX. Non-INBOX renames preserve the source row's
            //    uid_validity (RFC 9051 §6.3.6 — no MUA re-sync).
            let new_uid_validity = fresh_uid_validity();
            // Bias the INBOX re-seed by 1 ms so the empty INBOX
            // never collides with the migrated mailbox's
            // uid_validity in the same-millisecond case.
            let inbox_uid_validity = new_uid_validity.saturating_add(1).max(2);
            let outcome = state
                .db
                .rename_bridge_imap_mailbox(
                    &target,
                    &req.old_name,
                    &req.new_name,
                    new_uid_validity,
                    inbox_uid_validity,
                )
                .await
                .map_err(internal)?;
            match outcome {
                RenameMailboxDbOutcome::Renamed => {
                    // Spec § D2 (Rename record shape): `{ old, new }`
                    // — the manifest-side apply relabels every
                    // mailbox state row + placement + tombstone whose
                    // mailbox matches `old` (per
                    // `apply_record_to_manifest`).
                    //
                    // Spec § D6 (ε) atomic-with-SQL: the SQLite
                    // transaction inside `rename_bridge_imap_mailbox`
                    // has committed by the time the placement append
                    // below runs. Plan 2 T9 catches the crash window
                    // — same as T7 / T8.
                    let record = MailPlacementRecord::Rename {
                        old: req.old_name.clone(),
                        new: req.new_name.clone(),
                    };
                    state
                        .mail_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;

                    // INBOX-rename special case (RFC 9051 §6.3.6): the
                    // DB layer migrates every placement out of INBOX
                    // into `new_name` with renumbered UIDs (uid_next /
                    // highestmodseq reset to 1) and re-seeds an empty
                    // INBOX row with a freshly-allocated
                    // `inbox_uid_validity`. The placement journal
                    // captures the relabel via the `Rename` record
                    // above; a follow-up `Create` re-seeds INBOX in
                    // the manifest so subsequent reads from the
                    // compacted state see the empty INBOX row.
                    //
                    // Known divergence — deferred to Plan 2 T9: the
                    // Rename record's manifest-side apply preserves
                    // the source mailbox's `uid_validity` (i.e. 1
                    // from the seed) and the source placements' UIDs
                    // verbatim under the new name, whereas the DB
                    // assigns `new_uid_validity` to the migrated row
                    // and renumbers UIDs starting from 1. Plan 2 T9's
                    // divergence detection at SELECT / QRESYNC time
                    // is the design's mechanism for catching and
                    // repairing this drift (spec § D6 (γ)). The
                    // Rename record's spec-fixed `{ old, new }`
                    // shape (§ D2) cannot natively express either the
                    // UID renumber or the uid_validity bump.
                    if req.old_name == "INBOX" {
                        let inbox_create = MailPlacementRecord::Create {
                            mailbox: "INBOX".to_string(),
                            uid_validity: inbox_uid_validity,
                            attrs: standard_mailbox_attrs("INBOX"),
                        };
                        state
                            .mail_placement
                            .append_event(&target, &inbox_create)
                            .await
                            .map_err(placement_journal_diverged)?;
                    }
                    encode_reply(&RenameMailboxReply::Renamed)
                }
                RenameMailboxDbOutcome::NoSuchSource => {
                    // Idempotent-skip: source row absent, nothing to
                    // rename, no record.
                    encode_reply(&RenameMailboxReply::NoSuchSource)
                }
                RenameMailboxDbOutcome::TargetExists => {
                    // Reject: nothing changed in the DB, so no
                    // placement event.
                    encode_reply(&RenameMailboxReply::TargetExists)
                }
            }
        })
    })
}

// ── I5 Phase D.7 — SUBSCRIBE / UNSUBSCRIBE ────────────────────────

fn subscribe_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.subscribe_mailbox").await?;
            let req: SubscribeMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // RFC 9051 §6.3.7 explicitly permits SUBSCRIBE on a
            // not-yet-existing mailbox — MUAs subscribe before CREATE
            // is a normal pattern. The DB layer is `INSERT OR IGNORE`
            // so the call is idempotent on PK conflict too.
            state
                .db
                .insert_bridge_imap_subscription(&target, &req.mailbox)
                .await
                .map_err(internal)?;
            // Spec § D2 (Subscribe record shape): single `mailbox`
            // field; the manifest-side apply pushes onto
            // `subscriptions` only when the entry is absent (see
            // `apply_record_to_manifest` in
            // `segments/mail_placement.rs`), so duplicate-emission on
            // repeated SUBSCRIBE is safe.
            //
            // Spec § D6 (ε) atomic-with-SQL: the SQLite
            // `INSERT OR IGNORE` inside
            // `insert_bridge_imap_subscription` has already committed
            // when we reach this point; the placement append below
            // runs after. The narrow commit-then-append crash window
            // is closed by Plan 2 T9's divergence detection at SELECT
            // / LIST(SUBSCRIBED) time — same pattern as T7's APPEND /
            // STORE / EXPUNGE, T8's MOVE / COPY, T9's CREATE / DELETE
            // / RENAME wiring.
            //
            // Unlike T9, no idempotent-skip branch: the DB helper
            // returns `Result<()>` (it cannot distinguish "row
            // inserted" from "PK conflict absorbed by INSERT OR
            // IGNORE"), and the apply-side is already idempotent for
            // duplicate Subscribe records — so we emit unconditionally
            // on Ok(()) rather than churn the DB signature for a no-op
            // signal the manifest doesn't need. Journal noise from a
            // misbehaving MUA's SUBSCRIBE-twice compacts at D7.
            let record = MailPlacementRecord::Subscribe {
                mailbox: req.mailbox.clone(),
            };
            state
                .mail_placement
                .append_event(&target, &record)
                .await
                .map_err(placement_journal_diverged)?;
            encode_reply(&SubscribeMailboxReply::Subscribed)
        })
    })
}

fn unsubscribe_mailbox_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.unsubscribe_mailbox").await?;
            let req: UnsubscribeMailboxRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &target).await?;

            // RFC 9051 §6.3.8 makes UNSUBSCRIBE-on-not-subscribed a
            // success; the `DELETE` simply affects zero rows.
            state
                .db
                .delete_bridge_imap_subscription(&target, &req.mailbox)
                .await
                .map_err(internal)?;
            // Spec § D2 (Unsubscribe record shape): single `mailbox`
            // field; the manifest-side apply does
            // `subscriptions.retain(|s| s != mailbox)` (see
            // `apply_record_to_manifest` in
            // `segments/mail_placement.rs`), which is a no-op when the
            // mailbox isn't subscribed — so duplicate-emission on
            // repeated UNSUBSCRIBE is safe.
            //
            // Spec § D6 (ε) atomic-with-SQL: the SQLite DELETE inside
            // `delete_bridge_imap_subscription` has already committed
            // when we reach this point; the placement append below
            // runs after. The narrow commit-then-append crash window
            // is closed by Plan 2 T9's divergence detection at SELECT
            // / LIST(SUBSCRIBED) time — same pattern as T7's APPEND /
            // STORE / EXPUNGE, T8's MOVE / COPY, T9's CREATE / DELETE
            // / RENAME wiring.
            //
            // Same rationale as SUBSCRIBE for the unconditional emit
            // on Ok(()): DB helper returns `Result<()>` (can't
            // distinguish "row deleted" from "DELETE on zero rows"),
            // apply-side is idempotent, no DB-signature churn.
            let record = MailPlacementRecord::Unsubscribe {
                mailbox: req.mailbox.clone(),
            };
            state
                .mail_placement
                .append_event(&target, &record)
                .await
                .map_err(placement_journal_diverged)?;
            encode_reply(&UnsubscribeMailboxReply::Unsubscribed)
        })
    })
}

// ── I5 Phase F.1 — subscribe_mailbox_state (IDLE / NOTIFY push) ────

fn subscribe_mailbox_state_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // `actor_id` here is the *MDA service-user*'s WS-RPC
            // caller identity; the request payload carries the
            // *served user*'s actor_id (claim 1 in TODO §
            // Load-bearing claims / `imap-server.md` § IDLE / Push
            // wiring). The MDA is allowed to subscribe on behalf of
            // any served user it currently has an AUTH'd IMAP
            // session for — nest does NOT re-verify the AUTH binding
            // here because the MDA already enforces per-session
            // ownership (the request never reaches nest without a
            // logged-in IMAP session backing it).
            require_class(&state, &actor_id, "fauna.bridges.subscribe_mailbox_state").await?;
            let req: SubscribeMailboxStateRequest = decode(&payload).map_err(malformed)?;
            let served: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_local_mail_serving(&state, &served).await?;

            // Empty mailbox = NOTIFY wildcard (claim 5/6). Stored
            // verbatim — the emission path keys exact-match on
            // mailbox string, so wildcard subscriptions are matched
            // by emitters that pass an empty mailbox at the call
            // site. (NOTIFY v1 doesn't emit on the wildcard path
            // yet because the per-mailbox emission hooks all pass
            // their concrete mailbox; the wildcard slot lights up
            // once F.2's NOTIFY handler also subscribes per-mailbox
            // discovered from LIST.)
            let subscription_id =
                state
                    .bridge_push_registry
                    .register(&req.actor_id, &req.mailbox, actor_id);
            encode_reply(&SubscribeMailboxStateReply::Subscribed { subscription_id })
        })
    })
}

/// Helper: emit a `MailboxStateEvent` to every matching subscription.
/// Wraps `state.bridge_push_registry.emit(...)` so each callsite
/// reads as a one-liner; the helper exists for symmetry with the
/// other in-this-file post-commit emission helpers (e.g.
/// `emit_bootstrap_create_records`). Always called AFTER the SQLite
/// transaction commits (Spec § D6 (ε)).
pub(crate) fn emit_mailbox_state_event(
    state: &std::sync::Arc<crate::routes::AppState>,
    served_user: &[u8; 32],
    mailbox: &str,
    event: MailboxStateEvent,
) {
    state
        .bridge_push_registry
        .emit(&state.ws, served_user, mailbox, &event);
}

// ── Registration entry point ──────────────────────────────────────

pub fn register_bridge_imap_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.list_mailboxes",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_mailboxes_handler(),
        },
    );
    b.add(
        "fauna.bridges.select_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: select_mailbox_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_messages",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_messages_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_message_metadata",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_message_metadata_handler(),
        },
    );
    // C.3 — body-fetch + index-segments.
    // fetch_message_ciphertext uses the 60 s routing deadline (bodies can be
    // tens of MB); fetch_index_segments_since is metadata-only → 5 s.
    b.add(
        "fauna.bridges.fetch_message_ciphertext",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: fetch_message_ciphertext_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_index_segments_since",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_index_segments_since_handler(),
        },
    );
    // C.4 — flag-store + expunge.
    //
    // `store_flags` is `forbid_replay = false`, **decided** by the 82nd-pass
    // audit and only true because that audit made it true. Set/Add/Remove are
    // idempotent set operations, so the flag *state* always converged — but the
    // handler used to write and re-stamp the row unconditionally, which made
    // `modseq` an accumulator with no dedup key. Two consequences, the second
    // materially worse than a merely-diverging answer:
    //   * HIGHESTMODSEQ advanced on every re-issue, waking IDLE subscribers and
    //     re-reporting the messages to QRESYNC clients; and
    //   * with CONDSTORE `UNCHANGEDSINCE`, the first call's own bump pushed the
    //     row past the supplied value, so the identical re-issue was answered
    //     `MODIFIED` — telling the MUA a concurrent conflict had happened when
    //     nothing but its own retry had touched the message.
    // `CacheDb::apply_store_flags` now skips already-satisfied UIDs (reporting
    // them with their existing modseq) and tests that before the `UNCHANGEDSINCE`
    // gate, so a replay is byte-identical. Pinned by
    // `replayed_store_flags_is_byte_identical_and_does_not_bump_modseq` and
    // `replayed_conditional_store_does_not_report_a_false_conflict`.
    b.add(
        "fauna.bridges.store_flags",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: store_flags_handler(),
        },
    );
    // `expunge` is `forbid_replay = false`, decided: `apply_expunge` resolves
    // its target set from rows still carrying `\Deleted`, and an empty target
    // returns before any write — so the replay of a completed EXPUNGE bumps no
    // modseq, inserts no `bridge_imap_expunged` row and appends no journal
    // record. Only the reply diverges (`expunged_uids: []`), the 76th pass's
    // consume-shaped class. Worth noting for a future pass: unlike most of that
    // class a lookup remedy *is* reachable here — `bridge_imap_expunged` retains
    // (uid, modseq, expunged_at) for exactly the UIDs this actor expunged — so
    // the reply could be reconstructed if the divergence ever proves to matter.
    b.add(
        "fauna.bridges.expunge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: expunge_handler(),
        },
    );
    // C.5 — copy + move.
    //
    // These two run the SAME mutation helper (`CacheDb::apply_copy` and
    // `CacheDb::apply_move` both call `copy_within_locked`) and still take
    // **opposite** `forbid_replay` values. That is deliberate, and it is the
    // refinement the 82nd-pass audit owes the 80th pass's shared-helper
    // predicate: reaching one non-idempotent helper does not force one value at
    // every ingress — what decides it is whether the *kind* consumes the input
    // that helper reads.
    //
    // `copy` does not. `copy_within_locked` allocates the destination UID from
    // the dest mailbox's `uid_next` and inserts the placement with a bare
    // `INSERT` keyed on that fresh UID, so nothing in the request identifies the
    // copy. A replay stores a second placement for every source UID —
    // user-visible duplicate mail, plus a second RFC 9208 quota charge from
    // `enforce_imap_storage_quota`. A genuine double-apply, so the 76th pass's
    // lookup remedy does not apply and the flip is the honest declaration
    // (pinned by `replayed_copy_duplicates_the_message_hazard_pin`).
    b.add(
        "fauna.bridges.copy",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: copy_messages_handler(),
        },
    );
    // `move` does consume it: MOVE is copy-then-expunge inside one lock scope,
    // so the first call removes the very source rows the copy step reads. A
    // replay copies nothing and skips every source-side touch — no duplicate,
    // state converged. Its reply diverges (`moved: []`), which is the 76th
    // pass's consume-shaped class and not grounds for a flip. `false`, decided
    // (pinned by `replayed_move_is_a_no_op_and_does_not_duplicate`).
    b.add(
        "fauna.bridges.move",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: move_messages_handler(),
        },
    );
    // C.6 — append.  60 s deadline: encrypted bodies can be tens of MB.
    b.add(
        "fauna.bridges.append",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: append_message_handler(),
        },
    );
    // C.7 — search_messages.
    b.add(
        "fauna.bridges.search_messages",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: search_messages_handler(),
        },
    );
    // C.8 — get_quota.
    b.add(
        "fauna.bridges.get_quota",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_quota_handler(),
        },
    );
    // Slice 4 item 1 — fetch_spam_model. A self-only model read +
    // seal-on-read: one `spam_models` row read, an MSEK-pubkey lookup, and
    // one HPKE seal (bounded by `mail.spam.model_max_bytes`, default 1 MiB).
    // 5 s default is generous.
    b.add(
        "fauna.bridges.fetch_spam_model",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_spam_model_handler(),
        },
    );
    // The opaque sealed-write-back twin of `fetch_spam_model` (leg 3 of the
    // tier-1 spam-model client-write end-game). One `spam_models` row overwrite
    // with the client-re-sealed blob (opaque; no decode/seal/lookup), so the 5 s
    // default is generous. An overwrite is idempotent (replay-safe), NOT
    // forbid_replay; mirrors `fetch_spam_model`/`reset_spam_model`.
    b.add(
        "fauna.bridges.put_spam_model",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_spam_model_handler(),
        },
    );
    // The User-reachable read of the admin-effective spam-scoring policy for the
    // on-device Fauna-app scorer (`mail-spam.md` § Scoring placement). A pure
    // read of the single overlaid policy row — no seal, no per-actor lookup —
    // so the 5 s default is generous. Replay-safe (idempotent read), NOT
    // forbid_replay; mirrors `fetch_spam_model`.
    b.add(
        "fauna.bridges.get_spam_scoring_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_spam_scoring_policy_handler(),
        },
    );
    // Slice 5 — publish_spam_baseline (admin-opt-in deployment baseline).
    // Aggregating opt-in users' models scales with the contributor count ×
    // model size (each bounded by `mail.spam.model_max_bytes`, default 1 MiB);
    // 30 s covers a large deployment's re-publish. Admin-triggered + rare.
    b.add(
        "fauna.bridges.publish_spam_baseline",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: publish_spam_baseline_handler(),
        },
    );
    // The baseline's current state for the admin page (`mail-spam.md` § Cold
    // start Path 2 → *Standing publish*). Two single-row reads; 5 s.
    b.add(
        "fauna.bridges.get_spam_baseline_state",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_spam_baseline_state_handler(),
        },
    );
    // Slice 5 — set_baseline_contribution (per-user opt-in toggle). A single
    // `spam_preferences` read-modify-write; idempotent (replay-safe), 5 s.
    b.add(
        "fauna.bridges.set_baseline_contribution",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_baseline_contribution_handler(),
        },
    );
    // Slice 5 rest — the user-tier training-management RPCs (mail-spam.md
    // §§ Reset, Training-sample retention, Undo). Each is caller-scoped + a
    // small bounded DB op; 5 s. reset/undo mutate (a row delete + cache evict /
    // an inverse-delta re-apply) but are idempotent, so replay-safe.
    b.add(
        "fauna.bridges.reset_spam_model",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: reset_spam_model_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_spam_training_history",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_spam_training_history_handler(),
        },
    );
    // I5 Phase D.6 — mailbox admin (CREATE / DELETE / RENAME). Each
    // touches at most a single mailbox-state row + the placement
    // table; 5 s default covers even the worst-case INBOX-rename
    // (the row-migration shape is bounded by the per-actor placement
    // count which the quota policy already caps).
    b.add(
        "fauna.bridges.create_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: create_mailbox_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_mailbox_handler(),
        },
    );
    b.add(
        "fauna.bridges.rename_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: rename_mailbox_handler(),
        },
    );
    // I5 Phase D.7 — IMAP SUBSCRIBE / UNSUBSCRIBE. Two single-row
    // writes against `bridge_imap_subscriptions`; 5 s default
    // deadline is generous.
    b.add(
        "fauna.bridges.subscribe_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: subscribe_mailbox_handler(),
        },
    );
    b.add(
        "fauna.bridges.unsubscribe_mailbox",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: unsubscribe_mailbox_handler(),
        },
    );
    // I5 Phase F.1 — IDLE / NOTIFY push subscription. Pure
    // in-memory registry write; 5 s default deadline is generous.
    // The push registry registration is per-WS-connection-lifetime
    // per `imap-server.md` § IDLE (claim 7).
    b.add(
        "fauna.bridges.subscribe_mailbox_state",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: subscribe_mailbox_state_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use super::*;
    use crate::bridge_approval_test_support::approve_bridge;
    use crate::db::{CacheDb, bridge_service_users::BridgeRole};
    use crate::routes::AppState;
    use crate::test_support::{seed_recipient_seal_key, unseal_fetched_model};
    use fauna_protocol::encode_canonical;
    use fauna_segment_store::VersionedManifest;
    #[allow(unused_imports)]
    use rusqlite;

    /// A stored label or source this build does not know reads back as
    /// `Unknown` — never as `Spam` (which would let a client undo decrement the
    /// wrong class) or as the first-party `ManualOther` — and an `Unknown` is
    /// never mapped to a stored string.
    #[test]
    fn stored_spam_strings_read_unknown_values_as_unknown_and_never_write_them() {
        assert_eq!(spam_label_from_wire("spam"), SpamLabel::Spam);
        assert_eq!(spam_label_from_wire("ham"), SpamLabel::Ham);
        assert_eq!(spam_label_from_wire("quarantine"), SpamLabel::Unknown);
        assert_eq!(
            training_source_from_wire("manual_other"),
            TrainingSource::ManualOther
        );
        assert_eq!(
            training_source_from_wire("from_the_future"),
            TrainingSource::Unknown
        );
        assert_eq!(spam_label_wire(SpamLabel::Unknown), None);
        assert_eq!(training_source_wire(TrainingSource::Unknown), None);
        for label in [SpamLabel::Spam, SpamLabel::Ham] {
            let stored = spam_label_wire(label).expect("a known label is storable");
            assert_eq!(spam_label_from_wire(stored), label);
        }
    }

    // ── Test fixtures (mirrors bridge_routing_handlers.rs::tests) ──

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    // ── list_mailboxes tests ────────────────────────────────────────

    #[tokio::test]
    async fn list_mailboxes_returns_six_default_mailboxes() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [42u8; 32];

        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_mailboxes_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: ListMailboxesReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.mailboxes.len(), 6, "six standard mailboxes");
        let names: Vec<&str> = reply.mailboxes.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"INBOX"));
        assert!(names.contains(&"Archive"));
        assert!(names.contains(&"Sent"));
        assert!(names.contains(&"Drafts"));
        assert!(names.contains(&"Trash"));
        assert!(names.contains(&"Junk"));
    }

    #[tokio::test]
    async fn list_mailboxes_shows_correct_exists_and_unseen() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [43u8; 32];

        // Seed mailboxes and place 2 messages: one seen, one unseen.
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        let msg_seen = [10u8; 32];
        let msg_unseen = [11u8; 32];
        state
            .db
            .place_inbound_mail(
                &target,
                &msg_seen,
                "INBOX",
                1_700_000_000,
                "\\Seen",
                "",
                true,
            )
            .await
            .unwrap();
        state
            .db
            .place_inbound_mail(&target, &msg_unseen, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_mailboxes_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: ListMailboxesReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let inbox = reply
            .mailboxes
            .iter()
            .find(|m| m.name == "INBOX")
            .expect("INBOX in reply");
        assert_eq!(inbox.exists, 2);
        assert_eq!(inbox.unseen, 1);
    }

    #[tokio::test]
    async fn list_mailboxes_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_mailboxes_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_mailboxes_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ListMailboxesRequest {
            actor_id: vec![1u8; 16], // wrong length
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_mailboxes_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── select_mailbox tests ────────────────────────────────────────

    #[tokio::test]
    async fn select_mailbox_returns_selected_for_inbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [44u8; 32];

        // Place 2 messages into INBOX: one seen (uid 1), one unseen (uid 2).
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        let msg_seen = [20u8; 32];
        let msg_unseen = [21u8; 32];
        state
            .db
            .place_inbound_mail(
                &target,
                &msg_seen,
                "INBOX",
                1_700_000_000,
                "\\Seen",
                "",
                true,
            )
            .await
            .unwrap();
        state
            .db
            .place_inbound_mail(&target, &msg_unseen, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let req = SelectMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        match reply {
            SelectMailboxReply::Selected {
                exists,
                unseen,
                recent,
                first_unseen_uid,
                ..
            } => {
                assert_eq!(exists, 2);
                assert_eq!(unseen, 1);
                assert_eq!(recent, 0, "recent always 0");
                assert_eq!(first_unseen_uid, Some(2), "uid 2 is unseen");
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn select_mailbox_returns_no_such_mailbox_for_nonexistent() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [45u8; 32];

        let req = SelectMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: "Nonexistent".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state, mda, payload)
            .await
            .expect("handler ok (no-such is a valid reply)");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, SelectMailboxReply::NoSuchMailbox);
    }

    #[tokio::test]
    async fn select_mailbox_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        let req = SelectMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = select_mailbox_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn select_mailbox_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SelectMailboxRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = select_mailbox_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── C.2 handler tests ─────────────────────────────────────────────────────

    /// Seed a message for handler tests that don't need real segment
    /// bytes. Inserts a `segment_records` mirror row + a
    /// `bridge_imap_messages` placement row via raw SQL; no segment
    /// file on disk. Used by list / query / placement-only tests that
    /// only consume metadata.
    ///
    /// For tests that read body bytes through the segment store (T9:
    /// `fetch_message_ciphertext`, `fetch_index_segments_since`), use
    /// [`seed_message_for_handler_with_hint`] which writes a real
    /// envelope via `segments::mail::append_record`.
    async fn seed_message_for_handler(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        msg_id: &[u8; 32],
        mailbox: &str,
        _body: &[u8],
        internal_date: i64,
        flags: &str,
    ) -> u32 {
        let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(*msg_id);
        state
            .db
            .conn()
            .await
            .execute(
                "INSERT OR IGNORE INTO segment_records \
                    (scope_id, kind, segment_id, record_cid, bucket, \
                     tombstoned, \
                     received_at, sender_dom, spam_disp, is_own_submission) \
                 VALUES (?1, 'mail', 1, ?2, '2026-05', 0, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &record_cid.as_bytes()[..],
                    internal_date,
                    "example.com",
                    "accept",
                    0i64,
                ],
            )
            .unwrap();
        let (uid, _modseq) = state
            .db
            .place_inbound_mail(actor, msg_id, mailbox, internal_date, flags, "", true)
            .await
            .unwrap()
            .expect("place_inbound_mail returned None (content_was_new=true)");
        uid
    }

    /// Seed a message that writes a real segment file via the test
    /// state's mail `SegmentManager`. The envelope carries `body` and
    /// `index_hint`; floor metadata carries `internal_date` as both
    /// `received_at` (ms) and `timestamp` (s). Returns `(uid,
    /// msg_id)`; the caller must use the returned `msg_id` for
    /// follow-up reads (it's derived from `(actor, internal_date,
    /// body)`, not chosen by the caller).
    ///
    /// Used by T9 tests that read body bytes or index hints through the
    /// segment store (`fetch_message_ciphertext`,
    /// `fetch_index_segments_since`).
    async fn seed_message_for_handler_with_hint(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        mailbox: &str,
        body: &[u8],
        internal_date: i64,
        flags: &str,
        index_hint: &[u8],
    ) -> (u32, [u8; 32]) {
        let fields = crate::db::bridge_routing::InboundMailFields {
            actor_id: *actor,
            timestamp: internal_date,
            ciphertext_size: body.len() as u32,
            encrypted_body: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                body.to_vec(),
            ),
            encrypted_index_hint:
                fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    index_hint.to_vec(),
                ),
            sender_domain: "example.com".into(),
            spf: "none".into(),
            dkim: "none".into(),
            dmarc: "none".into(),
            dmarc_policy: "none".into(),
            arc: "none".into(),
            spam_score: 0,
            spam_disposition: "accept".into(),
            is_own_submission: false,
            scores: vec![],
            report_hash: vec![],
        };
        seed_fields(state, fields, mailbox, flags).await
    }

    /// [`seed_message_for_handler_with_hint`] with a canonical report-hash on
    /// the floor/mirror (report-sharing.md § Content identity) — the report-
    /// capture tests use this to give several actors copies of "the same"
    /// content.
    async fn seed_message_with_report_hash(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        body: &[u8],
        report_hash: [u8; 32],
    ) -> (u32, [u8; 32]) {
        let fields = crate::db::bridge_routing::InboundMailFields {
            actor_id: *actor,
            timestamp: 1_700_000_000,
            ciphertext_size: body.len() as u32,
            encrypted_body: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                body.to_vec(),
            ),
            encrypted_index_hint:
                fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"hint".to_vec(),
                ),
            sender_domain: "example.com".into(),
            spf: "none".into(),
            dkim: "none".into(),
            dmarc: "none".into(),
            dmarc_policy: "none".into(),
            arc: "none".into(),
            spam_score: 0,
            spam_disposition: "accept".into(),
            is_own_submission: false,
            scores: vec![],
            report_hash: report_hash.to_vec(),
        };
        seed_fields(state, fields, "INBOX", "").await
    }

    /// Shared tail of the seeding helpers: insert + INBOX placement.
    async fn seed_fields(
        state: &crate::routes::AppState,
        fields: crate::db::bridge_routing::InboundMailFields,
        mailbox: &str,
        flags: &str,
    ) -> (u32, [u8; 32]) {
        let internal_date = fields.timestamp;
        let actor = fields.actor_id;
        let msg_id = state
            .db
            .insert_inbound_mail(&state.mail_segments, &fields)
            .await
            .unwrap()
            .message_id;
        let (uid, _modseq) = state
            .db
            .place_inbound_mail(&actor, &msg_id, mailbox, internal_date, flags, "", true)
            .await
            .unwrap()
            .expect("place_inbound_mail returned None");
        (uid, msg_id)
    }

    /// Read a record's real CARv2 block byte-length — what RFC822.SIZE / SEARCH
    /// LARGER|SMALLER / quota now report — for size assertions. Looks the
    /// record's segment up from the mirror and sizes it through the same index
    /// path production uses (`segments::mail::record_size`). Requires a real
    /// segment (seed via [`seed_message_for_handler_with_hint`]).
    async fn real_record_size(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        msg_id: &[u8; 32],
    ) -> u64 {
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(*msg_id);
        let row = state
            .db
            .segment_records_lookup_record(actor, "mail", &cid)
            .await
            .unwrap()
            .expect("mirror row present");
        crate::segments::mail::record_size(&state.mail_segments, actor, row.segment_id, cid)
            .await
            .unwrap()
            .expect("size present (real segment)")
    }

    // Helper to call list_messages_handler and decode the reply.
    async fn call_list_messages(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::ListMessagesRequest,
    ) -> fauna_protocol::bridge_routing::ListMessagesReply {
        use fauna_protocol::bridge_routing::ListMessagesReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_messages_handler()(state, actor_id, payload)
            .await
            .expect("list_messages handler ok");
        fauna_cbor::decode_strict::<ListMessagesReply>(&bytes).unwrap()
    }

    // Helper to call fetch_message_metadata_handler and decode the reply.
    async fn call_fetch_message_metadata(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::FetchMessageMetadataRequest,
    ) -> fauna_protocol::bridge_routing::FetchMessageMetadataReply {
        use fauna_protocol::bridge_routing::FetchMessageMetadataReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_message_metadata_handler()(state, actor_id, payload)
            .await
            .expect("fetch_message_metadata handler ok");
        fauna_cbor::decode_strict::<FetchMessageMetadataReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn list_messages_returns_all_three_ascending() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [50u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[50u8; 32],
            "INBOX",
            b"body1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[51u8; 32],
            "INBOX",
            b"body22",
            1_700_000_002,
            "\\Seen",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[52u8; 32],
            "INBOX",
            b"body333",
            1_700_000_003,
            "\\Seen \\Answered",
        )
        .await;

        let reply = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: None,
                limit: 0,
                after_uid: None,
            },
        )
        .await;

        assert_eq!(reply.messages.len(), 3);
        assert_eq!(reply.messages[0].uid, 1);
        assert_eq!(reply.messages[1].uid, 2);
        assert_eq!(reply.messages[2].uid, 3);
        assert!(reply.messages[0].flags.is_empty());
        assert_eq!(reply.messages[1].flags, vec!["\\Seen"]);
        assert_eq!(reply.messages[2].flags, vec!["\\Seen", "\\Answered"]);
        assert!(!reply.more);
        assert!(reply.expunged_uids.is_empty());
        // highestmodseq should match state row (3 placements from initial 1).
        let state_row = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.highestmodseq, state_row.highestmodseq);
    }

    #[tokio::test]
    async fn list_messages_pagination_limit_and_after_uid() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [51u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[60u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[61u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[62u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        // First page: limit 2.
        let page1 = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: None,
                limit: 2,
                after_uid: None,
            },
        )
        .await;
        assert_eq!(page1.messages.len(), 2);
        assert_eq!(page1.messages[0].uid, 1);
        assert_eq!(page1.messages[1].uid, 2);
        assert!(page1.more, "more=true because there are 3 total");

        // Second page: after_uid=2.
        let page2 = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: None,
                limit: 2,
                after_uid: Some(2),
            },
        )
        .await;
        assert_eq!(page2.messages.len(), 1);
        assert_eq!(page2.messages[0].uid, 3);
        assert!(!page2.more, "no more pages");
    }

    #[tokio::test]
    async fn list_messages_since_modseq_returns_changed_message() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [52u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[70u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[71u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[72u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let old_hms = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        // Simulate a flag bump on uid 2.
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages SET modseq = ?1 WHERE actor_id = ?2 AND uid = 2",
                rusqlite::params![old_hms + 1, &target[..]],
            )
            .unwrap();

        let reply = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: Some(old_hms),
                limit: 0,
                after_uid: None,
            },
        )
        .await;

        assert_eq!(reply.messages.len(), 1);
        assert_eq!(reply.messages[0].uid, 2);
    }

    #[tokio::test]
    async fn list_messages_expunged_uids_populated_when_since_modseq_set() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [53u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();

        let old_hms: i64 = 5;
        // Insert an expunged row with modseq > old_hms.
        state
            .db
            .conn()
            .await
            .execute(
                "INSERT INTO bridge_imap_expunged \
                 (actor_id, mailbox, uid, modseq, expunged_at) \
                 VALUES (?1, 'INBOX', 99, 10, 1700000001)",
                rusqlite::params![&target[..]],
            )
            .unwrap();

        let reply = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: Some(old_hms),
                limit: 0,
                after_uid: None,
            },
        )
        .await;

        assert_eq!(reply.expunged_uids, vec![99u32]);
    }

    #[tokio::test]
    async fn list_messages_empty_mailbox_returns_empty_no_error() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [54u8; 32];

        let reply = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "Nonexistent".into(),
                since_modseq: None,
                limit: 0,
                after_uid: None,
            },
        )
        .await;

        assert!(reply.messages.is_empty());
        assert!(!reply.more);
        assert_eq!(reply.highestmodseq, 1);
    }

    #[tokio::test]
    async fn fetch_message_metadata_returns_specific_uids() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [55u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[80u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[81u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "\\Seen",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[82u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let reply = call_fetch_message_metadata(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchMessageMetadataRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1, 3],
            },
        )
        .await;

        assert_eq!(reply.messages.len(), 2);
        assert_eq!(reply.messages[0].uid, 1);
        assert_eq!(reply.messages[1].uid, 3);
        // F1: each row carries its rank in the FULL 3-message mailbox, so a
        // UID-subset fetch emits correct RFC 9051 seqNums (uid 3 → seq 3, not
        // 2), and mailbox_total is the whole-mailbox count regardless of the
        // UID filter — the two facts the Go bridge needs to stop re-fetching
        // the whole mailbox for FETCH (per-row seq) and IDLE (EXISTS total).
        assert_eq!(reply.messages[0].seq_num, 1);
        assert_eq!(reply.messages[1].seq_num, 3);
        assert_eq!(reply.mailbox_total, 3);
    }

    #[tokio::test]
    async fn fetch_message_metadata_empty_uids_returns_all() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [56u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[90u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[91u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[92u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let reply = call_fetch_message_metadata(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchMessageMetadataRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await;

        assert_eq!(reply.messages.len(), 3, "empty uids = all messages");
    }

    #[tokio::test]
    async fn list_messages_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [57u8; 32];

        let req = fauna_protocol::bridge_routing::ListMessagesRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            since_modseq: None,
            limit: 0,
            after_uid: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_messages_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_message_metadata_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [58u8; 32];

        let req = fauna_protocol::bridge_routing::FetchMessageMetadataRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_metadata_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_messages_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::ListMessagesRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: "INBOX".into(),
            since_modseq: None,
            limit: 0,
            after_uid: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_messages_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_message_metadata_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::FetchMessageMetadataRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_metadata_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── C.3 handler tests ─────────────────────────────────────────────────────

    // Helper to call fetch_message_ciphertext_handler and decode as Found/NotFound.
    async fn call_fetch_message_ciphertext(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::FetchMessageCiphertextRequest,
    ) -> Result<fauna_protocol::bridge_routing::FetchMessageCiphertextReply, fauna_protocol::RpcError>
    {
        use fauna_protocol::bridge_routing::FetchMessageCiphertextReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_message_ciphertext_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<FetchMessageCiphertextReply>(&bytes).unwrap())
    }

    // Helper to call fetch_index_segments_since_handler and decode.
    async fn call_fetch_index_segments_since(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest,
    ) -> fauna_protocol::bridge_routing::FetchIndexSegmentsSinceReply {
        use fauna_protocol::bridge_routing::FetchIndexSegmentsSinceReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_index_segments_since_handler()(state, actor_id, payload)
            .await
            .expect("fetch_index_segments_since handler ok");
        fauna_cbor::decode_strict::<FetchIndexSegmentsSinceReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_found() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [100u8; 32];
        let body = b"encrypted-body-content";

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // Real segment file write (msg_id is derived from the fields, not chosen).
        let (_uid, msg_id) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            body,
            1_700_000_042,
            "",
            b"hint",
        )
        .await;

        let reply = call_fetch_message_ciphertext(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
                actor_id: target.to_vec(),
                message_id: msg_id.to_vec(),
            },
        )
        .await
        .unwrap();

        match reply {
            fauna_protocol::bridge_routing::FetchMessageCiphertextReply::Found {
                encrypted_body,
                ciphertext_size,
                internal_date,
                body_ref,
                stored_at,
            } => {
                // A small body still rides the reply inline — no staging, no reference.
                assert_eq!(body_ref, None);
                assert_eq!(encrypted_body, body);
                assert_eq!(ciphertext_size, body.len() as u32);
                assert_eq!(internal_date, 1_700_000_042);
                // The epoch classification basis is served:
                // append-instant seconds, distinct from the fixture's historical
                // internal_date, and never 0.
                assert!(
                    stored_at > 1_700_000_042,
                    "stored_at must be the append instant, not the message's own timestamp"
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_wrong_actor_returns_not_found() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target_a = [102u8; 32];
        let target_b = [103u8; 32];
        let msg_id = [104u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target_a)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target_a,
            &msg_id,
            "INBOX",
            b"body-a",
            1_700_000_001,
            "",
        )
        .await;

        // Call with target_b's actor_id — must return NotFound, not a permission error.
        let reply = call_fetch_message_ciphertext(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
                actor_id: target_b.to_vec(), // wrong actor
                message_id: msg_id.to_vec(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::FetchMessageCiphertextReply::NotFound,
            "cross-actor lookup must return NotFound (not error)"
        );
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_unknown_id_returns_not_found() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [105u8; 32];
        let unknown_msg = [0xffu8; 32]; // never seeded

        let reply = call_fetch_message_ciphertext(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
                actor_id: target.to_vec(),
                message_id: unknown_msg.to_vec(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::FetchMessageCiphertextReply::NotFound
        );
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
            actor_id: vec![1u8; 16], // wrong length
            message_id: vec![0u8; 32],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_malformed_message_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
            actor_id: vec![1u8; 32],
            message_id: vec![0u8; 16], // wrong length
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = fauna_protocol::bridge_routing::FetchMessageCiphertextRequest {
            actor_id: vec![5u8; 32],
            message_id: vec![6u8; 32],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_ciphertext_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_index_segments_since_all_mailboxes_no_limit() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [110u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"hint-1",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"hint-2",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "Junk",
            b"bj",
            1_700_000_003,
            "",
            b"hint-j",
        )
        .await;

        // Get the current highestmodseq for assertion.
        let inbox_hms = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;
        let junk_hms = state
            .db
            .get_bridge_imap_mailbox_state(&target, "Junk")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;
        let expected_hms = inbox_hms.max(junk_hms);

        let reply = call_fetch_index_segments_since(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
                actor_id: target.to_vec(),
                mailbox: None,
                since_modseq: 0,
                limit: 0,
            },
        )
        .await;

        assert_eq!(
            reply.segments.len(),
            3,
            "all 3 segments across all mailboxes"
        );
        assert!(!reply.more);
        assert_eq!(reply.highestmodseq, expected_hms);

        // Check ascending modseq order.
        let modseqs: Vec<i64> = reply.segments.iter().map(|s| s.modseq).collect();
        let mut sorted = modseqs.clone();
        sorted.sort();
        assert_eq!(modseqs, sorted, "segments must be ascending modseq");

        // Verify all 3 hints are present.
        let hints: Vec<&[u8]> = reply
            .segments
            .iter()
            .map(|s| s.encrypted_index_hint.as_slice())
            .collect();
        assert!(hints.contains(&b"hint-1".as_slice()));
        assert!(hints.contains(&b"hint-2".as_slice()));
        assert!(hints.contains(&b"hint-j".as_slice()));
    }

    #[tokio::test]
    async fn fetch_index_segments_since_mailbox_filter() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [111u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"inbox-hint",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "Junk",
            b"bj",
            1_700_000_002,
            "",
            b"junk-hint",
        )
        .await;

        let reply = call_fetch_index_segments_since(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
                actor_id: target.to_vec(),
                mailbox: Some("INBOX".into()),
                since_modseq: 0,
                limit: 0,
            },
        )
        .await;

        assert_eq!(reply.segments.len(), 1, "only INBOX message");
        assert_eq!(reply.segments[0].mailbox, "INBOX");
        assert_eq!(reply.segments[0].encrypted_index_hint, b"inbox-hint");
        // The epoch classification basis is served per segment: append-instant seconds, distinct from the fixture's
        // historical internal_date.
        let stored_at = reply.segments[0].stored_at;
        assert!(
            stored_at > 1_700_000_002,
            "stored_at must be the append instant, not the message's own timestamp"
        );
    }

    #[tokio::test]
    async fn fetch_index_segments_since_modseq_cutoff() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [112u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"h1",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"h2",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
            b"h3",
        )
        .await;

        // modseqs after 3 placements: 2, 3, 4. since_modseq=3 → only modseq=4.
        let reply = call_fetch_index_segments_since(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
                actor_id: target.to_vec(),
                mailbox: Some("INBOX".into()),
                since_modseq: 3,
                limit: 0,
            },
        )
        .await;

        assert_eq!(reply.segments.len(), 1);
        assert_eq!(reply.segments[0].encrypted_index_hint, b"h3");
    }

    #[tokio::test]
    async fn fetch_index_segments_since_limit_and_more_flag() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [113u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"h1",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"h2",
        )
        .await;
        seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
            b"h3",
        )
        .await;

        // limit=2: should get 2 results + more=true.
        let reply = call_fetch_index_segments_since(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
                actor_id: target.to_vec(),
                mailbox: Some("INBOX".into()),
                since_modseq: 0,
                limit: 2,
            },
        )
        .await;

        assert_eq!(reply.segments.len(), 2, "limit 2 returns exactly 2");
        assert!(reply.more, "more=true because there's a third message");
        assert_eq!(reply.segments[0].encrypted_index_hint, b"h1");
        assert_eq!(reply.segments[1].encrypted_index_hint, b"h2");
    }

    #[tokio::test]
    async fn fetch_index_segments_since_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: None,
            since_modseq: 0,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_index_segments_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── C.4 handler tests ─────────────────────────────────────────────────────

    /// Helper to call store_flags_handler and decode the reply.
    async fn call_store_flags(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::StoreFlagsRequest,
    ) -> Result<fauna_protocol::bridge_routing::StoreFlagsReply, fauna_protocol::RpcError> {
        use fauna_protocol::bridge_routing::StoreFlagsReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = store_flags_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<StoreFlagsReply>(&bytes).unwrap())
    }

    /// Helper to call expunge_handler and decode the reply.
    async fn call_expunge(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::ExpungeRequest,
    ) -> Result<fauna_protocol::bridge_routing::ExpungeReply, fauna_protocol::RpcError> {
        use fauna_protocol::bridge_routing::ExpungeReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = expunge_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<ExpungeReply>(&bytes).unwrap())
    }

    #[tokio::test]
    async fn store_flags_set_updates_two_messages() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [120u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[120u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[121u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[122u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        // Set \Seen on uids 1 and 2.
        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1, 2],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store_flags ok");

        assert_eq!(reply.updated.len(), 2);
        assert_eq!(reply.updated[0].uid, 1);
        assert_eq!(reply.updated[0].flags, vec!["\\Seen"]);
        assert_eq!(reply.updated[1].uid, 2);
        assert_eq!(reply.updated[1].flags, vec!["\\Seen"]);
        assert_eq!(
            reply.updated[0].modseq, reply.updated[1].modseq,
            "shared modseq"
        );
        assert_eq!(reply.highestmodseq, reply.updated[0].modseq);
    }

    /// `fauna.bridges.store_flags` asserts `forbid_replay = false`, which
    /// `transport.md` § Idempotency and reconnect-with-resume defines as "the
    /// handler itself is naturally idempotent" — the per-connection idempotency
    /// cache cannot help, because `request_auto_retry` always re-issues on a
    /// *fresh* connection. This pins the assertion: the identical STORE, sent
    /// twice, answers byte-identically and leaves HIGHESTMODSEQ where the first
    /// call left it.
    ///
    /// Red before the 82nd-pass fix: the second call re-stamped the row, so it
    /// returned `modseq = n + 1` and bumped `highestmodseq` again.
    #[tokio::test]
    async fn replayed_store_flags_is_byte_identical_and_does_not_bump_modseq() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [123u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[140u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        let req = fauna_protocol::bridge_routing::StoreFlagsRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![1],
            op: fauna_protocol::bridge_routing::StoreFlagsOp::Add,
            flags: vec!["\\Seen".into()],
            ..Default::default()
        };

        let first = call_store_flags(state.clone(), mda, req.clone())
            .await
            .expect("first store ok");
        assert_eq!(first.updated.len(), 1);
        assert_eq!(first.updated[0].flags, vec!["\\Seen"]);

        let replay = call_store_flags(state.clone(), mda, req)
            .await
            .expect("replayed store ok");

        assert_eq!(
            replay.updated, first.updated,
            "a replayed STORE must answer byte-identically — the UID still holds \
             the requested flags, at the modseq the first call stamped"
        );
        assert_eq!(
            replay.highestmodseq, first.highestmodseq,
            "a no-op STORE must not advance HIGHESTMODSEQ (it would wake every \
             IDLE subscriber and re-report the message to QRESYNC clients)"
        );
        assert!(replay.modified.is_empty(), "nothing was refused");
    }

    /// The sharper half of the same defect: with CONDSTORE `UNCHANGEDSINCE`, the
    /// first call's own modseq bump used to push the row past the supplied
    /// value, so the identical re-issue was answered `MODIFIED` — RFC 7162's
    /// "someone else changed this message, your conditional store was refused".
    /// That is a *false* conflict report, not merely a diverging answer, which is
    /// why the fix tests the already-satisfied branch before the gate.
    #[tokio::test]
    async fn replayed_conditional_store_does_not_report_a_false_conflict() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [124u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[141u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        // The modseq the client last saw, i.e. what it would put in
        // `STORE (UNCHANGEDSINCE n)`.
        let seen_modseq = state
            .db
            .max_highestmodseq_for_actor(&target, Some("INBOX"))
            .await
            .unwrap();

        let req = fauna_protocol::bridge_routing::StoreFlagsRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![1],
            op: fauna_protocol::bridge_routing::StoreFlagsOp::Add,
            flags: vec!["\\Answered".into()],
            // Every field is named here, so no `..Default::default()` — it
            // would be a no-op and `clippy::needless_update` is deny-level.
            unchanged_since: Some(seen_modseq),
        };

        let first = call_store_flags(state.clone(), mda, req.clone())
            .await
            .expect("first conditional store ok");
        assert_eq!(first.updated.len(), 1, "the conditional store applied");
        assert!(first.modified.is_empty());

        let replay = call_store_flags(state.clone(), mda, req)
            .await
            .expect("replayed conditional store ok");

        assert!(
            replay.modified.is_empty(),
            "the replay must NOT be reported as a conflict — the only thing that \
             advanced this message's modseq was the caller's own first call"
        );
        assert_eq!(
            replay.updated, first.updated,
            "and it must still answer with the settled state"
        );
    }

    #[tokio::test]
    async fn store_flags_add_keeps_existing_flags() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [121u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[130u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[131u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        // Set \Seen first.
        call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Add \Flagged → should have \Flagged \Seen (BTreeSet order).
        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Add,
                flags: vec!["\\Flagged".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(reply.updated.len(), 1);
        assert_eq!(reply.updated[0].flags, vec!["\\Flagged", "\\Seen"]);
    }

    #[tokio::test]
    async fn store_flags_remove_drops_flag() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [122u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[140u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        // Set \Flagged and \Seen.
        call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Flagged".into(), "\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Remove \Seen.
        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Remove,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(reply.updated.len(), 1);
        assert_eq!(reply.updated[0].flags, vec!["\\Flagged"]);
    }

    // ── D.2 CONDSTORE UNCHANGEDSINCE precondition tests ──────────────────────

    /// `STORE UNCHANGEDSINCE n` with a mix of fresh and stale UIDs:
    /// the fresh subset (modseq ≤ n) is applied; the stale subset
    /// (modseq > n) lands in `reply.modified`. Per imap-server.md
    /// § CONDSTORE UNCHANGEDSINCE: "the nest RPC checks every UID's
    /// modseq against the supplied value inside the transaction, and
    /// applies only the unchanged subset; the response reports both
    /// the applied and rejected partitions in one round-trip".
    #[tokio::test]
    async fn store_flags_honors_unchanged_since_strict() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [180u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // Seed two messages; both get modseq=1 from the initial place.
        seed_message_for_handler(
            &state,
            &target,
            &[200u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[201u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        // Bump uid 2's modseq to 5 (simulating a prior STORE/COPY/MOVE)
        // while the mailbox's highestmodseq advances to 5 as well.
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages SET modseq = 5 \
                 WHERE actor_id = ?1 AND uid = 2",
                rusqlite::params![&target[..]],
            )
            .unwrap();
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_mailbox_state SET highestmodseq = 5 \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&target[..]],
            )
            .unwrap();

        // STORE +FLAGS (\Seen) UNCHANGEDSINCE 3 → uid 1 applied, uid 2 rejected.
        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1, 2],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Add,
                flags: vec!["\\Seen".into()],
                unchanged_since: Some(3),
            },
        )
        .await
        .expect("store_flags ok");

        assert_eq!(reply.updated.len(), 1, "only uid 1 should be applied");
        assert_eq!(reply.updated[0].uid, 1);
        assert_eq!(reply.updated[0].flags, vec!["\\Seen"]);
        assert_eq!(
            reply.modified,
            vec![2u32],
            "uid 2 must be reported MODIFIED"
        );
        // highestmodseq bumped exactly once for the applied partition.
        assert!(
            reply.highestmodseq > 5,
            "highestmodseq should advance past the pre-test value 5"
        );
    }

    /// `UNCHANGEDSINCE 0` against a mailbox where every UID's modseq
    /// is ≥ 1: nothing applies, every UID reported in `modified`,
    /// `updated` is empty. Verifies the "all-stale" edge case.
    #[tokio::test]
    async fn store_flags_unchanged_since_all_stale() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [181u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[210u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[211u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        let pre = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1, 2],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Add,
                flags: vec!["\\Seen".into()],
                unchanged_since: Some(0),
            },
        )
        .await
        .expect("store_flags ok");

        assert!(reply.updated.is_empty(), "no UID should be applied");
        assert_eq!(reply.modified, vec![1u32, 2u32]);
        // No applied rows → no modseq bump.
        assert_eq!(reply.highestmodseq, pre);
    }

    /// `unchanged_since: None` is a plain IMAP STORE without UNCHANGEDSINCE:
    /// every UID is updated, `modified` is empty.
    #[tokio::test]
    async fn store_flags_unchanged_since_none_means_unconditional() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [182u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[220u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        // Bump uid 1 to modseq=99 to prove unchanged_since=None ignores it.
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages SET modseq = 99 \
                 WHERE actor_id = ?1 AND uid = 1",
                rusqlite::params![&target[..]],
            )
            .unwrap();
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_mailbox_state SET highestmodseq = 99 \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&target[..]],
            )
            .unwrap();

        let reply = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Seen".into()],
                unchanged_since: None,
            },
        )
        .await
        .expect("store_flags ok");

        assert_eq!(reply.updated.len(), 1);
        assert_eq!(reply.updated[0].uid, 1);
        assert!(
            reply.modified.is_empty(),
            "modified must stay empty when unchanged_since is None"
        );
    }

    #[tokio::test]
    async fn store_flags_empty_uids_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [123u8; 32];

        let err = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![], // empty → malformed
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn store_flags_recent_flag_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [124u8; 32];

        let err = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Recent".into()], // malformed
                ..Default::default()
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn store_flags_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [125u8; 32];

        let req = fauna_protocol::bridge_routing::StoreFlagsRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![1],
            op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
            flags: vec!["\\Seen".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = store_flags_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn store_flags_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::StoreFlagsRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: "INBOX".into(),
            uids: vec![1],
            op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
            flags: vec!["\\Seen".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = store_flags_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn expunge_removes_deleted_messages() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [126u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[150u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[151u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[152u8; 32],
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        // Set \Deleted on uids 1 and 3 via store_flags.
        call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1, 3],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Deleted".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let hms_before = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        // Plain expunge (empty uids).
        let reply = call_expunge(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await
        .expect("expunge ok");

        assert_eq!(reply.expunged_uids, vec![1, 3], "uids 1 and 3 expunged");
        assert!(reply.highestmodseq > hms_before, "modseq bumped");
    }

    #[tokio::test]
    async fn expunge_uid_not_deleted_returns_empty() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [127u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[160u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[161u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        // uid 2 has no \Deleted.
        let hms_before = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        let reply = call_expunge(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![2],
            },
        )
        .await
        .unwrap();

        assert!(
            reply.expunged_uids.is_empty(),
            "uid 2 not \\Deleted => nothing expunged"
        );
        assert_eq!(reply.highestmodseq, hms_before, "modseq unchanged");
    }

    #[tokio::test]
    async fn expunge_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [128u8; 32];

        let req = fauna_protocol::bridge_routing::ExpungeRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = expunge_handler()(state, mta, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn expunge_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::ExpungeRequest {
            actor_id: vec![1u8; 16], // wrong length
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = expunge_handler()(state, mda, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_index_segments_since_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = fauna_protocol::bridge_routing::FetchIndexSegmentsSinceRequest {
            actor_id: vec![5u8; 32],
            mailbox: None,
            since_modseq: 0,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_index_segments_since_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── C.5 handler tests ─────────────────────────────────────────────────────

    /// Helper to call copy_messages_handler and decode the reply.
    async fn call_copy_messages(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::CopyMessagesRequest,
    ) -> Result<fauna_protocol::bridge_routing::CopyMessagesReply, fauna_protocol::RpcError> {
        use fauna_protocol::bridge_routing::CopyMessagesReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = copy_messages_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<CopyMessagesReply>(&bytes).unwrap())
    }

    /// Helper to call move_messages_handler and decode the reply.
    async fn call_move_messages(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::MoveMessagesRequest,
    ) -> Result<fauna_protocol::bridge_routing::MoveMessagesReply, fauna_protocol::RpcError> {
        use fauna_protocol::bridge_routing::MoveMessagesReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = move_messages_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<MoveMessagesReply>(&bytes).unwrap())
    }

    #[tokio::test]
    async fn copy_handler_copies_two_messages() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [200u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[200u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[201u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        let reply = call_copy_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CopyMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![1, 2],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect("copy handler ok");

        assert_eq!(reply.copied.len(), 2);
        assert_eq!(reply.copied[0].source_uid, 1);
        assert_eq!(reply.copied[0].dest_uid, 1);
        assert_eq!(reply.copied[1].source_uid, 2);
        assert_eq!(reply.copied[1].dest_uid, 2);

        // INBOX still has 2 messages.
        let inbox_rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(inbox_rows.len(), 2, "INBOX untouched");

        // Archive has 2 messages.
        let archive_rows = state
            .db
            .list_bridge_imap_messages(&target, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 2, "Archive populated");
    }

    /// Hazard pin for `fauna.bridges.copy`'s `forbid_replay = true` (82nd-pass
    /// audit). The flag is a declaration, so what a test can pin is the hazard
    /// it declares: re-issuing the identical COPY — which is exactly what
    /// `request_auto_retry` would do after a reconnect, since the per-connection
    /// idempotency cache cannot span one (`transport.md` § Idempotency and
    /// reconnect-with-resume) — stores a *second* placement per source UID under
    /// freshly-allocated dest UIDs. Nothing in the request identifies the copy,
    /// so no server-side dedup is reachable; the client must not auto-retry.
    ///
    /// If this test ever goes green with `Archive == 2`, COPY has become
    /// naturally idempotent and the flag should be re-examined.
    #[tokio::test]
    async fn replayed_copy_duplicates_the_message_hazard_pin() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [202u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[202u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        let req = fauna_protocol::bridge_routing::CopyMessagesRequest {
            actor_id: target.to_vec(),
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };

        let first = call_copy_messages(state.clone(), mda, req.clone())
            .await
            .expect("first copy ok");
        assert_eq!(first.copied.len(), 1);
        assert_eq!(first.copied[0].dest_uid, 1);

        // The replay: same actor, same source UID, same destination.
        let replay = call_copy_messages(state.clone(), mda, req)
            .await
            .expect("replayed copy is accepted — that is the hazard");
        assert_eq!(replay.copied.len(), 1);
        assert_eq!(
            replay.copied[0].dest_uid, 2,
            "the replay allocates a FRESH dest uid — nothing keys the copy"
        );

        let archive_rows = state
            .db
            .list_bridge_imap_messages(&target, "Archive")
            .await
            .unwrap();
        assert_eq!(
            archive_rows.len(),
            2,
            "a replayed COPY duplicates the mail — this is why the kind is forbid-replay"
        );
    }

    /// The other half of the C.5 pair: `fauna.bridges.move` keeps
    /// `forbid_replay = false` because MOVE consumes the very source rows its
    /// copy step reads, so a replay is a clean no-op rather than a duplicate.
    /// The reply does diverge (`moved: []`) — the 76th pass's consume-shaped
    /// class, ruled not grounds for a flip.
    #[tokio::test]
    async fn replayed_move_is_a_no_op_and_does_not_duplicate() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [203u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[203u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;

        let req = fauna_protocol::bridge_routing::MoveMessagesRequest {
            actor_id: target.to_vec(),
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };

        let first = call_move_messages(state.clone(), mda, req.clone())
            .await
            .expect("first move ok");
        assert_eq!(first.moved.len(), 1);

        let replay = call_move_messages(state.clone(), mda, req)
            .await
            .expect("replayed move ok");
        assert!(
            replay.moved.is_empty(),
            "the source rows are gone, so the copy step finds nothing"
        );

        let archive_rows = state
            .db
            .list_bridge_imap_messages(&target, "Archive")
            .await
            .unwrap();
        assert_eq!(
            archive_rows.len(),
            1,
            "MOVE converges: the replay must not duplicate"
        );
        let inbox_rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert!(inbox_rows.is_empty(), "source stays expunged");
    }

    #[tokio::test]
    async fn copy_over_storage_quota_returns_over_quota() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [224u8; 32];

        // One real message in INBOX (uid 1). COPY duplicates the placement,
        // re-counting its bytes, so projected usage is 2× its size; a ceiling of
        // size + 5 leaves no room for the copy → over_quota.
        let used = seed_one_real_record(&state, &target, &[b'x'; 500]).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(used + 5),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = call_copy_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CopyMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![1],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect_err("copy that would exceed the quota root must be rejected");
        assert_eq!(err.code, "fauna.bridges.over_quota");

        // Pre-check fires before apply_copy → nothing was copied.
        let archive_rows = state
            .db
            .list_bridge_imap_messages(&target, "Archive")
            .await
            .unwrap();
        assert!(
            archive_rows.is_empty(),
            "rejected COPY must not write Archive"
        );
    }

    #[tokio::test]
    async fn move_over_storage_quota_returns_over_quota_and_keeps_source() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [225u8; 32];

        // Same single real INBOX message. MOVE pre-checks the copy half like
        // COPY (the source is still counted pre-transaction), so 2× its size
        // trips a `size + 5` ceiling and the move never runs — source stays
        // (doc: "the source rows stay (the transaction rolls back)").
        let used = seed_one_real_record(&state, &target, &[b'x'; 500]).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(used + 5),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = call_move_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![1],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect_err("move that would exceed the quota root must be rejected");
        assert_eq!(err.code, "fauna.bridges.over_quota");

        let inbox_rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(inbox_rows.len(), 1, "rejected MOVE leaves the source row");
    }

    #[tokio::test]
    async fn copy_handler_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [202u8; 32];

        let req = fauna_protocol::bridge_routing::CopyMessagesRequest {
            actor_id: target.to_vec(),
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = copy_messages_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn copy_handler_empty_uids_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [203u8; 32];

        let err = call_copy_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CopyMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![], // empty → malformed
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn copy_handler_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::CopyMessagesRequest {
            actor_id: vec![1u8; 16], // wrong length
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = copy_messages_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn move_handler_moves_one_message() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [210u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_handler(
            &state,
            &target,
            &[210u8; 32],
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message_for_handler(
            &state,
            &target,
            &[211u8; 32],
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        let inbox_hms_before = state
            .db
            .get_bridge_imap_mailbox_state(&target, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        let reply = call_move_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![1],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect("move handler ok");

        assert_eq!(reply.moved.len(), 1);
        assert_eq!(reply.moved[0].source_uid, 1);
        assert_eq!(reply.moved[0].dest_uid, 1);
        assert!(
            reply.source_highestmodseq > inbox_hms_before,
            "source modseq bumped"
        );

        // INBOX uid 1 gone, uid 2 still there.
        let inbox_rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(inbox_rows.len(), 1);
        assert_eq!(inbox_rows[0].0, 2, "uid 2 remains in INBOX");

        // Archive has the moved message.
        let archive_rows = state
            .db
            .list_bridge_imap_messages(&target, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 1, "Archive has one row");

        // Expunged uid 1 is visible via list_messages with since_modseq.
        let list_reply = call_list_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ListMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                since_modseq: Some(inbox_hms_before),
                limit: 0,
                after_uid: None,
            },
        )
        .await;
        assert!(
            list_reply.expunged_uids.contains(&1),
            "uid 1 appears in expunged_uids after move"
        );
    }

    #[tokio::test]
    async fn move_handler_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [212u8; 32];

        let req = fauna_protocol::bridge_routing::MoveMessagesRequest {
            actor_id: target.to_vec(),
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = move_messages_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn move_handler_empty_uids_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [213u8; 32];

        let err = call_move_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![], // empty → malformed
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn move_handler_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = fauna_protocol::bridge_routing::MoveMessagesRequest {
            actor_id: vec![1u8; 16], // wrong length
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = move_messages_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── C.6 handler tests ─────────────────────────────────────────────────────

    /// Helper to call append_message_handler and decode the reply.
    async fn call_append_message(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::AppendMessageRequest,
    ) -> Result<fauna_protocol::bridge_routing::AppendMessageReply, fauna_protocol::RpcError> {
        use fauna_protocol::bridge_routing::AppendMessageReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = append_message_handler()(state, actor_id, payload).await?;
        Ok(fauna_cbor::decode_strict::<AppendMessageReply>(&bytes).unwrap())
    }

    /// Seal `plaintext` as a genuine recipient envelope — the shape the MDA
    /// produces before every APPEND. S6.12b makes `append_message_handler`
    /// refuse anything the wire-edge `SealedRecordBytes::verify` rejects, so
    /// success-path test bodies/hints must be real seals, not byte literals.
    /// NOT deterministic (HPKE encapsulation is randomized) — seal once and
    /// reuse the returned bytes when a test needs the same body twice.
    fn sealed(plaintext: &[u8]) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};
        let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
        seal_to_recipient(plaintext, &pubkey)
            .expect("seal test fixture")
            .to_canonical_bytes()
            .expect("canonical test fixture")
    }

    fn sample_append_req(
        target: &[u8; 32],
        mailbox: &str,
        flags: Vec<String>,
    ) -> fauna_protocol::bridge_routing::AppendMessageRequest {
        let body = sealed(b"encrypted-draft-body");
        let body_len = body.len() as u32;
        fauna_protocol::bridge_routing::AppendMessageRequest {
            actor_id: target.to_vec(),
            mailbox: mailbox.into(),
            flags,
            encrypted_body: body,
            encrypted_index_hint: sealed(b"encrypted-hint"),
            timestamp: 1_700_000_000,
            ciphertext_size: body_len,
            sender_domain: String::new(),
            // What the MDA does: the pair of the literal it is about to seal.
            dedup_key: "env:v1:encrypted-draft-body".into(),
            envelope_key: "env:v1:encrypted-draft-body".into(),
            ..Default::default()
        }
    }

    /// `mailbox-migration.md` § Dedup key persistence — a regular APPEND
    /// populates `actor_message_dedup`, so a later import dedup-hits mail the
    /// user's MUA filed here (the Track-B half of the two pre-existing write
    /// paths).
    #[tokio::test]
    async fn append_with_dedup_key_populates_the_index() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [221u8; 32];

        let key = "msgid:v1:a@example.com";
        let mut req = sample_append_req(&target, "INBOX", vec![]);
        req.dedup_key = key.into();
        req.envelope_key = "env:v1:append".into();

        let reply = call_append_message(state.clone(), mda, req)
            .await
            .expect("append handler ok");

        assert!(
            state.db.has_dedup_key(&target, key).await.unwrap(),
            "APPEND must record its dedup key"
        );
        // …and the envelope key beside it (§ The envelope key confirms a
        // Message-ID hit), so a later import's hit on this row is confirmed.
        assert_eq!(
            state.db.dedup_envelope_key(&target, key).await.unwrap(),
            Some("env:v1:append".to_string())
        );
        // The index points at the stored message, hex-encoded — same
        // `message_uri` shape the import path writes.
        assert_eq!(reply.message_id.len(), 32);

        // Scope is per-actor: another actor's index is untouched.
        let other = [222u8; 32];
        assert!(
            !state.db.has_dedup_key(&other, key).await.unwrap(),
            "dedup index must not leak across actors"
        );
    }

    /// A dedup hit must never suppress an APPEND — that is the import path's
    /// contract alone. Storing the same key twice keeps both messages and
    /// leaves the first writer's `message_uri` in place (INSERT OR IGNORE).
    #[tokio::test]
    async fn append_is_never_skipped_on_a_dedup_hit() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [223u8; 32];

        let key = "msgid:v1:dup@example.com";
        let mut first = sample_append_req(&target, "INBOX", vec![]);
        first.dedup_key = key.into();
        let r1 = call_append_message(state.clone(), mda, first)
            .await
            .expect("first append ok");

        // Same key, genuinely different message (distinct body → distinct id).
        let mut second = sample_append_req(&target, "INBOX", vec![]);
        second.dedup_key = key.into();
        second.encrypted_body = sealed(b"a different encrypted body");
        second.ciphertext_size = second.encrypted_body.len() as u32;
        let r2 = call_append_message(state.clone(), mda, second)
            .await
            .expect("second append must NOT be rejected or skipped");

        assert_ne!(r1.message_id, r2.message_id, "two distinct messages stored");
        assert_ne!(r1.uid, r2.uid, "both got their own uid");
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "a dedup hit must not drop the second message"
        );
    }

    /// Exactly-one-of guard (the MDA-APPEND reference leg, mirroring
    /// `persist_inbound_mail_request`): an APPEND carrying BOTH an inline
    /// `encrypted_body` and a `body_ref` is malformed — the sealed body must ride
    /// exactly one transport.
    #[tokio::test]
    async fn append_with_both_inline_body_and_body_ref_is_rejected() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [231u8; 32];

        // sample_append_req sets a non-empty inline body; adding a body_ref makes
        // both present, which the exactly-one-of guard rejects before any resolve.
        let mut req = sample_append_req(&target, "INBOX", vec![]);
        req.body_ref = Some(fauna_protocol::bridge_routing::MailBodyRef {
            chunk_hashes: vec![serde_bytes::ByteBuf::from(vec![0xABu8; 32])],
            total_bytes: 4_000_000,
        });
        let err = call_append_message(state.clone(), mda, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// An APPEND that carries neither an inline body nor a `body_ref` is
    /// malformed — the "empty on both" case that stops a version-skewed MDA (a
    /// new MDA staging a reference at an older nest that dropped the unknown key)
    /// from ever storing an empty message. Mirrors the ingest guard.
    #[tokio::test]
    async fn append_with_neither_inline_body_nor_body_ref_is_rejected() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [232u8; 32];

        let mut req = sample_append_req(&target, "INBOX", vec![]);
        req.encrypted_body = Vec::new();
        req.ciphertext_size = 0;
        req.body_ref = None;
        let err = call_append_message(state.clone(), mda, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// Message-size ceiling (`smtp-server.md` § Message size limits). Since
    /// ceiling retirement IMAP APPEND's authoritative size gate is nest — it has
    /// no SMTP perimeter clamp of its own. A declared `ciphertext_size` over
    /// `max_message_bytes` (plus the seal allowance) is refused with the shared
    /// typed `message_too_large` the MDA maps to an IMAP `BAD`, checked on the
    /// declared size before any body_ref resolve so an over-ceiling APPEND never
    /// forces nest to read the staged body.
    #[tokio::test]
    async fn append_over_the_product_ceiling_is_refused_message_too_large() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [233u8; 32];

        // Just over the shipped 50 MB default ceiling + the 64 KiB seal allowance.
        // The guard reads the declared field and fires before body resolution, so
        // the fixture body stays tiny (no 50 MB seal needed).
        let mut req = sample_append_req(&target, "INBOX", vec![]);
        req.ciphertext_size = 50_000_000 + 64 * 1024 + 1;
        let err = call_append_message(state.clone(), mda, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, fauna_protocol::email::MESSAGE_TOO_LARGE_CODE);
    }

    /// There is no absent key: an APPEND with an empty half of the pair is
    /// refused before anything is stored, rather than filed unindexed.
    #[tokio::test]
    async fn append_with_an_empty_key_is_refused() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [224u8; 32];

        for strip_dedup in [true, false] {
            let mut req = sample_append_req(&target, "INBOX", vec![]);
            if strip_dedup {
                req.dedup_key.clear();
            } else {
                req.envelope_key.clear();
            }
            let err = call_append_message(state.clone(), mda, req)
                .await
                .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed");
        }
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert!(rows.is_empty(), "a refused APPEND stores nothing");
    }

    #[tokio::test]
    async fn append_happy_path_returns_uid_and_validity() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [220u8; 32];

        let reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec!["\\Draft".into()]),
        )
        .await
        .expect("append handler ok");

        assert_eq!(reply.message_id.len(), 32, "message_id is 32 bytes");
        assert_eq!(reply.uid, 1, "first message in Drafts gets uid 1");
        assert_eq!(reply.uid_validity, 1);

        // The bridge_imap_messages row should have the correct flags and internal_date.
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "Drafts")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1, "uid 1");
        assert_eq!(rows[0].1, "\\Draft", "flags");
        assert_eq!(rows[0].2, 1_700_000_000, "internal_date");

        // The mail-segment record should carry is_own_submission=true,
        // spam_disposition="accept", and the auth verdicts the APPEND
        // path stamps server-side.
        let mid: [u8; 32] = reply.message_id[..].try_into().unwrap();
        let (_env, floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("mail-segment record present");
        assert!(
            floor.is_own_submission,
            "is_own_submission must be true for APPEND"
        );
        assert_eq!(floor.spam_disposition, "accept");
        assert_eq!(floor.spf, "none");
        assert_eq!(floor.dkim, "none");
        assert_eq!(floor.dmarc, "none");
        assert_eq!(floor.arc, "none");
        assert_eq!(floor.spam_score, 0);
    }

    /// Seed one **real** mail record (writing a real segment) into the actor's
    /// INBOX and return its CARv2 block byte-length — the size the quota path
    /// now counts. Quota usage is sized through the segment index by record_cid
    /// (imap-server.md § QUOTA), so it can no longer be faked with a synthetic
    /// `byte_length` column: near-cap tests instead lower the deployment ceiling
    /// via `put_imap_policy` relative to the returned size.
    async fn seed_one_real_record(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        body: &[u8],
    ) -> u64 {
        let (_uid, msg_id) = seed_message_for_handler_with_hint(
            state,
            actor,
            "INBOX",
            body,
            1_700_000_000,
            "",
            &[7u8; 4],
        )
        .await;
        real_record_size(state, actor, &msg_id).await
    }

    #[tokio::test]
    async fn append_over_storage_quota_returns_over_quota() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [222u8; 32];

        // Lower the storage ceiling below the 20-byte `sample_append_req` body:
        // with zero existing usage the added body alone trips the quota, so the
        // pre-check arithmetic (used + added vs ceiling) rejects.
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec![]),
        )
        .await
        .expect_err("append over the storage quota must be rejected");
        assert_eq!(err.code, "fauna.bridges.over_quota");
    }

    #[tokio::test]
    async fn append_within_storage_quota_succeeds_when_actor_is_loaded() {
        // An actor already holding a real message admits the small APPEND when
        // there's headroom — proves the pre-check admits, not just rejects, and
        // that existing usage (sized through the index) is counted.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [223u8; 32];

        let used = seed_one_real_record(&state, &target, &[b'x'; 200]).await;
        // 1 MiB of headroom above the existing real usage — comfortably above
        // the 20-byte body.
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(used + (1u64 << 20)),
                ..Default::default()
            })
            .await
            .unwrap();

        let reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec![]),
        )
        .await
        .expect("append within quota must succeed");
        assert_eq!(reply.uid, 1, "first Drafts message gets uid 1");
    }

    #[tokio::test]
    async fn append_over_lowered_override_quota_is_rejected() {
        // An admin `put_imap_policy { storage_bytes_default: <tiny> }` must
        // actually bind on the enforcement path, *and* existing real usage
        // (sized through the CARv2 index by record_cid) must count toward it.
        // Seed one real record, then lower the ceiling to its size + 5 bytes:
        // the 20-byte APPEND body has no headroom → over_quota. Under the
        // catalog 1 GiB default this APPEND would succeed, so a rejection
        // proves the override is read (not write-only).
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [224u8; 32];

        let used = seed_one_real_record(&state, &target, &[b'x'; 200]).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(used + 5),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec![]),
        )
        .await
        .expect_err("append over the lowered override quota must be rejected");
        assert_eq!(err.code, "fauna.bridges.over_quota");
    }

    #[tokio::test]
    async fn get_quota_reports_effective_override_limit() {
        // Reporting reads the same effective source as enforcement, so an
        // admin who lowers `storage_bytes_default` sees the new ceiling in
        // GETQUOTA — and it matches the threshold APPEND rejects at.
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(4096),
                message_count_default: Some(7),
                ..Default::default()
            })
            .await
            .unwrap();
        let target = [225u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();

        let reply = call_get_quota(
            state.clone(),
            mda,
            GetQuotaRequest {
                actor_id: target.to_vec(),
            },
        )
        .await;
        assert_eq!(
            reply.storage_bytes_limit, 4096,
            "GETQUOTA reflects the lowered storage override"
        );
        assert_eq!(
            reply.message_count_limit, 7,
            "GETQUOTA reflects the lowered message-count override"
        );
    }

    #[test]
    fn imap_quota_overage_storage_axis() {
        // 100 used + 50 added = 150 > 120 storage limit → storage trips.
        assert_eq!(
            imap_quota_overage(100, 1, 50, 1, 120, 1000),
            Some("storage")
        );
    }

    #[test]
    fn imap_quota_overage_message_axis() {
        // Storage fits (10 ≤ 1000) but 5 used + 1 added = 6 > 5 count limit.
        assert_eq!(imap_quota_overage(10, 5, 0, 1, 1000, 5), Some("message"));
    }

    #[test]
    fn imap_quota_overage_exactly_at_limit_is_allowed() {
        // used + added == limit on both axes: a ceiling is fillable exactly.
        assert_eq!(imap_quota_overage(80, 49, 40, 1, 120, 50), None);
    }

    #[test]
    fn imap_quota_overage_within_is_allowed() {
        assert_eq!(imap_quota_overage(10, 2, 5, 1, 1000, 50), None);
    }

    #[test]
    fn imap_quota_overage_storage_checked_before_message() {
        // Both axes would trip; STORAGE is reported first (RES-STORAGE).
        assert_eq!(
            imap_quota_overage(100, 50, 50, 50, 120, 60),
            Some("storage")
        );
    }

    #[test]
    fn imap_quota_overage_saturates_on_overflow() {
        // used + added would overflow u64 — naive `+` panics (debug) or wraps
        // to a tiny value that falsely "fits" (release). saturating_add caps
        // at u64::MAX, which exceeds the (MAX - 50) limit → correctly trips.
        assert_eq!(
            imap_quota_overage(u64::MAX - 5, 0, 100, 0, u64::MAX - 50, 1000),
            Some("storage"),
        );
    }

    #[tokio::test]
    async fn append_idempotent_retry_returns_same_uid_and_message_id() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [221u8; 32];

        let req = sample_append_req(&target, "Drafts", vec![]);

        let reply1 = call_append_message(state.clone(), mda, req.clone())
            .await
            .expect("first ok");
        let reply2 = call_append_message(state.clone(), mda, req)
            .await
            .expect("second ok");

        assert_eq!(
            reply1.message_id, reply2.message_id,
            "same message_id on retry"
        );
        assert_eq!(reply1.uid, reply2.uid, "same uid on retry");

        // Exactly one row in bridge_imap_messages.
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "Drafts")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "no duplicate placement on idempotent retry");
    }

    #[tokio::test]
    async fn append_auto_creates_non_standard_mailbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [222u8; 32];

        let reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "MyFolder", vec![]),
        )
        .await
        .expect("append to custom mailbox ok");

        assert_eq!(reply.uid, 1);
        assert_eq!(reply.uid_validity, 1);

        // State row must exist for "MyFolder".
        let state_row = state
            .db
            .get_bridge_imap_mailbox_state(&target, "MyFolder")
            .await
            .unwrap();
        assert!(state_row.is_some(), "MyFolder state row auto-created");
    }

    #[tokio::test]
    async fn append_empty_body_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [223u8; 32];

        let err = call_append_message(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "Drafts".into(),
                flags: vec![],
                encrypted_body: vec![], // empty → malformed
                encrypted_index_hint: b"hint".to_vec(),
                timestamp: 1_700_000_000,
                ciphertext_size: 0,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn append_empty_hint_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [224u8; 32];

        let err = call_append_message(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "Drafts".into(),
                flags: vec![],
                encrypted_body: b"body".to_vec(),
                encrypted_index_hint: vec![], // empty → malformed
                timestamp: 1_700_000_000,
                ciphertext_size: 4,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn append_ciphertext_size_mismatch_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [225u8; 32];

        let err = call_append_message(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "Drafts".into(),
                flags: vec![],
                encrypted_body: b"body".to_vec(), // len=4
                encrypted_index_hint: b"hint".to_vec(),
                timestamp: 1_700_000_000,
                ciphertext_size: 99, // mismatch → malformed
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn append_recent_flag_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [226u8; 32];

        let err = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec!["\\Recent".into()]),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn append_short_actor_id_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let body = b"body".to_vec();
        let err = call_append_message(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::AppendMessageRequest {
                actor_id: vec![1u8; 16], // 16 bytes → malformed
                mailbox: "Drafts".into(),
                flags: vec![],
                encrypted_body: body.clone(),
                encrypted_index_hint: b"hint".to_vec(),
                timestamp: 1_700_000_000,
                ciphertext_size: body.len() as u32,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// S6.12b structural seal gate (mail): `append_message_handler` proves both
    /// payload halves are sealed recipient envelopes at the wire edge. A raw
    /// RFC 5322 body — non-empty, correct `ciphertext_size`, no `\Recent`, with
    /// a genuinely sealed hint — is refused as malformed, so nothing unsealed
    /// can reach the backup-eligible `__mail` segment store via APPEND.
    #[tokio::test]
    async fn append_rejects_an_unsealed_body() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xADu8; 32];

        let raw_body = b"From: a@b.example\r\n\r\nnot a sealed envelope\r\n".to_vec();
        let err = call_append_message(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                flags: vec![],
                ciphertext_size: raw_body.len() as u32,
                encrypted_body: raw_body,
                // A genuine seal — so the body verify (not the hint verify) is
                // the check that fires.
                encrypted_index_hint: sealed(b"index-hint"),
                timestamp: 1_700_000_000,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .expect_err("an unsealed APPEND body must be rejected");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn append_mta_caller_returns_permission_denied() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [227u8; 32];

        let err = call_append_message(
            state.clone(),
            mta,
            sample_append_req(&target, "Drafts", vec![]),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── C.7 search_messages handler tests ─────────────────────────────

    /// Seed a placement row with explicit `*_norm` columns. Bypasses
    /// `place_inbound_mail` so multi-axis tests (Subject/To/Cc/From)
    /// can populate every column independently. Mirrors the test seeds
    /// used by C.5/C.6 metadata/body fetch suites.
    async fn seed_message_for_search(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        msg_id: &[u8; 32],
        mailbox: &str,
        _body: &[u8],
        internal_date: i64,
        flags: &str,
        from_norm: &str,
        to_norm: &str,
        cc_norm: &str,
        subject_norm: &str,
    ) -> u32 {
        // Seed segment_records mirror so the search SQL JOIN (on
        // substr(record_cid, 5) = message_id) finds the row.
        let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(*msg_id);
        state
            .db
            .conn()
            .await
            .execute(
                "INSERT OR IGNORE INTO segment_records \
                    (scope_id, kind, segment_id, record_cid, bucket, \
                     tombstoned, \
                     received_at, sender_dom, spam_disp, is_own_submission) \
                 VALUES (?1, 'mail', 1, ?2, '2026-05', 0, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &record_cid.as_bytes()[..],
                    internal_date,
                    from_norm,
                    "accept",
                    0i64,
                ],
            )
            .unwrap();
        // place_inbound_mail populates from_norm itself; afterward we
        // overwrite the other three norm columns to whatever the test
        // wants.
        let (uid, _modseq) = state
            .db
            .place_inbound_mail(
                actor,
                msg_id,
                mailbox,
                internal_date,
                flags,
                from_norm,
                true,
            )
            .await
            .unwrap()
            .expect("place_inbound_mail returned None");
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages \
                 SET to_norm = ?1, cc_norm = ?2, subject_norm = ?3 \
                 WHERE actor_id = ?4 AND mailbox = ?5 AND uid = ?6",
                rusqlite::params![
                    to_norm,
                    cc_norm,
                    subject_norm,
                    &actor[..],
                    mailbox,
                    uid as i64
                ],
            )
            .unwrap();
        uid
    }

    async fn call_search_messages(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::SearchMessagesRequest,
    ) -> fauna_protocol::bridge_routing::SearchMessagesReply {
        use fauna_protocol::bridge_routing::SearchMessagesReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = search_messages_handler()(state, actor_id, payload)
            .await
            .expect("search_messages handler ok");
        fauna_cbor::decode_strict::<SearchMessagesReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn search_has_flag_returns_only_flagged_uids() {
        use fauna_protocol::bridge_routing::{SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [70u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // uid 1: \Seen.  uid 2: empty.  uid 3: \Seen \Answered.
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "\\Seen",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_002,
            "",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[3u8; 32],
            "INBOX",
            b"ccc",
            1_700_000_003,
            "\\Seen \\Answered",
            "",
            "",
            "",
            "",
        )
        .await;

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::HasFlag {
                    flag: "\\Seen".into(),
                }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![1, 3]);
    }

    #[tokio::test]
    async fn search_lacks_flag_returns_unflagged_uids() {
        use fauna_protocol::bridge_routing::{SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [71u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "\\Seen",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_002,
            "",
            "",
            "",
            "",
            "",
        )
        .await;

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::LacksFlag {
                    flag: "\\Seen".into(),
                }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![2]);
    }

    #[tokio::test]
    async fn search_header_contains_matches_case_folded_substring() {
        use fauna_protocol::bridge_routing::{HeaderField, SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [72u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // Seed three rows with different header norms.
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "",
            "alice@example.com",
            "bob@somewhere.com",
            "",
            "weekly update",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_002,
            "",
            "carol@elsewhere.org",
            "",
            "",
            "quarterly invoice",
        )
        .await;

        // From substring (case-folded — pattern "EXAMPLE" matches lowercased "example").
        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::HeaderContains {
                    field: HeaderField::From,
                    value: "EXAMPLE".into(),
                }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![1]);

        // Subject substring (matches uid 2).
        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::HeaderContains {
                    field: HeaderField::Subject,
                    value: "invoice".into(),
                }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![2]);
    }

    #[tokio::test]
    async fn search_internal_date_predicates_match_inclusive_since_exclusive_before() {
        use fauna_protocol::bridge_routing::{SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [73u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_005,
            "",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[3u8; 32],
            "INBOX",
            b"ccc",
            1_700_000_010,
            "",
            "",
            "",
            "",
            "",
        )
        .await;

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::SinceInternalDate { ts: 1_700_000_005 }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![2, 3], "Since is inclusive");

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::BeforeInternalDate { ts: 1_700_000_005 }],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![1], "Before is exclusive");
    }

    #[tokio::test]
    async fn search_size_predicates_match_strict_larger_smaller() {
        use fauna_protocol::bridge_routing::{SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [74u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // Real segments with clearly increasing bodies → strictly increasing
        // CARv2 block lengths. SEARCH size axes are sized through the index by
        // record_cid (imap-server.md § SEARCH), so the test must store real
        // records and key the thresholds off their actual sizes (the framing +
        // hint overhead means the size isn't the body length).
        let (uid1, mid1) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'a'; 50],
            1_700_000_001,
            "",
            &[7u8; 4],
        )
        .await;
        let (_uid2, mid2) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'b'; 500],
            1_700_000_002,
            "",
            &[7u8; 4],
        )
        .await;
        let (uid3, mid3) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'c'; 5000],
            1_700_000_003,
            "",
            &[7u8; 4],
        )
        .await;
        let s1 = real_record_size(&state, &target, &mid1).await;
        let s2 = real_record_size(&state, &target, &mid2).await;
        let s3 = real_record_size(&state, &target, &mid3).await;
        assert!(s1 < s2 && s2 < s3, "bodies give strictly increasing sizes");

        // LARGER is strict (>): a record whose size == the threshold is excluded.
        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::Larger { size: s2 as u32 }],
            },
        )
        .await;
        assert_eq!(
            reply.uids,
            vec![uid3],
            "Larger is strict (>); the record whose size == s2 (uid2) is excluded"
        );

        // SMALLER is strict (<): the == record is excluded.
        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![SearchTerm::Smaller { size: s2 as u32 }],
            },
        )
        .await;
        assert_eq!(
            reply.uids,
            vec![uid1],
            "Smaller is strict (<); the record whose size == s2 (uid2) is excluded"
        );
    }

    #[tokio::test]
    async fn search_anded_terms_intersect() {
        use fauna_protocol::bridge_routing::{HeaderField, SearchMessagesRequest, SearchTerm};

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [75u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // uid 1: \Seen + Subject "invoice"
        // uid 2: \Seen + Subject "newsletter"
        // uid 3: empty + Subject "invoice"
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "\\Seen",
            "",
            "",
            "",
            "monthly invoice",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_002,
            "\\Seen",
            "",
            "",
            "",
            "weekly newsletter",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[3u8; 32],
            "INBOX",
            b"ccc",
            1_700_000_003,
            "",
            "",
            "",
            "",
            "another invoice",
        )
        .await;

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![
                    SearchTerm::HasFlag {
                        flag: "\\Seen".into(),
                    },
                    SearchTerm::HeaderContains {
                        field: HeaderField::Subject,
                        value: "invoice".into(),
                    },
                ],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![1]);
    }

    #[tokio::test]
    async fn search_empty_terms_returns_all_uids_ascending() {
        use fauna_protocol::bridge_routing::SearchMessagesRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [76u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        seed_message_for_search(
            &state,
            &target,
            &[1u8; 32],
            "INBOX",
            b"a",
            1_700_000_001,
            "",
            "",
            "",
            "",
            "",
        )
        .await;
        seed_message_for_search(
            &state,
            &target,
            &[2u8; 32],
            "INBOX",
            b"bb",
            1_700_000_002,
            "",
            "",
            "",
            "",
            "",
        )
        .await;

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                terms: vec![],
            },
        )
        .await;
        assert_eq!(reply.uids, vec![1, 2]);
    }

    #[tokio::test]
    async fn search_unknown_mailbox_returns_empty() {
        use fauna_protocol::bridge_routing::SearchMessagesRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [77u8; 32];

        let reply = call_search_messages(
            state.clone(),
            mda,
            SearchMessagesRequest {
                actor_id: target.to_vec(),
                mailbox: "Nonexistent".into(),
                terms: vec![],
            },
        )
        .await;
        assert_eq!(reply.uids, Vec::<u32>::new());
    }

    #[tokio::test]
    async fn search_mta_caller_returns_permission_denied() {
        use fauna_protocol::bridge_routing::SearchMessagesRequest;

        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [78u8; 32];

        let req = SearchMessagesRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
            terms: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = search_messages_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn search_short_actor_id_returns_malformed() {
        use fauna_protocol::bridge_routing::SearchMessagesRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SearchMessagesRequest {
            actor_id: vec![1u8; 16],
            mailbox: "INBOX".into(),
            terms: vec![],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = search_messages_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── C.8 get_quota handler tests ───────────────────────────────────

    async fn call_get_quota(
        state: Arc<crate::routes::AppState>,
        actor_id: [u8; 32],
        req: fauna_protocol::bridge_routing::GetQuotaRequest,
    ) -> fauna_protocol::bridge_routing::GetQuotaReply {
        use fauna_protocol::bridge_routing::GetQuotaReply;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = get_quota_handler()(state, actor_id, payload)
            .await
            .expect("get_quota handler ok");
        fauna_cbor::decode_strict::<GetQuotaReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn get_quota_sums_placement_byte_lengths_and_counts() {
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [80u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        // Three real placements; STORAGE = Σ record block length sized through
        // the CARv2 index by record_cid (imap-server.md § QUOTA), not a SQL
        // byte column — so the expected total is the sum of the real sizes.
        let (_u1, m1) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'a'; 100],
            1_700_000_001,
            "",
            &[7u8; 4],
        )
        .await;
        let (_u2, m2) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'b'; 200],
            1_700_000_002,
            "",
            &[7u8; 4],
        )
        .await;
        let (_u3, m3) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'c'; 300],
            1_700_000_003,
            "",
            &[7u8; 4],
        )
        .await;
        let expected_total = real_record_size(&state, &target, &m1).await
            + real_record_size(&state, &target, &m2).await
            + real_record_size(&state, &target, &m3).await;

        let reply = call_get_quota(
            state.clone(),
            mda,
            GetQuotaRequest {
                actor_id: target.to_vec(),
            },
        )
        .await;
        assert_eq!(reply.storage_bytes_used, expected_total);
        assert_eq!(reply.message_count_used, 3);
        // Defaults match `ImapPolicy::default()` — the handler returns
        // them in-band so the bridge doesn't need a concurrent
        // fetch_config round-trip.
        assert_eq!(reply.storage_bytes_limit, 1 << 30);
        assert_eq!(reply.message_count_limit, 50_000);
    }

    #[tokio::test]
    async fn get_quota_zero_for_actor_with_no_messages() {
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [81u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();

        let reply = call_get_quota(
            state.clone(),
            mda,
            GetQuotaRequest {
                actor_id: target.to_vec(),
            },
        )
        .await;
        assert_eq!(reply.storage_bytes_used, 0);
        assert_eq!(reply.message_count_used, 0);
        assert_eq!(reply.storage_bytes_limit, 1 << 30);
        assert_eq!(reply.message_count_limit, 50_000);
    }

    #[tokio::test]
    async fn get_quota_ignores_tombstoned_segments() {
        // QUOTA must reflect live messages only — tombstoned segments
        // represent expunged content that's been GC'd from the storage
        // accounting. Mirrors `search_bridge_imap_messages`'s
        // `sr.tombstoned = 0` predicate.
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [82u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        let (_u1, live_msg) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'a'; 100],
            1_700_000_001,
            "",
            &[7u8; 4],
        )
        .await;
        let (_u2, tombstoned_msg) = seed_message_for_handler_with_hint(
            &state,
            &target,
            "INBOX",
            &[b'b'; 999],
            1_700_000_002,
            "",
            &[7u8; 4],
        )
        .await;
        // Tombstone the second message's segment record.
        let tombstoned_cid = fauna_cbor::Cid::from_digest_dag_cbor(tombstoned_msg);
        state
            .db
            .conn()
            .await
            .execute(
                "UPDATE segment_records SET tombstoned = 1 \
                 WHERE scope_id = ?1 AND kind = 'mail' AND record_cid = ?2",
                rusqlite::params![&target[..], &tombstoned_cid.as_bytes()[..]],
            )
            .unwrap();

        // Only the live record's real CARv2 block length counts.
        let expected_total = real_record_size(&state, &target, &live_msg).await;

        let reply = call_get_quota(
            state.clone(),
            mda,
            GetQuotaRequest {
                actor_id: target.to_vec(),
            },
        )
        .await;
        assert_eq!(reply.storage_bytes_used, expected_total);
        assert_eq!(reply.message_count_used, 1);
    }

    #[tokio::test]
    async fn get_quota_mta_caller_returns_permission_denied() {
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [83u8; 32];

        let req = GetQuotaRequest {
            actor_id: target.to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = get_quota_handler()(state, mta, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn get_quota_short_actor_id_returns_malformed() {
        use fauna_protocol::bridge_routing::GetQuotaRequest;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = GetQuotaRequest {
            actor_id: vec![1u8; 16],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = get_quota_handler()(state, mda, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── the training lesson: put_spam_model's history op ─────────────
    //
    // Every training lesson — the MDA's `\Junk` STORE/MOVE and the Fauna app's
    // mail button alike — is a holder's sealed `put_spam_model` carrying a
    // history `Insert`; an undo is one carrying a `Delete`. These tests pin the
    // nest's half of that: the one-lesson rule and the mail report capture.

    /// The MDA's agent-side `\Junk` train for `actor`: a re-sealed model plus
    /// one sealed history row for `msg_id` (opaque stand-in bytes — the nest
    /// never opens them).
    async fn mda_lesson(
        state: &Arc<crate::routes::AppState>,
        mda: [u8; 32],
        actor: [u8; 32],
        msg_id: [u8; 32],
        label: SpamLabel,
        source: TrainingSource,
    ) -> PutSpamModelOutcome {
        call_put_spam_model(
            state.clone(),
            mda,
            PutSpamModelRequest {
                actor_id: actor.to_vec(),
                sealed_model: vec![0xEEu8; 64],
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: msg_id.to_vec(),
                    mailbox: "Junk".into(),
                    sealed_subject: vec![0xC1u8; 24],
                    sealed_delta: vec![0xD1u8; 48],
                    label,
                    source,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("put ok")
        .outcome
    }

    /// The client's undo of one lesson: its re-sealed model plus a history
    /// `Delete` (the client already applied the inverse locally).
    async fn undo_lesson(
        state: &Arc<crate::routes::AppState>,
        actor: [u8; 32],
        history_id: Vec<u8>,
    ) -> PutSpamModelOutcome {
        call_put_spam_model(
            state.clone(),
            actor,
            PutSpamModelRequest {
                sealed_model: vec![0xEFu8; 64],
                history_op: Some(SpamHistoryOp::Delete { history_id }),
                ..Default::default()
            },
        )
        .await
        .expect("undo put ok")
        .outcome
    }

    /// The full report-capture journey through the training write
    /// (report-sharing.md § Report capture + § The aggregate): three opted-in
    /// actors flag copies of the same content (same canonical report-hash) →
    /// nothing is readable anywhere below k, and at exactly k=3 every copy
    /// carries the tier-3 `report:spam` bus row at 200‰; one reporter's ham
    /// correction then drops the aggregate below k and withdraws the rows.
    #[tokio::test]
    async fn a_spam_lesson_captures_the_report_and_a_ham_lesson_withdraws_it() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let hash = [0x5A; 32];
        let actors = [[71u8; 32], [72u8; 32], [73u8; 32]];
        let mut msg_ids = Vec::new();
        for actor in &actors {
            state.db.set_share_reports(actor, true).await.unwrap();
            let (_uid, msg_id) =
                seed_message_with_report_hash(&state, actor, b"same spam campaign", hash).await;
            msg_ids.push(msg_id);
        }

        let report_score = |msg_id: [u8; 32]| {
            let state = state.clone();
            async move { state.db.test_report_row_score(&msg_id).await }
        };

        for (i, (actor, msg_id)) in actors.iter().zip(&msg_ids).enumerate() {
            let outcome = mda_lesson(
                &state,
                mda,
                *actor,
                *msg_id,
                SpamLabel::Spam,
                TrainingSource::ImapJunkFlag,
            )
            .await;
            assert_eq!(outcome, PutSpamModelOutcome::Written);
            if i < 2 {
                assert_eq!(
                    report_score(msg_ids[0]).await,
                    None,
                    "below k nothing is readable (after reporter {})",
                    i + 1
                );
            }
        }
        // k=3: every copy carries the row.
        for msg_id in &msg_ids {
            assert_eq!(report_score(*msg_id).await, Some(200), "k=3 → 200‰");
        }

        // A ham correction by one reporter withdraws their report → below k
        // → the rows are withdrawn everywhere.
        let outcome = mda_lesson(
            &state,
            mda,
            actors[0],
            msg_ids[0],
            SpamLabel::Ham,
            TrainingSource::ImapJunkMove,
        )
        .await;
        assert_eq!(outcome, PutSpamModelOutcome::Written);
        for msg_id in &msg_ids {
            assert_eq!(report_score(*msg_id).await, None, "below k withdraws");
        }
    }

    /// Undoing a spam lesson withdraws the report it captured — a withdrawn
    /// judgment leaves no residue (report-sharing.md § Report capture,
    /// *Symmetry*). The nest reads the removed row's message and label inside
    /// the write's transaction; the delete carries only the history id.
    #[tokio::test]
    async fn undoing_a_spam_lesson_withdraws_its_report() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let hash = [0x5C; 32];
        let actors = [[75u8; 32], [76u8; 32], [77u8; 32]];
        let mut msg_ids = Vec::new();
        for actor in &actors {
            state.db.create_user(actor, "free", "test").await.unwrap();
            state.db.set_share_reports(actor, true).await.unwrap();
            let (_uid, msg_id) =
                seed_message_with_report_hash(&state, actor, b"undo campaign", hash).await;
            mda_lesson(
                &state,
                mda,
                *actor,
                msg_id,
                SpamLabel::Spam,
                TrainingSource::ImapJunkFlag,
            )
            .await;
            msg_ids.push(msg_id);
        }
        assert_eq!(
            state.db.test_report_row_score(&msg_ids[0]).await,
            Some(200),
            "control: k=3 reporters"
        );

        let rows = call_list_spam_training_history(state.clone(), actors[0], None, None)
            .await
            .expect("list ok")
            .events;
        assert_eq!(rows.len(), 1);
        let outcome = undo_lesson(&state, actors[0], rows[0].history_id.clone()).await;
        assert_eq!(outcome, PutSpamModelOutcome::Written);
        assert_eq!(
            state.db.test_report_row_score(&msg_ids[0]).await,
            None,
            "the undone report drops the aggregate below k"
        );
        assert_eq!(state.db.test_content_reports_count().await, 2);

        // Undoing a HAM lesson withdraws nothing: it captured no report.
        mda_lesson(
            &state,
            mda,
            actors[1],
            msg_ids[1],
            SpamLabel::Ham,
            TrainingSource::ImapJunkMove,
        )
        .await;
        assert_eq!(
            state.db.test_content_reports_count().await,
            1,
            "the ham lesson withdrew actor 1's report"
        );
        let ham_row = call_list_spam_training_history(state.clone(), actors[1], None, None)
            .await
            .expect("list ok")
            .events
            .into_iter()
            .find(|r| r.label == SpamLabel::Ham)
            .expect("the ham row");
        undo_lesson(&state, actors[1], ham_row.history_id).await;
        assert_eq!(state.db.test_content_reports_count().await, 1);
    }

    /// Default-off pref: a lesson from an un-opted actor emits no report row
    /// (report-sharing.md § Report capture — opt-in, default off).
    #[tokio::test]
    async fn a_lesson_without_opt_in_captures_nothing() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [74u8; 32];
        let hash = [0x5B; 32];
        let (_uid, msg_id) =
            seed_message_with_report_hash(&state, &actor, b"unshared flag", hash).await;
        let outcome = mda_lesson(
            &state,
            mda,
            actor,
            msg_id,
            SpamLabel::Spam,
            TrainingSource::ImapJunkFlag,
        )
        .await;
        assert_eq!(outcome, PutSpamModelOutcome::Written);
        assert_eq!(
            state.db.test_content_reports_count().await,
            0,
            "no report row without the opt-in"
        );
    }

    /// A message with no stored report-hash (an APPEND stores none) cannot
    /// aggregate: an opted-in spam lesson on it lands, and captures nothing.
    #[tokio::test]
    async fn a_lesson_on_a_message_without_a_report_hash_captures_nothing() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [78u8; 32];
        state.db.set_share_reports(&actor, true).await.unwrap();
        let (_uid, msg_id) = seed_message_for_handler_with_hint(
            &state,
            &actor,
            "INBOX",
            b"appended mail",
            1_700_000_000,
            "",
            b"hint",
        )
        .await;
        let outcome = mda_lesson(
            &state,
            mda,
            actor,
            msg_id,
            SpamLabel::Spam,
            TrainingSource::ImapJunkFlag,
        )
        .await;
        assert_eq!(outcome, PutSpamModelOutcome::Written);
        assert_eq!(
            state.db.test_content_reports_count().await,
            0,
            "a hash-less message captures no report"
        );
    }

    /// The one-lesson rule keys on the NEWEST lesson: an opposite-label lesson
    /// re-opens the key, so spam → ham → spam is three lessons (the user
    /// changed their mind twice), and an undo deletes the row, so the next mark
    /// trains again. A duplicate captures no report either.
    #[tokio::test]
    async fn a_lesson_reopens_after_an_opposite_lesson_or_an_undo() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [65u8; 32];
        state.db.create_user(&target, "free", "test").await.unwrap();
        state.db.set_share_reports(&target, true).await.unwrap();
        let (_uid, msg_id) =
            seed_message_with_report_hash(&state, &target, b"limited offer", [0x5D; 32]).await;
        use fauna_protocol::bridge_routing::TrainingSource::{ImapJunkFlag, ImapJunkMove};
        // The source varies per step only so the test can pick the third
        // lesson's row below without leaning on the list's ordering (rows
        // written within one millisecond tie on `created_at`); the rule
        // itself never looks at the source.
        for (label, source, want) in [
            (SpamLabel::Spam, ImapJunkFlag, PutSpamModelOutcome::Written),
            (
                SpamLabel::Spam,
                ImapJunkMove,
                PutSpamModelOutcome::DuplicateSignal,
            ),
            (SpamLabel::Ham, ImapJunkMove, PutSpamModelOutcome::Written),
            (SpamLabel::Spam, ImapJunkMove, PutSpamModelOutcome::Written),
            (
                SpamLabel::Spam,
                ImapJunkFlag,
                PutSpamModelOutcome::DuplicateSignal,
            ),
        ] {
            let got = mda_lesson(&state, mda, target, msg_id, label, source).await;
            assert_eq!(got, want, "label {label:?} via {source:?}");
        }
        let history = call_list_spam_training_history(state.clone(), target, None, None)
            .await
            .expect("list ok");
        assert_eq!(history.events.len(), 3, "spam, ham, spam — three lessons");
        assert_eq!(
            state.db.test_content_reports_count().await,
            1,
            "the newest lesson is spam: one report on record"
        );

        // Undo the newest lesson (the spam MOVE): the row goes, so the same
        // mark trains again — the undone lesson is no longer on record and
        // the ham lesson before it is now the newest.
        let newest = history
            .events
            .iter()
            .find(|e| e.label == SpamLabel::Spam && e.source == ImapJunkMove)
            .expect("the third lesson's row")
            .history_id
            .clone();
        assert_eq!(
            undo_lesson(&state, target, newest).await,
            PutSpamModelOutcome::Written
        );
        assert_eq!(state.db.test_content_reports_count().await, 0);
        let again = mda_lesson(&state, mda, target, msg_id, SpamLabel::Spam, ImapJunkFlag).await;
        assert_eq!(again, PutSpamModelOutcome::Written);
        assert_eq!(state.db.test_content_reports_count().await, 1);
    }

    // ── Slice 4 item 1 — fetch_spam_model handler tests (MDA-only) ──

    async fn call_fetch_spam_model(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        req: FetchSpamModelRequest,
    ) -> Result<FetchSpamModelReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_spam_model_handler()(state, caller, payload).await?;
        Ok(fauna_cbor::decode_strict::<FetchSpamModelReply>(&bytes).unwrap())
    }

    /// An actor with no trained model gets `blob: None` (⇒ cold start on
    /// the device). No MLS pubkey is needed — there is nothing to seal.
    #[tokio::test]
    async fn fetch_spam_model_untrained_returns_none() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [80u8; 32];
        let reply = call_fetch_spam_model(
            state,
            mda,
            FetchSpamModelRequest {
                actor_id: target.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("handler ok");
        assert_eq!(reply.blob, None);
    }

    /// A stored model that is NOT sealed cannot have been written by this
    /// binary (`put_spam_model` refuses one), so `fetch_spam_model` fails closed
    /// with `internal` — it never decodes it, folds into it or hands it out.
    #[tokio::test]
    async fn fetch_spam_model_fails_closed_on_a_stored_model_that_is_not_sealed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [81u8; 32];
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO spam_models (actor_id, model_json, updated_at) VALUES (?1, ?2, 1)",
                rusqlite::params![
                    target.to_vec(),
                    crate::test_support::small_spam_model("free crypto", "lunch", 2).to_bytes()
                ],
            )
            .unwrap();
        }
        let err = call_fetch_spam_model(
            state,
            mda,
            FetchSpamModelRequest {
                actor_id: target.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect_err("a plaintext stored model fails closed");
        assert_eq!(err.code, "fauna.protocol.internal");
    }

    /// A User caller may fetch their OWN model (caller-scoped). The
    /// gates landed, so the User/
    /// Fauna-app leg (e.g. the android per-user scorer) is enabled.
    /// Untrained ⇒ `None` (cold start on the device).
    #[tokio::test]
    async fn fetch_spam_model_user_own_model_ok() {
        let state = fixture_state().await;
        // A bare, non-bridge actor resolves to `CallerClass::User`.
        let user = [82u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a User may fetch their own model");
        assert_eq!(reply.blob, None);
    }

    /// A User fetching ANOTHER actor's model is denied
    /// (`target != caller` for a non-MDA class) — closes the cross-user
    /// trained-state / mail-serving existence oracle. The check fires before
    /// `require_local_mail_serving`, so it denies regardless of the victim's
    /// mail config.
    #[tokio::test]
    async fn fetch_spam_model_user_cross_actor_denied() {
        let state = fixture_state().await;
        let user = [82u8; 32];
        let victim = [83u8; 32];
        let err = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: victim.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect_err("a User may not fetch another actor's model");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_spam_model_short_actor_id_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let payload = Bytes::from(
            encode_canonical(&FetchSpamModelRequest {
                actor_id: vec![1u8; 16],
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = fetch_spam_model_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── put_spam_model (opaque sealed-write-back, leg 3) ──

    async fn call_put_spam_model(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        req: PutSpamModelRequest,
    ) -> Result<PutSpamModelReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = put_spam_model_handler()(state, caller, payload).await?;
        Ok(fauna_cbor::decode_strict::<PutSpamModelReply>(&bytes).unwrap())
    }

    /// The optional deployment-baseline holder copy rides `put_spam_model`
    /// atomically: present ⇒ the `(actor, holder)` copy row is replaced with
    /// the model write; absent ⇒ an existing copy is left untouched (a
    /// writer that opted out or has no holder leaves stale weights, never
    /// drops the contributor).
    /// Malformed copies are rejected before any write.
    #[tokio::test]
    async fn put_spam_model_holder_copy_stored_replaced_and_kept_on_absent() {
        let state = fixture_state().await;
        let user = [90u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let holder = [0x44u8; 32];

        // Write 1: model + copy v1.
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEE; 64],
                holder_copy: Some(fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                    holder_pubkey: holder.to_vec(),
                    sealed_copy: vec![0xC1; 96],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("copy write ok");
        let copies = state
            .db
            .list_spam_model_holder_copies(&holder)
            .await
            .unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].0, user);
        assert_eq!(copies[0].1, vec![0xC1; 96]);

        // Write 2: model + copy v2 replaces v1 (one row per (actor, holder)).
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEF; 64],
                holder_copy: Some(fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                    holder_pubkey: holder.to_vec(),
                    sealed_copy: vec![0xC2; 96],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("copy replace ok");
        let copies = state
            .db
            .list_spam_model_holder_copies(&holder)
            .await
            .unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].1, vec![0xC2; 96]);

        // Write 3: model-only (no copy field) keeps the stored copy.
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xF0; 64],
                ..Default::default()
            },
        )
        .await
        .expect("model-only write ok");
        let copies = state
            .db
            .list_spam_model_holder_copies(&holder)
            .await
            .unwrap();
        assert_eq!(copies.len(), 1, "absent copy field leaves the row alone");
        assert_eq!(copies[0].1, vec![0xC2; 96]);

        // Malformed copies are typed rejections, and nothing was written.
        for bad in [
            fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                holder_pubkey: vec![0x44; 31], // not 32 bytes
                sealed_copy: vec![0xC3; 8],
                ..Default::default()
            },
            fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                holder_pubkey: holder.to_vec(),
                sealed_copy: Vec::new(), // empty
                ..Default::default()
            },
        ] {
            let err = call_put_spam_model(
                state.clone(),
                user,
                PutSpamModelRequest {
                    sealed_model: vec![0xF1; 64],
                    holder_copy: Some(bad),
                    ..Default::default()
                },
            )
            .await
            .expect_err("malformed copy rejected");
            assert_eq!(err.code, "fauna.protocol.malformed");
        }
        let copies = state
            .db
            .list_spam_model_holder_copies(&holder)
            .await
            .unwrap();
        assert_eq!(
            copies[0].1,
            vec![0xC2; 96],
            "rejected writes changed nothing"
        );
    }

    /// Opt-out (`set_baseline_contribution(false)`) and model reset both
    /// delete the actor's sealed-to-holder copies — the at-rest artifact
    /// follows the consent / the model it mirrors (`mail-spam.md`
    /// § Encrypted-mode interaction, ratified 2026-07-13).
    #[tokio::test]
    async fn opt_out_and_reset_delete_holder_copies() {
        let state = fixture_state().await;
        let user = [91u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let holder = [0x45u8; 32];
        let seed = |st: Arc<crate::routes::AppState>| async move {
            call_put_spam_model(
                st,
                user,
                PutSpamModelRequest {
                    sealed_model: vec![0xEE; 32],
                    holder_copy: Some(fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                        holder_pubkey: holder.to_vec(),
                        sealed_copy: vec![0xC1; 32],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("seed copy");
        };

        // Opt-out deletes.
        seed(state.clone()).await;
        call_set_baseline_contribution(state.clone(), user, true)
            .await
            .expect("opt in");
        assert_eq!(
            state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .unwrap()
                .len(),
            1
        );
        call_set_baseline_contribution(state.clone(), user, false)
            .await
            .expect("opt out");
        assert!(
            state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .unwrap()
                .is_empty(),
            "opt-out deletes the copies"
        );

        // Reset deletes.
        seed(state.clone()).await;
        assert_eq!(
            state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .unwrap()
                .len(),
            1
        );
        let payload = Bytes::from(
            encode_canonical(&ResetSpamModelRequest::default())
                .unwrap()
                .to_vec(),
        );
        reset_spam_model_handler()(state.clone(), user, payload)
            .await
            .expect("reset ok");
        assert!(
            state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .unwrap()
                .is_empty(),
            "reset deletes the copies"
        );
    }

    /// The opaque write stores the client's sealed blob VERBATIM into the
    /// caller's own `spam_models` row — no decode, no re-seal, byte-for-byte —
    /// and records zero counts (an opaque blob carries no nest-visible per-class
    /// counts). The reply is the empty ack.
    #[tokio::test]
    async fn put_spam_model_stores_opaque_blob_verbatim() {
        let state = fixture_state().await;
        let user = [90u8; 32]; // a bare actor ⇒ CallerClass::User
        state.db.create_user(&user, "free", "test").await.unwrap();
        // An arbitrary opaque payload standing in for a client-re-sealed model —
        // deliberately NOT valid serde_json, to prove the nest never decodes it.
        let sealed = vec![0xEEu8; 300];
        let reply = call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: sealed.clone(),
                sample_count: 7,
                ..Default::default()
            },
        )
        .await
        .expect("a User may write their own model");
        assert_eq!(reply, PutSpamModelReply::default(), "empty ack reply");

        let (stored, ham, spam) = state
            .db
            .get_spam_model(&user)
            .await
            .unwrap()
            .expect("row exists after put");
        assert_eq!(stored, sealed, "the sealed blob is stored byte-for-byte");
        assert_eq!((ham, spam), (0, 0), "opaque blob ⇒ no nest-visible counts");
    }

    /// Caller-scoped by construction: the connection's actor is the subject (no
    /// target field), so a put writes ONLY the caller's row and cannot touch
    /// another actor's model — there is no cross-actor write surface.
    #[tokio::test]
    async fn put_spam_model_writes_only_the_callers_own_row() {
        let state = fixture_state().await;
        let alice = [91u8; 32];
        let bob = [92u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        call_put_spam_model(
            state.clone(),
            alice,
            PutSpamModelRequest {
                sealed_model: vec![0xA1u8; 64],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("alice writes her own model");
        assert!(
            state.db.get_spam_model(&alice).await.unwrap().is_some(),
            "alice's row exists"
        );
        assert!(
            state.db.get_spam_model(&bob).await.unwrap().is_none(),
            "bob's row is untouched — no cross-actor write surface"
        );
    }

    /// An empty blob is rejected (malformed) so it can't silently blank the
    /// model — clearing a model is `reset_spam_model`, not an empty write.
    #[tokio::test]
    async fn put_spam_model_rejects_empty_blob() {
        let state = fixture_state().await;
        let user = [93u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let err = call_put_spam_model(
            state,
            user,
            PutSpamModelRequest {
                sealed_model: Vec::new(),
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect_err("empty sealed_model is rejected");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── put_spam_model BridgeMda write-back (leg 2 — L2b) ──

    /// Leg 2: the MDA (BridgeMda) writes a re-sealed model for the actor it
    /// serves, naming the target in `req.actor_id` (trusted-naming, the same
    /// model as `fetch_spam_model`). The blob lands in
    /// the *named* actor's row, not the bridge's own.
    #[tokio::test]
    async fn put_spam_model_bridge_mda_writes_named_actor() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let served = [70u8; 32]; // the local-mail actor the MDA serves
        let sealed = vec![0xEEu8; 300];
        call_put_spam_model(
            state.clone(),
            mda,
            PutSpamModelRequest {
                sealed_model: sealed.clone(),
                actor_id: served.to_vec(),
                ..Default::default()
            },
        )
        .await
        .expect("the MDA writes the served actor's re-sealed model");
        let (stored, ..) = state
            .db
            .get_spam_model(&served)
            .await
            .unwrap()
            .expect("the served actor's row exists");
        assert_eq!(
            stored, sealed,
            "the sealed blob landed in the NAMED actor's row"
        );
        assert!(
            state.db.get_spam_model(&mda).await.unwrap().is_none(),
            "no row is written for the bridge's own id"
        );
    }

    /// A `User`/`Admin`
    /// naming any OTHER actor in `req.actor_id` is REJECTED — never silently
    /// redirected to self (a buggy privileged client must get an error, not an
    /// `ok` that overwrote its own row). Nothing is written to either row.
    /// Naming *yourself* is equivalent to the empty pre-leg-2 shape.
    #[tokio::test]
    async fn put_spam_model_user_naming_another_actor_is_denied() {
        let state = fixture_state().await;
        let alice = [91u8; 32]; // bare actor ⇒ CallerClass::User
        let victim = [92u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        let err = call_put_spam_model(
            state.clone(),
            alice,
            PutSpamModelRequest {
                sealed_model: vec![0xA1u8; 64],
                actor_id: victim.to_vec(),
                ..Default::default()
            },
        )
        .await
        .expect_err("a User may write only their own model");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        assert!(
            state.db.get_spam_model(&victim).await.unwrap().is_none(),
            "the named victim's row is untouched"
        );
        assert!(
            state.db.get_spam_model(&alice).await.unwrap().is_none(),
            "alice's own row is untouched too — rejected, not redirected to self"
        );

        // Naming self is fine — same as leaving the field empty.
        call_put_spam_model(
            state.clone(),
            alice,
            PutSpamModelRequest {
                sealed_model: vec![0xA1u8; 64],
                actor_id: alice.to_vec(),
                ..Default::default()
            },
        )
        .await
        .expect("naming self is the empty/self shape");
        assert!(state.db.get_spam_model(&alice).await.unwrap().is_some());
    }

    /// Leg 2 atomicity across the trusted-naming path: a BridgeMda write-back
    /// carrying a `history_op: Insert` lands the re-sealed model AND the sealed
    /// audit row on the SERVED actor's rows in one transaction — the shape L2c's
    /// Go `\Junk`-train emits.
    #[tokio::test]
    async fn put_spam_model_bridge_mda_history_op_lands_on_served_actor() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let served = [71u8; 32];
        // `served` is trusted-named by the MDA on the put, but ALSO dispatches
        // `list_spam_training_history` as its own User-class caller below.
        state.db.create_user(&served, "free", "test").await.unwrap();
        let sealed_model = vec![0xB4u8; 128];
        let sealed_subject = vec![0xB5u8; 40];
        let sealed_delta = vec![0xB6u8; 96];
        call_put_spam_model(
            state.clone(),
            mda,
            PutSpamModelRequest {
                actor_id: served.to_vec(),
                sealed_model: sealed_model.clone(),
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: vec![0xB7u8; 32],
                    mailbox: "Junk".into(),
                    sealed_subject: sealed_subject.clone(),
                    sealed_delta: sealed_delta.clone(),
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkFlag,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the MDA writes the served actor's re-sealed model + audit row");
        let (stored, ..) = state
            .db
            .get_spam_model(&served)
            .await
            .unwrap()
            .expect("the served actor's model row exists");
        assert_eq!(stored, sealed_model);
        let reply = call_list_spam_training_history(state.clone(), served, None, None)
            .await
            .expect("list ok");
        assert_eq!(
            reply.events.len(),
            1,
            "the sealed history row lands on the SERVED actor"
        );
        assert_eq!(reply.events[0].sealed_subject, sealed_subject);
        // The list RPC is User-only, so check the bridge's own rows at the
        // DB layer: nothing landed under the MDA service user's id.
        let mda_rows = state
            .db
            .list_spam_training_history(&mda, 10, None)
            .await
            .expect("db list ok");
        assert!(
            mda_rows.is_empty(),
            "no history row for the bridge's own id"
        );
    }

    /// The one-lesson rule (`mail-spam.md` § 3) on the sealed write: an `Insert`
    /// repeating the actor's newest recorded lesson for that message rejects the
    /// WHOLE write — the model stays byte-identical to the last accepted write,
    /// no row lands, and the holder copy riding the same request is not stored
    /// either — and the reply says `duplicate_signal`. An opposite label
    /// re-opens the key; a repeat of THAT collapses again.
    #[tokio::test]
    async fn put_spam_model_history_insert_repeating_the_newest_lesson_rejects_the_whole_write() {
        let state = fixture_state().await;
        let user = [98u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let holder = [0x45u8; 32];
        let msg = vec![0x57u8; 32];
        let put = |model: u8, label: SpamLabel, copy: bool| PutSpamModelRequest {
            sealed_model: vec![model; 64],
            history_op: Some(SpamHistoryOp::Insert {
                message_id: msg.clone(),
                mailbox: "INBOX".into(),
                sealed_subject: vec![0x58u8; 24],
                sealed_delta: vec![0x59u8; 48],
                label,
                source: TrainingSource::ImapJunkFlag,
            }),
            holder_copy: copy.then(|| fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                holder_pubkey: holder.to_vec(),
                sealed_copy: vec![model; 32],
                ..Default::default()
            }),
            ..Default::default()
        };

        // The flag-set lands.
        let first = call_put_spam_model(state.clone(), user, put(0xA1, SpamLabel::Spam, false))
            .await
            .expect("first write ok");
        assert_eq!(first.outcome, PutSpamModelOutcome::Written);

        // The MOVE into Junk repeats the lesson: rejected whole.
        let dup = call_put_spam_model(state.clone(), user, put(0xA2, SpamLabel::Spam, true))
            .await
            .expect("a duplicate is a typed outcome, not an error");
        assert_eq!(dup.outcome, PutSpamModelOutcome::DuplicateSignal);
        let (stored, ..) = state.db.get_spam_model(&user).await.unwrap().unwrap();
        assert_eq!(
            stored,
            vec![0xA1u8; 64],
            "the model is the last ACCEPTED write"
        );
        let rows = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .expect("list ok")
            .events;
        assert_eq!(rows.len(), 1, "no second row");
        assert!(
            state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .unwrap()
                .is_empty(),
            "the holder copy riding a rejected write is not stored either"
        );

        // Moving it back out of Junk is a new lesson (ham re-opens the key)…
        let ham = call_put_spam_model(state.clone(), user, put(0xA3, SpamLabel::Ham, false))
            .await
            .expect("ham write ok");
        assert_eq!(ham.outcome, PutSpamModelOutcome::Written);
        // …and marking it junk once more is a third lesson, whose repeat collapses.
        let spam_again =
            call_put_spam_model(state.clone(), user, put(0xA4, SpamLabel::Spam, false))
                .await
                .expect("spam-again write ok");
        assert_eq!(spam_again.outcome, PutSpamModelOutcome::Written);
        let dup_again = call_put_spam_model(state.clone(), user, put(0xA5, SpamLabel::Spam, false))
            .await
            .expect("typed outcome");
        assert_eq!(dup_again.outcome, PutSpamModelOutcome::DuplicateSignal);
        let (stored, ..) = state.db.get_spam_model(&user).await.unwrap().unwrap();
        assert_eq!(stored, vec![0xA4u8; 64]);
        let rows = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .expect("list ok")
            .events;
        assert_eq!(rows.len(), 3, "spam, ham, spam");
    }

    /// The definition of done for the sealed-at-rest ruling (2026-10-01 survey
    /// A24): after a training write with a history row, the raw bytes resting in
    /// `spam_models` and `spam_training_history` hold no plaintext — the model
    /// blob is the opaque write (it fails `SpamModel::from_bytes`), the history
    /// table has no `subject` column, the row's `sealed_subject` and delta are
    /// the opaque writes, and no column of either row carries the message's
    /// words.
    #[tokio::test]
    async fn a_training_write_rests_no_plaintext() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let served = [0x91u8; 32];
        let (_uid, msg_id) = seed_message_for_handler_with_hint(
            &state,
            &served,
            "INBOX",
            b"Subject: zqxplaintextword\r\n\r\nzqxbodyword",
            1_700_000_000,
            "",
            b"hint",
        )
        .await;
        let sealed_model = vec![0xB4u8; 128];
        let sealed_subject = vec![0xB5u8; 40];
        let sealed_delta = vec![0xB6u8; 96];
        let outcome = call_put_spam_model(
            state.clone(),
            mda,
            PutSpamModelRequest {
                actor_id: served.to_vec(),
                sealed_model: sealed_model.clone(),
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: msg_id.to_vec(),
                    mailbox: "Junk".into(),
                    sealed_subject: sealed_subject.clone(),
                    sealed_delta: sealed_delta.clone(),
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkFlag,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("the sealed write lands")
        .outcome;
        assert_eq!(outcome, PutSpamModelOutcome::Written);

        let conn = state.db.conn().await;
        let model: Vec<u8> = conn
            .query_row(
                "SELECT model_json FROM spam_models WHERE actor_id = ?1",
                [served.to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model, sealed_model, "the model rests as the opaque write");
        assert!(
            fauna_mail::spam::SpamModel::from_bytes(&model).is_none(),
            "the stored model is not a plaintext SpamModel"
        );
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('spam_training_history')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            !columns.iter().any(|c| c == "subject"),
            "the history table has no plaintext subject column: {columns:?}"
        );
        let (subject, delta): (Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT sealed_subject, model_delta_applied FROM spam_training_history
                 WHERE actor_id = ?1",
                [served.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(subject, sealed_subject);
        assert!(!subject.is_empty(), "the sealed subject is non-empty");
        assert_eq!(delta, sealed_delta);
        assert!(!crate::spam_model_seal::is_plaintext_delta(&delta));
        // No column of either row carries the message's words.
        for table in ["spam_models", "spam_training_history"] {
            let cols: Vec<String> = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            for col in cols {
                let bytes: Vec<Vec<u8>> = conn
                    .prepare(&format!("SELECT CAST({col} AS BLOB) FROM {table}"))
                    .unwrap()
                    .query_map([], |r| r.get::<_, Option<Vec<u8>>>(0))
                    .unwrap()
                    .map(|v| v.unwrap().unwrap_or_default())
                    .collect();
                for b in bytes {
                    for word in [&b"zqxplaintextword"[..], &b"zqxbodyword"[..]] {
                        assert!(
                            !b.windows(word.len()).any(|w| w == word),
                            "`{table}.{col}` rests a plaintext word"
                        );
                    }
                }
            }
        }
    }

    /// `put_spam_model` refuses to put plaintext at rest: a model blob that
    /// decodes as a plaintext `SpamModel`, and a history `Insert` with an empty
    /// sealed subject, an empty delta, or a delta that decodes as the plaintext
    /// n-gram set — each `invalid_params`, and nothing is written.
    #[tokio::test]
    async fn put_spam_model_refuses_plaintext() {
        let state = fixture_state().await;
        let user = [0x92u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let insert = |sealed_subject: Vec<u8>, sealed_delta: Vec<u8>| SpamHistoryOp::Insert {
            message_id: vec![0x93u8; 32],
            mailbox: "Junk".into(),
            sealed_subject,
            sealed_delta,
            label: SpamLabel::Spam,
            source: TrainingSource::ManualOther,
        };
        let plaintext_delta = serde_json::to_vec(&fauna_mail::spam::SpamModel::delta_ngrams(
            "buy cheap pills now",
        ))
        .unwrap();
        let cases: Vec<(&str, PutSpamModelRequest)> = vec![
            (
                "a plaintext model blob",
                PutSpamModelRequest {
                    sealed_model: crate::test_support::small_spam_model("buy pills", "lunch", 2)
                        .to_bytes(),
                    ..Default::default()
                },
            ),
            (
                "an empty sealed subject",
                PutSpamModelRequest {
                    sealed_model: vec![0xEEu8; 64],
                    history_op: Some(insert(Vec::new(), vec![0xD1u8; 48])),
                    ..Default::default()
                },
            ),
            (
                "an empty delta",
                PutSpamModelRequest {
                    sealed_model: vec![0xEEu8; 64],
                    history_op: Some(insert(vec![0xC1u8; 24], Vec::new())),
                    ..Default::default()
                },
            ),
            (
                "a plaintext delta",
                PutSpamModelRequest {
                    sealed_model: vec![0xEEu8; 64],
                    history_op: Some(insert(vec![0xC1u8; 24], plaintext_delta)),
                    ..Default::default()
                },
            ),
        ];
        for (what, req) in cases {
            let err = call_put_spam_model(state.clone(), user, req)
                .await
                .expect_err(what);
            assert_eq!(err.code, "fauna.email.invalid_params", "{what}");
        }
        assert!(
            state.db.get_spam_model(&user).await.unwrap().is_none(),
            "no refused write left a model"
        );
        assert!(
            state
                .db
                .list_spam_training_history(&user, 10, None)
                .await
                .unwrap()
                .is_empty(),
            "no refused write left a row"
        );
    }

    /// The MDA path parses `req.actor_id` as the 32-byte target; a short id is
    /// malformed (mirrors `fetch_spam_model`'s trusted-naming parse).
    #[tokio::test]
    async fn put_spam_model_bridge_mda_short_actor_id_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let err = call_put_spam_model(
            state,
            mda,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 64],
                actor_id: vec![1u8; 16], // not 32 bytes
                ..Default::default()
            },
        )
        .await
        .expect_err("a short target actor_id is malformed");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── the sealed model on fetch ──
    //
    // A model a holder wrote via `put_spam_model` rests sealed, and the nest
    // returns it VERBATIM on fetch (never a double seal).

    /// Once the stored model is a client-sealed opaque blob,
    /// `fetch_spam_model` returns it VERBATIM — no seal-on-read (which would
    /// double-seal → the client unwraps once and gets undecodable bytes) and no
    /// baseline merge. A recipient pubkey is provisioned so the pre-fix
    /// seal-on-read path is reachable, proving the opaque short-circuit fires.
    #[tokio::test]
    async fn fetch_spam_model_returns_client_sealed_blob_verbatim() {
        let state = fixture_state().await;
        let user = [94u8; 32]; // bare actor ⇒ CallerClass::User, own model
        state.db.create_user(&user, "free", "test").await.unwrap();
        let msek: [u8; 32] = [0x5e; 32];
        crate::test_support::seed_recipient_seal_key(&state.db, &user, &msek).await;

        // A client-sealed opaque blob (deliberately not valid SpamModel JSON).
        let sealed = vec![0xEEu8; 300];
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: sealed.clone(),
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert_eq!(
            reply.blob.map(|b| b.into_vec()),
            Some(sealed),
            "the sealed model is returned byte-for-byte, not re-sealed"
        );
        assert!(
            reply.stored_sealed,
            "the leg-2 dispatch signal marks the STORED blob sealed"
        );
        assert_eq!(
            reply.baseline, None,
            "no baseline is published, so none rides the reply"
        );
    }

    /// Piece (b1-nest): an **opted-in** contributor whose stored model is
    /// client-sealed gets the deployment-baseline write signal on the
    /// sealed-verbatim reply — `contribute_baseline: true` plus the box's
    /// volunteered content-processor holder seal target (a classical-only holder,
    /// so the X25519 half is present and the ML-KEM ek is `None`). This is the
    /// reply an opted-in client reads to seal + attach its `SpamModelCopyBlob`
    /// (`mail-spam.md` § Wire shapes / § Encrypted-mode interaction).
    #[tokio::test]
    async fn fetch_spam_model_opted_in_volunteers_holder_seal_target() {
        let state = fixture_state().await;
        let user = [110u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        crate::test_support::seed_recipient_seal_key(&state.db, &user, &[0x5e; 32]).await;

        // Enroll an approved content-processor holder (x25519 = [9; 32], no ek).
        approve_bridge(&state.db, &[130u8; 32], BridgeRole::ContentProcessor).await;

        // The user stores a client-sealed model and opts in.
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 300],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");
        call_set_baseline_contribution(state.clone(), user, true)
            .await
            .expect("opt in");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");

        assert!(reply.stored_sealed, "the stored model is client-sealed");
        assert!(reply.contribute_baseline, "the opt-in bit rides the reply");
        let target = reply
            .holder_seal_target
            .expect("an enrolled content-processor holder is volunteered");
        assert_eq!(
            target.x25519_pubkey,
            [9u8; 32].to_vec(),
            "the volunteered target is the holder's attested x25519 pubkey"
        );
        assert_eq!(
            target.mlkem_ek, None,
            "a classical-only holder carries no ML-KEM ek"
        );
    }

    /// The volunteered seal target carries the holder's ML-KEM ek when the
    /// content-processor is an X-Wing (hybrid) holder — the client then seals the
    /// `SpamModelCopyBlob` hybrid.
    #[tokio::test]
    async fn fetch_spam_model_holder_seal_target_carries_mlkem_ek() {
        let state = fixture_state().await;
        let user = [111u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        crate::test_support::seed_recipient_seal_key(&state.db, &user, &[0x5e; 32]).await;

        let holder = [131u8; 32];
        approve_bridge(&state.db, &holder, BridgeRole::ContentProcessor).await;
        let ek = vec![0x42u8; 1184];
        state.db.upsert_bridge_mlkem_ek(&holder, &ek).await.unwrap();

        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 300],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");
        call_set_baseline_contribution(state.clone(), user, true)
            .await
            .expect("opt in");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        let target = reply.holder_seal_target.expect("holder volunteered");
        assert_eq!(
            target.mlkem_ek.map(|e| e.into_vec()),
            Some(ek),
            "the holder's ML-KEM ek rides the seal target for X-Wing sealing"
        );
    }

    /// Opted in but the box has **no** content-processor holder enrolled: the
    /// opt-in bit still rides, but `holder_seal_target` is `None` (the client
    /// attaches no copy — there is nowhere to seal it).
    #[tokio::test]
    async fn fetch_spam_model_opted_in_no_holder_omits_seal_target() {
        let state = fixture_state().await;
        let user = [112u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        crate::test_support::seed_recipient_seal_key(&state.db, &user, &[0x5e; 32]).await;

        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 300],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");
        call_set_baseline_contribution(state.clone(), user, true)
            .await
            .expect("opt in");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(reply.contribute_baseline, "opt-in bit still rides");
        assert_eq!(
            reply.holder_seal_target, None,
            "no holder enrolled ⇒ no seal target volunteered"
        );
    }

    /// The default (opted-out) contributor gets neither signal even when a
    /// content-processor holder IS enrolled: `contribute_baseline: false` and —
    /// because the holder lookup is gated on the opt-in bit — `holder_seal_target:
    /// None`. So a non-contributing MDA-scoring fetch does no holder DB work.
    #[tokio::test]
    async fn fetch_spam_model_opted_out_omits_write_signal() {
        let state = fixture_state().await;
        let user = [113u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        crate::test_support::seed_recipient_seal_key(&state.db, &user, &[0x5e; 32]).await;

        // A holder IS enrolled — the opt-in gate, not holder-absence, withholds it.
        approve_bridge(&state.db, &[132u8; 32], BridgeRole::ContentProcessor).await;

        // No `set_baseline_contribution` call ⇒ default opted-out.
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 300],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(!reply.contribute_baseline, "default is opted-out");
        assert_eq!(
            reply.holder_seal_target, None,
            "gated on the opt-in bit — no holder lookup for a non-contributor"
        );
    }

    /// The nest's own IN-PROCESS web-paywall holder (`web_content::holder`)
    /// self-approves an enrollment row matching the aggregation-holder filter
    /// (`ContentProcessor` + attested X25519) at the earliest `created_at` — but
    /// it must NEVER be resolved as the spam-baseline seal target: its X25519
    /// secret derives from a seed in the nest's data dir, so a contributor copy
    /// sealed to it rests beside its own key (`mail-spam.md` § Encrypted-mode
    /// interaction — "aggregation runs at the granted holder, never nest
    /// in-process"; `encryption-at-rest.md` § Don't do these), and it never
    /// dials in over WS, so the publish poke it wins is always dropped. With a
    /// genuine off-box content-processor also enrolled (later `created_at`),
    /// the resolver must pick the off-box one.
    #[tokio::test]
    async fn resolver_never_selects_the_in_process_web_serve_holder() {
        let state = fixture_state().await;
        let dir = tempfile::tempdir().unwrap();
        let web_serve =
            crate::web_content::holder::WebServeHolder::init(dir.path(), state.db.clone())
                .await
                .unwrap()
                .expect("web-serve holder must init");

        // A genuine off-box content-processor, enrolled AFTER the in-process
        // holder (later created_at — the ordering the bug hid behind).
        let cp_actor = [140u8; 32];
        approve_bridge(&state.db, &cp_actor, BridgeRole::ContentProcessor).await;

        let (holder, target) = resolve_content_processor_holder(&state)
            .await
            .expect("resolve ok")
            .expect("the off-box content-processor is resolvable");
        assert_ne!(
            holder,
            web_serve.ed25519_pubkey(),
            "the in-process web-serve holder must never win the aggregation-holder resolution"
        );
        assert_eq!(holder, cp_actor, "the genuine off-box holder is selected");
        assert_ne!(
            target.x25519_pubkey,
            web_serve.x25519_pubkey.to_vec(),
            "the volunteered seal target must not be the nest's own key"
        );
    }

    /// Fail closed: with ONLY the in-process web-serve holder enrolled, the
    /// resolver returns `None` — no `holder_seal_target` is volunteered (the
    /// client attaches no copy) and no publish run is opened — rather than
    /// telling contributors to seal their spam models to the nest's own key.
    #[tokio::test]
    async fn resolver_returns_none_when_only_the_web_serve_holder_is_enrolled() {
        let state = fixture_state().await;
        let dir = tempfile::tempdir().unwrap();
        crate::web_content::holder::WebServeHolder::init(dir.path(), state.db.clone())
            .await
            .unwrap()
            .expect("web-serve holder must init");

        assert!(
            resolve_content_processor_holder(&state)
                .await
                .expect("resolve ok")
                .is_none(),
            "an in-process holder alone must resolve to None (fail closed)"
        );
        assert!(
            resolve_content_processor_holder_seal_target(&state)
                .await
                .expect("resolve ok")
                .is_none(),
            "no seal target is volunteered when only the in-process holder exists"
        );
    }

    /// The standard-box shape: no dedicated `ContentProcessor` is enrolled
    /// (that role never auto-approves — `mail-bridge-lifecycle.md` § Onboarding
    /// auto-approval), so the resolver falls back to the off-box MDA — the
    /// content-processor-family holder the 2026-07-12 ratification named
    /// (`mail-spam.md` § Encrypted-mode interaction, holder ruling 2026-08-03).
    #[tokio::test]
    async fn resolver_falls_back_to_the_off_box_mda_when_no_dedicated_holder_exists() {
        let state = fixture_state().await;
        let mda = [141u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let (holder, target) = resolve_content_processor_holder(&state)
            .await
            .expect("resolve ok")
            .expect("the off-box MDA is the fallback aggregation holder");
        assert_eq!(
            holder, mda,
            "the MDA is resolved when no dedicated holder exists"
        );
        assert_eq!(
            target.x25519_pubkey,
            vec![9u8; 32],
            "the volunteered seal target is the MDA's attested x25519"
        );
    }

    /// A dedicated off-box `ContentProcessor` outranks the MDA regardless of
    /// enrollment order: the moment an admin approves one (always a manual
    /// approval card), the resolution flips to it and contributor copies
    /// migrate lazily on each next sealed write.
    #[tokio::test]
    async fn resolver_prefers_a_dedicated_content_processor_over_the_mda() {
        let state = fixture_state().await;
        // The MDA enrolls FIRST (earlier created_at — the order a real box has).
        let mda = [142u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let cp = [143u8; 32];
        approve_bridge(&state.db, &cp, BridgeRole::ContentProcessor).await;

        let (holder, _target) = resolve_content_processor_holder(&state)
            .await
            .expect("resolve ok")
            .expect("a holder resolves");
        assert_eq!(
            holder, cp,
            "a dedicated content-processor outranks the MDA even at a later created_at"
        );
    }

    /// The family bound: only content-processor-family roles (`ContentProcessor`,
    /// `Mda`) may hold contributor copies. An MTA (perimeter, no content leg) or
    /// an atproto PDS never resolves, x25519 or not.
    #[tokio::test]
    async fn resolver_ignores_mta_and_atproto_rows() {
        let state = fixture_state().await;
        approve_bridge(&state.db, &[144u8; 32], BridgeRole::Mta).await;
        approve_bridge(&state.db, &[145u8; 32], BridgeRole::AtprotoPds).await;

        assert!(
            resolve_content_processor_holder(&state)
                .await
                .expect("resolve ok")
                .is_none(),
            "neither an MTA nor an atproto PDS is a content-processor-family holder"
        );
    }

    /// Fail closed on a keyless fallback: an MDA without an attested X25519
    /// cannot be sealed to, so it must not resolve (same bound the dedicated
    /// holder already has).
    #[tokio::test]
    async fn resolver_requires_an_attested_x25519_for_the_mda_fallback() {
        let state = fixture_state().await;
        let mda = [146u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mda, BridgeRole::Mda, "b1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mda, None)
            .await
            .unwrap();

        assert!(
            resolve_content_processor_holder(&state)
                .await
                .expect("resolve ok")
                .is_none(),
            "an MDA without an attested x25519 must not be volunteered as a seal target"
        );
    }

    /// A client-sealed stored model cannot be baseline-folded by the nest, so
    /// the published deployment baseline rides the reply's additive `baseline`
    /// field for the AGENT to fold locally (the no-double-fold rule —
    /// `mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-12).
    #[tokio::test]
    async fn fetch_spam_model_sealed_model_carries_published_baseline() {
        let state = fixture_state().await;
        let user = [96u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        // A published (non-empty) deployment baseline.
        let base_model =
            crate::test_support::small_spam_model("qzbasetokwx urgent offer", "ham note", 3);
        state
            .db
            .upsert_spam_baseline(
                &base_model.to_bytes(),
                base_model.ham_messages as i64,
                base_model.spam_messages as i64,
                1,
            )
            .await
            .unwrap();

        // The user's own model is a client-sealed opaque blob.
        let sealed = vec![0xEEu8; 300];
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: sealed.clone(),
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(reply.stored_sealed);
        assert_eq!(
            reply.blob.map(|b| b.into_vec()),
            Some(sealed),
            "the sealed model is still returned verbatim"
        );
        let baseline_bytes = reply
            .baseline
            .expect("the published baseline rides the reply for the agent-side fold")
            .into_vec();
        let baseline = fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes)
            .expect("the baseline field is the plaintext aggregate");
        assert_eq!(
            baseline, base_model,
            "the baseline rides verbatim — the agent applies the fade, not the nest"
        );
    }

    /// A withdrawn (empty) baseline — the k-anon floor's withhold shape — seeds
    /// nothing, so it does not ride the reply.
    #[tokio::test]
    async fn fetch_spam_model_sealed_model_omits_withdrawn_baseline() {
        let state = fixture_state().await;
        let user = [98u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        let empty = fauna_mail::spam::SpamModel::new();
        state
            .db
            .upsert_spam_baseline(&empty.to_bytes(), 0, 0, 0)
            .await
            .unwrap();

        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xEEu8; 300],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("put ok");

        let reply = call_fetch_spam_model(
            state,
            user,
            FetchSpamModelRequest {
                actor_id: user.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(reply.stored_sealed);
        assert_eq!(
            reply.baseline, None,
            "a withdrawn/empty baseline seeds nothing and is omitted"
        );
    }

    /// The cold-start seed (no stored model, a baseline published) is the
    /// nest's read-time fold, so the `baseline` field stays absent — it is
    /// present exactly when the nest did not fold (no-double-fold rule) — and
    /// `stored_sealed` is false: the blob is not the actor's model, and no train
    /// position may train on it.
    #[tokio::test]
    async fn fetch_spam_model_cold_start_seed_omits_baseline_field() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let base_model =
            crate::test_support::small_spam_model("qzbasetokwx pump and dump", "ham note", 3);
        state
            .db
            .upsert_spam_baseline(
                &base_model.to_bytes(),
                base_model.ham_messages as i64,
                base_model.spam_messages as i64,
                1,
            )
            .await
            .unwrap();

        let fresh = [100u8; 32];
        seed_recipient_seal_key(&state.db, &fresh, &[0x44; 32]).await;

        let reply = call_fetch_spam_model(
            state,
            mda,
            FetchSpamModelRequest {
                actor_id: fresh.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(!reply.stored_sealed, "the seed is not a stored model");
        assert!(reply.blob.is_some(), "the sealed-on-read seed is returned");
        assert_eq!(
            reply.baseline, None,
            "the nest already folded — no baseline field (no double fold)"
        );
    }

    // ── publish_spam_baseline + set_baseline_contribution (Slice 5) ──

    /// Publish with the simulated aggregation holder answering the run
    /// (`test_support::publish_through_sim_holder`) — every per-user model
    /// rests sealed, so the holder's merged half is the whole baseline.
    async fn call_publish_spam_baseline(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
    ) -> Result<PublishSpamBaselineReply, fauna_protocol::RpcError> {
        crate::test_support::publish_through_sim_holder(&state, caller).await
    }

    async fn call_set_baseline_contribution(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        contribute: bool,
    ) -> Result<SetBaselineContributionReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(
            encode_canonical(&SetBaselineContributionRequest {
                contribute,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let bytes = set_baseline_contribution_handler()(state, caller, payload).await?;
        Ok(fauna_cbor::decode_strict::<SetBaselineContributionReply>(&bytes).unwrap())
    }

    /// Train an actor's per-user model with `n` spam + `n` ham samples and
    /// store it as their writing agent does — sealed, with a holder copy and
    /// the holder grant (`test_support::seed_sealed_contribution`) — returning
    /// the model so a test can assert the published baseline equals the merge
    /// of the opt-in models.
    async fn seed_spam_model(
        state: &Arc<crate::routes::AppState>,
        actor: &[u8; 32],
        spam_text: &str,
        ham_text: &str,
        n: usize,
    ) -> fauna_mail::spam::SpamModel {
        let m = crate::test_support::small_spam_model(spam_text, ham_text, n);
        crate::test_support::seed_sealed_contribution(state, actor, &m).await;
        m
    }

    /// Only opt-in users' models are merged into the published baseline; an
    /// opt-out user's training never reaches it (`mail-spam.md` § Cold start,
    /// Path 2). The published baseline equals the additive merge of exactly
    /// the opt-in models, and the aggregate `contributors` count reflects them.
    #[tokio::test]
    async fn publish_spam_baseline_aggregates_only_opt_in() {
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // Three opt-in contributors (the k-anonymity floor) + one opt-out.
        let alice = [10u8; 32];
        let bob = [11u8; 32];
        let dave = [13u8; 32];
        let carol = [12u8; 32];
        // Only the three opt-in contributors dispatch the User-class toggle, so
        // only they need a `users` row (carol never dispatches — stays opt-out).
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        state.db.create_user(&dave, "free", "test").await.unwrap();
        let a_model =
            seed_spam_model(&state, &alice, "buy cheap pills now", "lunch agenda", 3).await;
        let b_model = seed_spam_model(
            &state,
            &bob,
            "cheap watches sale",
            "project status update",
            2,
        )
        .await;
        let d_model =
            seed_spam_model(&state, &dave, "limited time offer", "team sync notes", 4).await;
        // carol has a model but does NOT opt in — her unique token must stay out.
        seed_spam_model(
            &state,
            &carol,
            "caroluniquetoken zzqxv",
            "carol ham note",
            5,
        )
        .await;

        call_set_baseline_contribution(state.clone(), alice, true)
            .await
            .expect("alice opts in");
        call_set_baseline_contribution(state.clone(), bob, true)
            .await
            .expect("bob opts in");
        call_set_baseline_contribution(state.clone(), dave, true)
            .await
            .expect("dave opts in");
        // carol stays default-off.

        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin publishes");
        assert_eq!(reply.contributors, 3, "only alice + bob + dave contribute");
        assert!(reply.published, "three contributors reach the k-anon floor");
        assert_eq!(
            reply.sample_count,
            a_model.sample_count() + b_model.sample_count() + d_model.sample_count()
        );

        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("a baseline row exists after publish");
        let baseline =
            fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        let mut expected = fauna_mail::spam::SpamModel::new();
        expected.merge(&a_model);
        expected.merge(&b_model);
        expected.merge(&d_model);
        assert_eq!(baseline, expected, "baseline = merge of opt-in models only");
        assert!(
            !baseline.ngrams.keys().any(|k| k.contains("zzqxv")),
            "an opt-out user's training must not reach the baseline"
        );
    }

    /// Contributors whose models each fit the
    /// `model_max_bytes` cap can still union past it, and the publish must not
    /// produce an over-cap deployment baseline — `publish_spam_baseline` caps
    /// the merged fold so the artifact sealed + scored on every cold-start
    /// fetch stays within the inference budget. The message counters survive
    /// capping, so the aggregate `sample_count` still reflects every
    /// contributor.
    #[tokio::test]
    async fn publish_spam_baseline_caps_the_merged_fold() {
        use fauna_mail::spam::{MODEL_MAX_BYTES_DEFAULT, NgramCount, SpamModel};
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // Two contributors whose models fit the cap one by one, with disjoint
        // n-grams so their union does not.
        let whale = |prefix: &str| {
            let mut m = SpamModel::new();
            for i in 0..20_000u32 {
                m.ngrams
                    .insert(format!("{prefix}{i:06}"), NgramCount { spam: 1, ham: 0 });
            }
            m.spam_messages = 20_000;
            m
        };
        let (w1, w2) = (whale("ngramkeya"), whale("ngramkeyb"));
        let (s1, s2) = (w1.to_bytes().len(), w2.to_bytes().len());
        assert!(s1 <= MODEL_MAX_BYTES_DEFAULT && s2 <= MODEL_MAX_BYTES_DEFAULT);
        assert!(
            s1 + s2 > MODEL_MAX_BYTES_DEFAULT,
            "fixture union must exceed the cap to exercise the path"
        );
        for (actor, model) in [([20u8; 32], &w1), ([21u8; 32], &w2)] {
            state.db.create_user(&actor, "free", "test").await.unwrap();
            crate::test_support::seed_sealed_contribution(&state, &actor, model).await;
            call_set_baseline_contribution(state.clone(), actor, true)
                .await
                .expect("opts in");
        }
        // A third small contributor so the k-anonymity floor is met (the cap
        // path is what's under test, not the floor).
        let minnow = [22u8; 32];
        state.db.create_user(&minnow, "free", "test").await.unwrap();
        let m = seed_spam_model(&state, &minnow, "small spam", "small ham", 1).await;
        call_set_baseline_contribution(state.clone(), minnow, true)
            .await
            .unwrap();

        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin publishes");
        assert_eq!(reply.contributors, 3);
        assert!(reply.published, "three contributors reach the k-anon floor");
        assert_eq!(
            reply.sample_count,
            40_000 + m.sample_count(),
            "counters survive capping"
        );

        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("a baseline row exists after publish");
        assert!(
            baseline_bytes.len() <= MODEL_MAX_BYTES_DEFAULT,
            "published baseline must be capped: {} > {MODEL_MAX_BYTES_DEFAULT}",
            baseline_bytes.len()
        );
    }

    /// publish_spam_baseline is admin-only — a User caller is denied by the
    /// allowlist (`require_class`), never touching the aggregate.
    #[tokio::test]
    async fn publish_spam_baseline_requires_admin() {
        let state = fixture_state().await;
        let user = [82u8; 32]; // a bare, non-bridge actor resolves to User.
        let err = call_publish_spam_baseline(state, user)
            .await
            .expect_err("a non-admin may not publish the baseline");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// With nobody opted in, publish yields an empty baseline (0 contributors,
    /// 0 samples) — overwriting any prior one, the "opt-out removes from the
    /// next republish" contract taken to its limit.
    #[tokio::test]
    async fn publish_spam_baseline_empty_when_no_contributors() {
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        // A trained-but-opted-out user exists; their model must NOT be merged.
        seed_spam_model(&state, &[20u8; 32], "spam token", "ham token", 4).await;

        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin publishes an empty baseline");
        assert_eq!(reply.contributors, 0);
        assert_eq!(reply.sample_count, 0);
        assert!(
            !reply.published,
            "zero contributors ⇒ withdrawn (not published)"
        );
        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("row exists");
        let baseline =
            fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_eq!(baseline, fauna_mail::spam::SpamModel::new());
    }

    /// k-anonymity floor (BASELINE-KANON, `mail-spam.md` § Cold start Path 2):
    /// a baseline aggregated from fewer than `BASELINE_MIN_CONTRIBUTORS` opt-in
    /// contributors is WITHHELD — publishing it would approximate an
    /// individual's model. Below the floor the handler publishes an EMPTY
    /// baseline (withdrawing any prior one), exactly as the zero-contributor
    /// case does, and the reply reports `published = false`. Bumping to the
    /// floor publishes a real baseline.
    #[tokio::test]
    async fn publish_spam_baseline_withholds_below_kanon_floor() {
        use fauna_mail::spam::BASELINE_MIN_CONTRIBUTORS;
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // Two opt-in contributors — below the floor of 3.
        let alice = [50u8; 32];
        let bob = [51u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        seed_spam_model(&state, &alice, "buy cheap pills", "lunch", 3).await;
        seed_spam_model(&state, &bob, "cheap watches", "status", 3).await;
        call_set_baseline_contribution(state.clone(), alice, true)
            .await
            .unwrap();
        call_set_baseline_contribution(state.clone(), bob, true)
            .await
            .unwrap();

        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("publish runs (withholding is not an error)");
        assert_eq!(reply.contributors, 2, "the count still reflects opt-ins");
        assert!(
            !reply.published,
            "a sub-floor baseline must be withheld (published = false)"
        );
        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("row exists");
        let baseline =
            fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_eq!(
            baseline,
            fauna_mail::spam::SpamModel::new(),
            "below the k-anonymity floor the served baseline is empty (withdrawn)"
        );

        // A third opt-in contributor reaches the floor ⇒ a real baseline.
        let carol = [52u8; 32];
        state.db.create_user(&carol, "free", "test").await.unwrap();
        seed_spam_model(&state, &carol, "free crypto", "agenda", 3).await;
        call_set_baseline_contribution(state.clone(), carol, true)
            .await
            .unwrap();
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("publish at the floor");
        assert_eq!(reply.contributors, BASELINE_MIN_CONTRIBUTORS);
        assert!(reply.published, "at the floor the baseline publishes");
        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("row exists");
        let baseline =
            fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_ne!(
            baseline,
            fauna_mail::spam::SpamModel::new(),
            "at/above the floor a non-empty baseline is served"
        );
    }

    /// Holder absent (`mail-spam.md` § Encrypted-mode interaction, ratified
    /// 2026-07-13): opted-in contributors with sealed models are present but no
    /// holder answers the drain. The nest merges nothing itself, so the run
    /// counts nobody, the erosion is HONEST via `skipped_contributors`, and the
    /// below-floor withhold fires on the merged (not opted-in) count.
    #[tokio::test]
    async fn publish_spam_baseline_reports_skipped_sealed_when_no_holder_answers() {
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // Three opted-in contributors (the real write path: an opaque
        // re-sealed model + a sealed-to-holder copy on `put_spam_model`) — but
        // no holder is enrolled, so nobody ever answers the drain.
        for actor in [[70u8; 32], [71u8; 32], [72u8; 32]] {
            state.db.create_user(&actor, "free", "test").await.unwrap();
            call_put_spam_model(
                state.clone(),
                actor,
                PutSpamModelRequest {
                    sealed_model: vec![0xEE; 64],
                    holder_copy: Some(fauna_protocol::wrapped_blob::SpamModelHolderCopy {
                        holder_pubkey: vec![0x44; 32],
                        sealed_copy: vec![0xC1; 96],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("sealed write ok");
            call_set_baseline_contribution(state.clone(), actor, true)
                .await
                .expect("sealed contributor opts in");
        }

        let payload = Bytes::from(
            encode_canonical(&PublishSpamBaselineRequest::default())
                .unwrap()
                .to_vec(),
        );
        let bytes = publish_spam_baseline_handler()(state.clone(), admin, payload)
            .await
            .expect("publish runs (withholding is not an error)");
        let reply = fauna_cbor::decode_strict::<PublishSpamBaselineReply>(&bytes).unwrap();
        assert_eq!(reply.contributors, 0, "the nest merges no model itself");
        assert_eq!(
            reply.skipped_contributors, 3,
            "every sealed contributor is reported, not silently dropped"
        );
        assert!(
            !reply.published,
            "below-floor withholding counts the MERGED contributors, not opt-ins"
        );
        let baseline_bytes = state
            .db
            .get_spam_baseline()
            .await
            .unwrap()
            .expect("row exists");
        let baseline =
            fauna_mail::spam::SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_eq!(
            baseline,
            fauna_mail::spam::SpamModel::new(),
            "the withheld baseline is empty (withdrawn)"
        );
        assert!(
            state.spam_baseline_runs.lock().await.is_empty(),
            "no pending run leaks"
        );
    }

    // ── A contributor's departure withdraws the published baseline ─────────
    //
    // `mail-spam.md` § Cold start Path 2 → *A contributor's departure withdraws
    // the baseline*. Each test publishes over exactly the floor (three
    // contributors), so ONE departure is also the below-the-floor case, and
    // reads the result off BOTH places the baseline is served from.

    const DEPARTURE_MDA: [u8; 32] = [9u8; 32];
    /// A token only `dave` trained, so a test can tell his counts from the rest.
    const DAVE_TOKEN: &str = "davetokqzx";

    /// Admin + three opted-in sealed contributors, published through the
    /// simulated holder. Returns
    /// `(admin, [alice, bob, dave])`.
    async fn publish_over_three_contributors(
        state: &Arc<crate::routes::AppState>,
    ) -> ([u8; 32], [[u8; 32]; 3]) {
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        approve_bridge(&state.db, &DEPARTURE_MDA, BridgeRole::Mda).await;
        let contributors = [[10u8; 32], [11u8; 32], [13u8; 32]];
        let texts = [
            "buy cheap pills now",
            "cheap watches sale",
            "davetokqzx limited offer",
        ];
        for (actor, spam_text) in contributors.iter().zip(texts) {
            state.db.create_user(actor, "free", "test").await.unwrap();
            seed_spam_model(state, actor, spam_text, "team sync notes", 3).await;
            call_set_baseline_contribution(state.clone(), *actor, true)
                .await
                .expect("opts in");
        }
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin publishes");
        assert!(reply.published, "three contributors reach the floor");
        (admin, contributors)
    }

    /// What the two serving paths hand out right now — the shared
    /// `test_support::baseline_as_served`, fetched through `DEPARTURE_MDA`.
    async fn baseline_as_served(
        state: &Arc<crate::routes::AppState>,
        probe: u8,
    ) -> (
        Option<fauna_mail::spam::SpamModel>,
        Option<fauna_mail::spam::SpamModel>,
    ) {
        crate::test_support::baseline_as_served(state, &DEPARTURE_MDA, probe).await
    }

    /// Both serving paths serve a baseline right now (same helper).
    async fn assert_baseline_served(state: &Arc<crate::routes::AppState>, probe: u8) {
        crate::test_support::assert_baseline_served(state, &DEPARTURE_MDA, probe).await;
    }

    /// Neither serving path serves a baseline right now (same helper).
    async fn assert_baseline_withdrawn(state: &Arc<crate::routes::AppState>, probe: u8, why: &str) {
        crate::test_support::assert_baseline_withdrawn(state, &DEPARTURE_MDA, probe, why).await;
    }

    #[tokio::test]
    async fn opt_out_of_a_summed_contributor_withdraws_the_published_baseline() {
        let state = fixture_state().await;
        let (_admin, [_alice, _bob, dave]) = publish_over_three_contributors(&state).await;
        assert_baseline_served(&state, 0x60).await;

        call_set_baseline_contribution(state.clone(), dave, false)
            .await
            .expect("dave opts out");

        assert_baseline_withdrawn(&state, 0x70, "dave opted out, his counts are in the sum").await;
    }

    #[tokio::test]
    async fn model_reset_of_a_summed_contributor_withdraws_the_published_baseline() {
        let state = fixture_state().await;
        let (_admin, [_alice, _bob, dave]) = publish_over_three_contributors(&state).await;
        assert_baseline_served(&state, 0x60).await;

        call_reset_spam_model(state.clone(), dave)
            .await
            .expect("dave resets");

        assert_baseline_withdrawn(&state, 0x70, "dave deleted the model that was summed").await;
    }

    /// The account-deletion leg: the registry walk destroys the two rows that
    /// say the actor was contributing, so it is the walk that must withdraw.
    #[tokio::test]
    async fn account_deletion_of_a_summed_contributor_withdraws_the_published_baseline() {
        let state = fixture_state().await;
        let (_admin, [_alice, _bob, dave]) = publish_over_three_contributors(&state).await;
        assert_baseline_served(&state, 0x60).await;

        state.db.purge_orphaned_actor_rows(&dave).await.unwrap();

        assert_baseline_withdrawn(&state, 0x70, "dave's account is gone").await;
    }

    /// The withdrawal is keyed on the DEPARTING actor standing as a contributor
    /// — not on any departure. Someone who never contributed leaves the
    /// deployment's baseline alone on every one of the three routes.
    #[tokio::test]
    async fn a_non_contributors_departure_leaves_the_published_baseline_served() {
        let state = fixture_state().await;
        publish_over_three_contributors(&state).await;
        let carol = [12u8; 32];
        state.db.create_user(&carol, "free", "test").await.unwrap();
        seed_spam_model(&state, &carol, "carol spam", "carol ham", 3).await;

        call_set_baseline_contribution(state.clone(), carol, false)
            .await
            .expect("carol confirms off");
        call_reset_spam_model(state.clone(), carol)
            .await
            .expect("carol resets");
        state.db.purge_orphaned_actor_rows(&carol).await.unwrap();

        assert_baseline_served(&state, 0x60).await;
    }

    /// Only a SUMMED contributor withdraws — one the last served publish summed
    /// (narrowed 2026-09-22). An actor who opts in after that publish stands as
    /// a contributor but is not in the served sum, so their opt-out leaves it
    /// served: otherwise any mail user could withdraw the deployment's baseline
    /// at will, after every republish, without ever having been in it.
    #[tokio::test]
    async fn opt_out_by_an_actor_who_joined_after_the_publish_leaves_the_baseline_served() {
        let state = fixture_state().await;
        publish_over_three_contributors(&state).await;
        let erin = [14u8; 32];
        join(&state, erin, "erin spam words").await;

        call_set_baseline_contribution(state.clone(), erin, false)
            .await
            .expect("erin opts out");

        assert_baseline_served(&state, 0x60).await;
    }

    /// The reset leg of the same narrowing.
    #[tokio::test]
    async fn model_reset_by_an_actor_who_joined_after_the_publish_leaves_the_baseline_served() {
        let state = fixture_state().await;
        publish_over_three_contributors(&state).await;
        let erin = [14u8; 32];
        join(&state, erin, "erin spam words").await;

        call_reset_spam_model(state.clone(), erin)
            .await
            .expect("erin resets");

        assert_baseline_served(&state, 0x60).await;
    }

    /// A new opt-in cannot bring the old sum back. The withdrawal erases the
    /// sum itself rather than hiding it behind a standing-contributor count, so
    /// a fourth contributor arriving after a departure finds nothing to serve
    /// until a publish rebuilds from the contributors then standing — and that
    /// rebuild no longer holds the departed counts.
    #[tokio::test]
    async fn a_new_opt_in_after_a_departure_does_not_resurrect_the_old_sum() {
        let state = fixture_state().await;
        let (admin, [alice, _bob, dave]) = publish_over_three_contributors(&state).await;
        call_set_baseline_contribution(state.clone(), dave, false)
            .await
            .expect("dave opts out");

        let erin = [14u8; 32];
        state.db.create_user(&erin, "free", "test").await.unwrap();
        seed_spam_model(&state, &erin, "erin spam words", "erin ham", 3).await;
        call_set_baseline_contribution(state.clone(), erin, true)
            .await
            .expect("erin opts in");
        assert_baseline_withdrawn(&state, 0x60, "three stand again, but no publish has run").await;

        // One departure + one join is two changed contributions: below the
        // delta floor, so the republish defers and nothing comes back.
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin republishes");
        assert!(reply.deferred && !reply.published);
        assert_baseline_withdrawn(&state, 0x68, "a deferred run writes nothing").await;

        // A third change (alice trains) lets the rebuild land.
        seed_spam_model(&state, &alice, "alice fresh spam", "alice ham", 3).await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .expect("admin republishes");
        assert!(reply.published && !reply.deferred);
        let (field, folded) = baseline_as_served(&state, 0x70).await;
        for served in [field, folded] {
            let served = served.expect("the republished baseline is served");
            assert!(
                !served.ngrams.keys().any(|k| k.contains(DAVE_TOKEN)),
                "the rebuilt sum holds none of the departed contributor's counts"
            );
        }
    }

    // ── The delta floor + standing publish ───────────────────────────────
    //
    // `mail-spam.md` § Cold start Path 2 → *The floor applies to every
    // published DELTA* and *Standing publish* (ruled 2026-09-21). The cadence
    // is driven by calling its tick with the time meant (convention 14): the
    // test asserts state, never waits.

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    async fn call_get_spam_baseline_state(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
    ) -> Result<GetSpamBaselineStateReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(
            encode_canonical(&GetSpamBaselineStateRequest::default())
                .unwrap()
                .to_vec(),
        );
        let bytes = get_spam_baseline_state_handler()(state, caller, payload).await?;
        // The reply's keys are exactly the ruled seven — no withdrawal time or
        // reason can ride it, now or through `extra`.
        let keys: std::collections::BTreeMap<String, fauna_protocol::Value> =
            fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(
            keys.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "contributors",
                "deferred",
                "published",
                "published_at",
                "sample_count",
                "skipped_contributors",
                "standing",
            ],
            "the admin read carries the baseline's state and nothing about a withdrawal"
        );
        Ok(fauna_cbor::decode_strict::<GetSpamBaselineStateReply>(&bytes).unwrap())
    }

    async fn set_standing_publish(state: &Arc<crate::routes::AppState>, admin: [u8; 32], on: bool) {
        let payload = Bytes::from(
            encode_canonical(&fauna_protocol::bridge_routing::PutSpamPolicyRequest {
                baseline_standing_publish: on,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        crate::bridge_routing_handlers::put_spam_policy_handler()(state.clone(), admin, payload)
            .await
            .expect("admin sets standing publish");
    }

    /// Opt a fresh contributor in with a trained model.
    async fn join(state: &Arc<crate::routes::AppState>, actor: [u8; 32], spam_text: &str) {
        state.db.create_user(&actor, "free", "test").await.unwrap();
        seed_spam_model(state, &actor, spam_text, "team sync notes", 3).await;
        call_set_baseline_contribution(state.clone(), actor, true)
            .await
            .expect("opts in");
    }

    /// The whole ruled flow, each arrow an assertion — the click and the
    /// cadence, both floors, a departure, and the setting turned off.
    #[tokio::test]
    async fn the_delta_floor_and_standing_publish_flow() {
        let state = fixture_state().await;
        let (admin, [alice, bob, dave]) = publish_over_three_contributors(&state).await;
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(read.published && !read.deferred && !read.standing);
        assert_eq!(read.contributors, 3);
        assert!(read.published_at.is_some());

        // A fourth joins: one change, below the delta floor. Deferred — and
        // the old baseline is still served on both paths.
        let erin = [14u8; 32];
        join(&state, erin, "erin spam words").await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(reply.deferred && !reply.published && reply.sample_count == 0);
        assert_eq!(
            reply.contributors, 4,
            "the run still reports what it merged"
        );
        assert_baseline_served(&state, 0x40).await;
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(
            read.published && read.deferred,
            "old baseline served, waiting"
        );
        assert_eq!(
            read.contributors, 3,
            "the served baseline is still the old one"
        );

        // Two standing contributors train: three changes since the served
        // baseline (erin's join, alice, bob). The publish lands.
        seed_spam_model(&state, &alice, "alice more spam", "alice ham", 3).await;
        seed_spam_model(&state, &bob, "bob more spam", "bob ham", 3).await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(reply.published && !reply.deferred);
        assert_eq!(reply.contributors, 4);

        // A contributor deletes their account: withdrawn at once.
        state.db.purge_orphaned_actor_rows(&dave).await.unwrap();
        assert_baseline_withdrawn(&state, 0x48, "dave's account is gone").await;
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(!read.published && read.published_at.is_none());

        // Standing publish on. The cadence's first due run sees one change
        // (dave's purge): deferred, and nothing is served.
        set_standing_publish(&state, admin, true).await;
        let now = crate::db::now_epoch_millis();
        let tick = |at: i64| {
            let state = state.clone();
            async move {
                crate::test_support::run_with_sim_holder(&state.clone(), async move {
                    crate::spam_baseline::run_standing_publish_if_due(&state, at).await
                })
                .await
            }
        };
        let outcome = tick(now + DAY_MS)
            .await
            .unwrap()
            .expect("standing on and a day past the last run: due");
        assert!(outcome.deferred && !outcome.published);
        assert_baseline_withdrawn(&state, 0x50, "a deferred cadence run writes nothing").await;
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(!read.published && read.deferred && read.standing);
        // The run stamps the wall clock, not the `now` handed in above, so a
        // day is measured from the stamp it wrote: a run that finishes inside
        // `now`'s own millisecond (most of them, on a quiet box) would
        // otherwise be a full day old at `now + DAY_MS`. That every way a run
        // ends stamps is pinned in `db::spam_baseline::tests`.
        let ran_at = state
            .db
            .get_spam_baseline_state()
            .await
            .unwrap()
            .1
            .last_run_at
            .expect("the deferred run recorded when it finished");
        assert!(
            crate::spam_baseline::run_standing_publish_if_due(&state, ran_at + DAY_MS - 1)
                .await
                .unwrap()
                .is_none(),
            "not due again until a further day has passed"
        );

        // Two more changes (frank joins, erin trains): the next cadence run
        // lands, and none of the departed contributor's counts are in it.
        join(&state, [15u8; 32], "frank spam words").await;
        seed_spam_model(&state, &erin, "erin more spam", "erin ham", 3).await;
        let outcome = tick(now + 3 * DAY_MS).await.unwrap().expect("due");
        assert!(outcome.published && !outcome.deferred);
        let (field, folded) = baseline_as_served(&state, 0x58).await;
        for served in [field, folded] {
            let served = served.expect("the cadence's baseline is served");
            assert!(!served.ngrams.keys().any(|k| k.contains(DAVE_TOKEN)));
        }
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(read.published && !read.deferred && read.standing);
        assert_eq!(read.contributors, 4);

        // Off: withdrawn, and no further runs.
        set_standing_publish(&state, admin, false).await;
        assert_baseline_withdrawn(&state, 0x5c, "standing publish turned off").await;
        assert!(
            crate::spam_baseline::run_standing_publish_if_due(&state, now + 10 * DAY_MS)
                .await
                .unwrap()
                .is_none(),
            "off never runs"
        );
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(!read.published && !read.standing);
    }

    /// Leaving and rejoining before the next publish counts ONCE: dave's
    /// round trip plus erin's join is two changes, not three.
    #[tokio::test]
    async fn leaving_and_rejoining_counts_once() {
        let state = fixture_state().await;
        let (admin, [alice, _bob, dave]) = publish_over_three_contributors(&state).await;
        call_set_baseline_contribution(state.clone(), dave, false)
            .await
            .unwrap();
        call_set_baseline_contribution(state.clone(), dave, true)
            .await
            .unwrap();
        join(&state, [14u8; 32], "erin spam words").await;

        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(
            reply.deferred,
            "dave's leave-and-rejoin + erin's join = 2 changes"
        );

        seed_spam_model(&state, &alice, "alice fresh spam", "alice ham", 3).await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(reply.published, "the third change lands it");
    }

    /// Sybil-free proof that a deployment's FIRST publish is bound by the
    /// contributor floor alone: three contributors, never a baseline before,
    /// and it lands — there is no earlier baseline to subtract from.
    #[tokio::test]
    async fn the_first_publish_needs_only_the_contributor_floor() {
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        // A withheld run first (two contributors): it writes an empty baseline
        // but sets no reference, so the next run is still a first publish.
        join(&state, [20u8; 32], "first spam one").await;
        join(&state, [21u8; 32], "first spam two").await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(!reply.published && !reply.deferred);
        join(&state, [22u8; 32], "first spam three").await;
        let reply = call_publish_spam_baseline(state.clone(), admin)
            .await
            .unwrap();
        assert!(reply.published && !reply.deferred);
        assert_eq!(reply.contributors, 3);
    }

    /// Only on → off withdraws: saving "off" over "off" does not, and a save
    /// that restates "on" keeps the baseline served.
    #[tokio::test]
    async fn only_on_to_off_withdraws_the_baseline() {
        let state = fixture_state().await;
        let (admin, _) = publish_over_three_contributors(&state).await;

        // Off over off (an ordinary spam-policy save): the published baseline
        // stays — it was published by a click, not by standing publish.
        set_standing_publish(&state, admin, false).await;
        assert_baseline_served(&state, 0x40).await;

        set_standing_publish(&state, admin, true).await;
        set_standing_publish(&state, admin, true).await;
        let read = call_get_spam_baseline_state(state.clone(), admin)
            .await
            .unwrap();
        assert!(read.standing, "restating on keeps it on");
        assert_baseline_served(&state, 0x48).await;

        set_standing_publish(&state, admin, false).await;
        assert_baseline_withdrawn(&state, 0x50, "on → off withdraws").await;
    }

    /// The state read is admin-only.
    #[tokio::test]
    async fn get_spam_baseline_state_requires_admin() {
        let state = fixture_state().await;
        let user = [2u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let payload = Bytes::from(
            encode_canonical(&GetSpamBaselineStateRequest::default())
                .unwrap()
                .to_vec(),
        );
        assert!(
            get_spam_baseline_state_handler()(state, user, payload)
                .await
                .is_err()
        );
    }

    /// The per-user opt-in toggle persists (and the reply echoes the new
    /// value), and is caller-scoped: setting alice's flag never touches bob's.
    #[tokio::test]
    async fn set_baseline_contribution_persists_and_is_caller_scoped() {
        let state = fixture_state().await;
        let alice = [30u8; 32];
        let bob = [31u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();

        // Default off.
        assert!(
            !state
                .db
                .get_spam_preferences(&alice)
                .await
                .unwrap()
                .contribute_baseline
        );

        let reply = call_set_baseline_contribution(state.clone(), alice, true)
            .await
            .expect("alice opts in");
        assert!(reply.contribute, "reply echoes the new value");
        assert!(
            state
                .db
                .get_spam_preferences(&alice)
                .await
                .unwrap()
                .contribute_baseline
        );
        // bob is untouched (caller-scoped).
        assert!(
            !state
                .db
                .get_spam_preferences(&bob)
                .await
                .unwrap()
                .contribute_baseline
        );

        // Opting back out persists too.
        call_set_baseline_contribution(state.clone(), alice, false)
            .await
            .expect("alice opts out");
        assert!(
            !state
                .db
                .get_spam_preferences(&alice)
                .await
                .unwrap()
                .contribute_baseline
        );
    }

    /// set_baseline_contribution preserves the actor's other spam preferences
    /// (it shares the `spam_preferences` row with `fauna.spam.set_preferences`).
    #[tokio::test]
    async fn set_baseline_contribution_preserves_other_prefs() {
        let state = fixture_state().await;
        let alice = [32u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        let mut prefs = state.db.get_spam_preferences(&alice).await.unwrap();
        prefs.phishing_threshold = 0.7;
        prefs.spam_threshold = 0.9;
        state
            .db
            .upsert_spam_preferences(&alice, &prefs)
            .await
            .unwrap();

        call_set_baseline_contribution(state.clone(), alice, true)
            .await
            .expect("opt in");

        let after = state.db.get_spam_preferences(&alice).await.unwrap();
        assert!(after.contribute_baseline);
        assert_eq!(
            after.phishing_threshold, 0.7,
            "phishing threshold preserved"
        );
        assert_eq!(after.spam_threshold, 0.9, "threshold preserved");
    }

    // ── Slice 5 rest — reset / list / undo training-management RPCs ──────
    //
    // The user-tier RPCs that light up the `mail-spam` page (`mail-spam.md`
    // §§ Reset, Training-sample retention, Undo). Each is caller-scoped by
    // construction (the connection's actor is the subject — no `actor_id`
    // field), so the cross-actor guard is structural; the tests confirm one
    // actor's reset/undo never touches another's model or history. An undo is
    // the client's own `put_spam_model` with a history `Delete`.

    async fn call_reset_spam_model(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
    ) -> Result<ResetSpamModelReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(
            encode_canonical(&ResetSpamModelRequest::default())
                .unwrap()
                .to_vec(),
        );
        let bytes = reset_spam_model_handler()(state, caller, payload).await?;
        Ok(fauna_cbor::decode_strict::<ResetSpamModelReply>(&bytes).unwrap())
    }

    async fn call_list_spam_training_history(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        limit: Option<u32>,
        before_history_id: Option<Vec<u8>>,
    ) -> Result<ListSpamTrainingHistoryReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(
            encode_canonical(&ListSpamTrainingHistoryRequest {
                limit,
                before_history_id: before_history_id.map(ByteBuf::from),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let bytes = list_spam_training_history_handler()(state, caller, payload).await?;
        Ok(fauna_cbor::decode_strict::<ListSpamTrainingHistoryReply>(&bytes).unwrap())
    }

    /// Train one event the way the MDA's agent-side `\Junk` train lands it
    /// (seeding the placed message first): a re-sealed model plus one sealed
    /// history row through `put_spam_model`. Returns the sealed delta the row
    /// carries.
    async fn train_one_event(
        state: &Arc<crate::routes::AppState>,
        mda: [u8; 32],
        actor: [u8; 32],
        mailbox: &str,
        body: &[u8],
        label: SpamLabel,
        source: TrainingSource,
    ) -> Vec<u8> {
        let (_uid, msg_id) = seed_message_for_handler_with_hint(
            state,
            &actor,
            mailbox,
            body,
            1_700_000_000,
            "",
            b"h",
        )
        .await;
        let sealed_delta = [&[0xD7u8; 16][..], &msg_id[..]].concat();
        let reply = call_put_spam_model(
            state.clone(),
            mda,
            PutSpamModelRequest {
                actor_id: actor.to_vec(),
                sealed_model: [&[0xEEu8; 16][..], &msg_id[..]].concat(),
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: msg_id.to_vec(),
                    mailbox: mailbox.into(),
                    sealed_subject: [&[0xC7u8; 16][..], &msg_id[..]].concat(),
                    sealed_delta: sealed_delta.clone(),
                    label,
                    source,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("train ok");
        assert_eq!(reply.outcome, PutSpamModelOutcome::Written);
        sealed_delta
    }

    /// One lesson writes one row: the train-time mailbox, label and source in
    /// the clear, the subject only sealed — so `message` carries the mailbox
    /// alone and a client renders `{unwrapped subject} · {mailbox}`.
    #[tokio::test]
    async fn a_lesson_writes_a_history_row_with_mailbox_label_source() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let alice = [40u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        train_one_event(
            &state,
            mda,
            alice,
            "Junk",
            b"Subject: Win a free prize\r\n\r\nclick now to claim",
            SpamLabel::Spam,
            TrainingSource::ImapJunkMove,
        )
        .await;

        let reply = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .expect("list ok");
        assert_eq!(reply.events.len(), 1, "one training event recorded");
        let row = &reply.events[0];
        assert_eq!(row.label, SpamLabel::Spam);
        assert_eq!(row.source, TrainingSource::ImapJunkMove);
        assert_eq!(row.message, "Junk", "the message carries the mailbox alone");
        assert_eq!(row.mailbox, "Junk");
        assert!(!row.sealed_subject.is_empty(), "the subject rests sealed");
        assert!(!row.history_id.is_empty());
        assert!(
            !reply.contribute_baseline,
            "contribute_baseline read-back defaults off"
        );
    }

    #[tokio::test]
    async fn list_returns_model_delta_applied_for_client_side_undo() {
        // `list_spam_training_history` returns each row's stored
        // `model_delta_applied` — the sealed delta, verbatim — so a client can
        // unwrap it and replay its inverse for an undo (co-design § 3).
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let alice = [42u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        let body = b"Subject: Win a free prize\r\n\r\nclick now to claim";
        let expected = train_one_event(
            &state,
            mda,
            alice,
            "Junk",
            body,
            SpamLabel::Spam,
            TrainingSource::ImapJunkMove,
        )
        .await;

        let reply = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .expect("list ok");
        assert_eq!(reply.events.len(), 1);
        let row = &reply.events[0];
        assert_eq!(
            row.model_delta_applied, expected,
            "the row carries the stored sealed delta verbatim"
        );
    }

    /// build-item 3 WRITE side: a client-path train rides an `Insert` history_op
    /// on `put_spam_model` — the re-sealed model + a client-sealed audit row land
    /// atomically. `list_spam_training_history` returns the sealed columns opaque
    /// (`sealed_subject` + the sealed delta on `model_delta_applied`) and the
    /// mailbox separately; `message` degrades to the mailbox alone (the nest can't
    /// format a sealed subject — a 1c client unwraps `sealed_subject`).
    #[tokio::test]
    async fn put_spam_model_history_op_insert_writes_a_sealed_row() {
        let state = fixture_state().await;
        let user = [97u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let sealed_model = vec![0xE1u8; 128];
        let sealed_subject = vec![0x51u8; 40];
        let sealed_delta = vec![0x52u8; 96];
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: sealed_model.clone(),
                sample_count: 3,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: vec![0x53u8; 32],
                    mailbox: "Junk".into(),
                    sealed_subject: sealed_subject.clone(),
                    sealed_delta: sealed_delta.clone(),
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkMove,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("a User writes their own re-sealed model + sealed history row");

        // The re-sealed model was stored verbatim (opaque, atomic with the row).
        let (stored, ..) = state
            .db
            .get_spam_model(&user)
            .await
            .unwrap()
            .expect("model row exists");
        assert_eq!(
            stored, sealed_model,
            "the re-sealed model is stored verbatim"
        );

        // The sealed audit row round-trips through list with opaque content columns.
        let reply = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .expect("list ok");
        assert_eq!(reply.events.len(), 1, "one client-written history row");
        let row = &reply.events[0];
        assert_eq!(row.label, SpamLabel::Spam);
        assert_eq!(row.source, TrainingSource::ImapJunkMove);
        assert_eq!(
            row.mailbox, "Junk",
            "the train-time mailbox is returned separately for the sealed-row display"
        );
        assert_eq!(
            row.sealed_subject, sealed_subject,
            "the client-sealed subject is returned opaque"
        );
        assert_eq!(
            row.model_delta_applied, sealed_delta,
            "the client-sealed delta rides the model_delta_applied column opaque"
        );
        assert_eq!(
            row.message, "Junk",
            "a sealed row's message degrades to the mailbox (nest can't read the subject)"
        );
        assert!(!row.history_id.is_empty());
    }

    /// A client-path undo rides a `Delete` history_op: the re-sealed
    /// (inverse-applied) model + the removal of the consumed audit row, atomically.
    #[tokio::test]
    async fn put_spam_model_history_op_delete_removes_the_row() {
        let state = fixture_state().await;
        let user = [98u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xE2u8; 96],
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: vec![0x60u8; 32],
                    mailbox: "Junk".into(),
                    sealed_subject: vec![0x61u8; 32],
                    sealed_delta: vec![0x62u8; 48],
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkFlag,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("seed a client-written row");
        let listed = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .unwrap();
        assert_eq!(listed.events.len(), 1);
        let history_id = listed.events[0].history_id.clone();

        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xE3u8; 96],
                sample_count: 0,
                history_op: Some(SpamHistoryOp::Delete {
                    history_id: history_id.clone(),
                }),
                ..Default::default()
            },
        )
        .await
        .expect("a client-path undo deletes the row");

        let after = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .unwrap();
        assert!(after.events.is_empty(), "the undo removed the audit row");
        let (stored, ..) = state
            .db
            .get_spam_model(&user)
            .await
            .unwrap()
            .expect("model row exists");
        assert_eq!(
            stored,
            vec![0xE3u8; 96],
            "the inverse-applied re-sealed model is written atomically with the delete"
        );
    }

    /// A model-only write (`history_op = None`, a re-seal with no per-event
    /// history) replaces the model but leaves the training history untouched.
    #[tokio::test]
    async fn put_spam_model_without_history_op_leaves_history_untouched() {
        let state = fixture_state().await;
        let user = [99u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xE4u8; 64],
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: vec![0x70u8; 32],
                    mailbox: "INBOX".into(),
                    sealed_subject: vec![0x71u8; 16],
                    sealed_delta: vec![0x72u8; 24],
                    label: SpamLabel::Ham,
                    source: TrainingSource::ManualOther,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("seed a row");
        call_put_spam_model(
            state.clone(),
            user,
            PutSpamModelRequest {
                sealed_model: vec![0xE5u8; 64],
                sample_count: 0,
                ..Default::default()
            },
        )
        .await
        .expect("a model-only put");
        let reply = call_list_spam_training_history(state.clone(), user, None, None)
            .await
            .unwrap();
        assert_eq!(
            reply.events.len(),
            1,
            "a model-only write leaves the history row in place"
        );
        let (stored, ..) = state.db.get_spam_model(&user).await.unwrap().unwrap();
        assert_eq!(
            stored,
            vec![0xE5u8; 64],
            "the model-only write still replaced the model"
        );
    }

    /// A `Delete` history_op is caller-scoped (`WHERE actor_id = caller`): one
    /// actor cannot delete another actor's history row (no cross-actor undo).
    #[tokio::test]
    async fn put_spam_model_history_op_delete_is_caller_scoped() {
        let state = fixture_state().await;
        let alice = [100u8; 32];
        let bob = [101u8; 32];
        // Both actors dispatch as their own User-class caller (alice writes +
        // lists; bob attempts the cross-actor delete), so both need a `users` row.
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        call_put_spam_model(
            state.clone(),
            alice,
            PutSpamModelRequest {
                sealed_model: vec![0xE6u8; 64],
                sample_count: 1,
                history_op: Some(SpamHistoryOp::Insert {
                    message_id: vec![0x80u8; 32],
                    mailbox: "Junk".into(),
                    sealed_subject: vec![0x81u8; 16],
                    sealed_delta: vec![0x82u8; 24],
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkFlag,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("alice writes a client-sealed row");
        let alice_history_id = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .unwrap()
            .events[0]
            .history_id
            .clone();

        // Bob tries to delete Alice's row — caller-scoped, so it's a clean no-op.
        call_put_spam_model(
            state.clone(),
            bob,
            PutSpamModelRequest {
                sealed_model: vec![0xE7u8; 64],
                sample_count: 0,
                history_op: Some(SpamHistoryOp::Delete {
                    history_id: alice_history_id.clone(),
                }),
                ..Default::default()
            },
        )
        .await
        .expect("bob's cross-actor delete is a clean no-op ack");
        let alice_after = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .unwrap();
        assert_eq!(
            alice_after.events.len(),
            1,
            "alice's row survives bob's cross-actor delete attempt"
        );
    }

    #[tokio::test]
    async fn list_spam_training_history_reads_back_contribute_baseline() {
        let state = fixture_state().await;
        let alice = [41u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        // No training yet → empty list, flag default-off.
        let r0 = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .expect("list ok");
        assert!(r0.events.is_empty());
        assert!(!r0.contribute_baseline);

        call_set_baseline_contribution(state.clone(), alice, true)
            .await
            .expect("opt in");
        let r1 = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .expect("list ok");
        assert!(
            r1.contribute_baseline,
            "the toggle read-back reflects the persisted opt-in"
        );
    }

    #[tokio::test]
    async fn reset_spam_model_deletes_model_and_history() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let alice = [42u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        train_one_event(
            &state,
            mda,
            alice,
            "INBOX",
            b"Subject: pills\r\n\r\nbuy cheap pills now",
            SpamLabel::Spam,
            TrainingSource::ImapJunkFlag,
        )
        .await;
        assert!(
            state.db.get_spam_model(&alice).await.unwrap().is_some(),
            "model row exists after train"
        );

        call_reset_spam_model(state.clone(), alice)
            .await
            .expect("reset ok");

        assert!(
            state.db.get_spam_model(&alice).await.unwrap().is_none(),
            "model row deleted by reset"
        );
        let after = call_list_spam_training_history(state.clone(), alice, None, None)
            .await
            .expect("list ok");
        assert!(after.events.is_empty(), "history cleared by reset");
    }

    #[tokio::test]
    async fn undo_and_reset_are_caller_scoped_to_own_history() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let alice = [44u8; 32];
        let bob = [45u8; 32];
        // Both dispatch as their own User-class caller (alice undoes + resets;
        // bob lists its own history), so both need a `users` row.
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        train_one_event(
            &state,
            mda,
            alice,
            "Junk",
            b"Subject: pills\r\n\r\nbuy cheap pills now",
            SpamLabel::Spam,
            TrainingSource::ImapJunkFlag,
        )
        .await;
        train_one_event(
            &state,
            mda,
            bob,
            "Junk",
            b"Subject: watches\r\n\r\ncheap watches sale",
            SpamLabel::Spam,
            TrainingSource::ImapJunkFlag,
        )
        .await;

        // Alice cannot see Bob's history; her undo of Bob's history_id is a no-op.
        let bob_list = call_list_spam_training_history(state.clone(), bob, None, None)
            .await
            .expect("list ok");
        let bob_row = bob_list.events[0].history_id.clone();
        call_put_spam_model(
            state.clone(),
            alice,
            PutSpamModelRequest {
                sealed_model: vec![0xE8u8; 64],
                history_op: Some(SpamHistoryOp::Delete {
                    history_id: bob_row,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("no-op ack, not an error");
        assert!(
            state.db.get_spam_model(&bob).await.unwrap().is_some(),
            "alice's undo did not touch bob's model"
        );
        assert_eq!(
            call_list_spam_training_history(state.clone(), bob, None, None)
                .await
                .expect("list ok")
                .events
                .len(),
            1,
            "bob's history row survives alice's cross-actor undo attempt"
        );

        // Alice's reset clears only Alice's data.
        call_reset_spam_model(state.clone(), alice)
            .await
            .expect("reset ok");
        assert!(
            state.db.get_spam_model(&bob).await.unwrap().is_some(),
            "bob's model survives alice's reset"
        );
        assert!(
            state.db.get_spam_model(&alice).await.unwrap().is_none(),
            "alice's model is gone"
        );
    }

    #[tokio::test]
    async fn list_spam_training_history_paginates_newest_first() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let alice = [46u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        // Insert three history rows directly (the write stamps `now`; a tiny
        // gap between them keeps the newest-first order + keyset cursor
        // deterministic).
        let mut ids = Vec::new();
        for i in 0..3u8 {
            let crate::db::moderation::SpamModelWrite::Written {
                history_id: Some(id),
                ..
            } = state
                .db
                .put_spam_model_with_history(
                    &alice,
                    &[0xEEu8; 64],
                    Some(crate::db::moderation::SpamHistoryDbOp::Insert {
                        message_id: &[i; 32],
                        mailbox: "INBOX",
                        sealed_subject: &[0xC1u8; 16],
                        sealed_delta: &[0xD1u8; 16],
                        label: "spam",
                        source: "imap_junk_flag",
                    }),
                    None,
                )
                .await
                .unwrap()
            else {
                panic!("an insert mints a history id");
            };
            ids.push(id);
            // tiny gap so created_at differs
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        let page1 = call_list_spam_training_history(state.clone(), alice, Some(2), None)
            .await
            .expect("list ok");
        assert_eq!(page1.events.len(), 2, "limit honored");
        // Newest first: the last-inserted id is first.
        assert_eq!(page1.events[0].history_id, *ids.last().unwrap());

        // Keyset: rows older than the last row of page 1.
        let cursor = page1.events[1].history_id.clone();
        let page2 = call_list_spam_training_history(state.clone(), alice, Some(2), Some(cursor))
            .await
            .expect("list ok");
        assert_eq!(page2.events.len(), 1, "one older row remains");
        assert_eq!(page2.events[0].history_id, ids[0]);
    }

    // ── Slice 5b — the cold-start seed in fetch ──────────────────────
    //
    // The CONSUME half of the admin-opt-in deployment baseline
    // (`mail-spam.md` § Cold start, Path 2 step 4). An actor with a stored
    // (sealed) model gets the baseline on the reply's `baseline` field and the
    // agent folds it (the sealed-model tests above); a FRESH actor gets the
    // nest's read-time seed — the baseline folded onto a fresh model, sealed on
    // read, never persisted.

    /// A FRESH actor (no own model) whose deployment has a published baseline
    /// inherits the FULL baseline at read time — the cold-start seed — so a
    /// fresh actor scores against the deployment's shared spam knowledge from
    /// message one.
    #[tokio::test]
    async fn fetch_spam_model_cold_start_seeds_from_published_baseline() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Seed the deployment baseline directly — this test exercises the
        // cold-start CONSUME/fade path, not the publish k-anonymity floor, so a
        // single-contributor baseline keeps the "full baseline == that model"
        // assertion clean (publishing it would be withheld below the floor).
        let a_model =
            crate::test_support::small_spam_model("qzbasetokwx urgent offer", "ham note", 3);
        state
            .db
            .upsert_spam_baseline(
                &a_model.to_bytes(),
                a_model.ham_messages as i64,
                a_model.spam_messages as i64,
                1,
            )
            .await
            .unwrap();

        // carol is fresh — no `spam_models` row of her own.
        let carol = [41u8; 32];
        let msek = [0x40; 32];
        seed_recipient_seal_key(&state.db, &carol, &msek).await;
        let reply = call_fetch_spam_model(
            state.clone(),
            mda,
            FetchSpamModelRequest {
                actor_id: carol.to_vec(),
                extra: Default::default(),
            },
        )
        .await
        .expect("fetch ok");
        assert!(!reply.stored_sealed, "the seed is not the actor's model");
        let sealed = reply
            .blob
            .expect("a fresh actor inherits the published baseline");

        let model = unseal_fetched_model(&sealed, &msek);
        // The full baseline (fade fraction 1 at 0 own samples) == alice's model
        // (single contributor), so carol's read-time model equals the baseline.
        assert_eq!(
            model, a_model,
            "cold-start actor inherits the full baseline"
        );
        assert!(
            model.ngrams.keys().any(|k| k.contains("qzbasetokwx")),
            "the baseline's distinctive spam token seeds the fresh actor"
        );
        assert_eq!(model.sample_count(), a_model.sample_count());
    }

    // ── I5 Phase D.6 — create / delete / rename handler tests ──────

    async fn call_create_mailbox(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        req: fauna_protocol::bridge_routing::CreateMailboxRequest,
    ) -> Result<fauna_protocol::bridge_routing::CreateMailboxReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = create_mailbox_handler()(state, caller, payload).await?;
        Ok(
            fauna_cbor::decode_strict::<fauna_protocol::bridge_routing::CreateMailboxReply>(&bytes)
                .unwrap(),
        )
    }

    async fn call_delete_mailbox(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        req: fauna_protocol::bridge_routing::DeleteMailboxRequest,
    ) -> Result<fauna_protocol::bridge_routing::DeleteMailboxReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = delete_mailbox_handler()(state, caller, payload).await?;
        Ok(
            fauna_cbor::decode_strict::<fauna_protocol::bridge_routing::DeleteMailboxReply>(&bytes)
                .unwrap(),
        )
    }

    async fn call_rename_mailbox(
        state: Arc<crate::routes::AppState>,
        caller: [u8; 32],
        req: fauna_protocol::bridge_routing::RenameMailboxRequest,
    ) -> Result<fauna_protocol::bridge_routing::RenameMailboxReply, fauna_protocol::RpcError> {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = rename_mailbox_handler()(state, caller, payload).await?;
        Ok(
            fauna_cbor::decode_strict::<fauna_protocol::bridge_routing::RenameMailboxReply>(&bytes)
                .unwrap(),
        )
    }

    // ── CREATE ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_mailbox_inserts_fresh_state_row() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [80u8; 32];

        let reply = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Projects".into(),
            },
        )
        .await
        .expect("create handler ok");
        let uid_validity = match reply {
            fauna_protocol::bridge_routing::CreateMailboxReply::Created { uid_validity } => {
                uid_validity
            }
            other => panic!("expected Created, got {other:?}"),
        };
        assert!(uid_validity >= 2, "uid_validity must be > 1 (reserved)");

        // The mailbox is now present in the actor's state list.
        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        let projects = rows
            .iter()
            .find(|r| r.name == "Projects")
            .expect("Projects row");
        assert_eq!(projects.uid_validity, uid_validity);
        assert_eq!(projects.uid_next, 1);
        assert_eq!(projects.highestmodseq, 1);
    }

    #[tokio::test]
    async fn create_mailbox_reserved_name_rejects() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [81u8; 32];

        for reserved in ["INBOX", "Archive", "Drafts", "Sent", "Trash", "Junk"] {
            let reply = call_create_mailbox(
                state.clone(),
                mda,
                fauna_protocol::bridge_routing::CreateMailboxRequest {
                    actor_id: target.to_vec(),
                    name: reserved.into(),
                },
            )
            .await
            .expect("create handler ok");
            assert_eq!(
                reply,
                fauna_protocol::bridge_routing::CreateMailboxReply::Reserved,
                "{reserved} must be reserved"
            );
        }
    }

    #[tokio::test]
    async fn create_mailbox_already_exists() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [82u8; 32];

        let first = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Notes".into(),
            },
        )
        .await
        .expect("first create ok");
        assert!(matches!(
            first,
            fauna_protocol::bridge_routing::CreateMailboxReply::Created { .. }
        ));

        let second = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Notes".into(),
            },
        )
        .await
        .expect("second create ok");
        assert_eq!(
            second,
            fauna_protocol::bridge_routing::CreateMailboxReply::AlreadyExists
        );
    }

    #[tokio::test]
    async fn create_mailbox_invalid_name_rejected() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [83u8; 32];

        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            ("contains\0nul", "contains NUL"),
            ("trailing/", "leading/trailing separator"),
            ("/leading", "leading/trailing separator"),
            ("empty//component", "empty path component"),
        ];
        for (name, expected_reason) in cases {
            let reply = call_create_mailbox(
                state.clone(),
                mda,
                fauna_protocol::bridge_routing::CreateMailboxRequest {
                    actor_id: target.to_vec(),
                    name: (*name).into(),
                },
            )
            .await
            .expect("create handler ok");
            match reply {
                fauna_protocol::bridge_routing::CreateMailboxReply::InvalidName { reason } => {
                    assert_eq!(reason, *expected_reason, "for name {name:?}")
                }
                other => panic!("expected InvalidName for {name:?}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn create_mailbox_mta_caller_returns_permission_denied() {
        let state = fixture_state().await;
        let mta = [11u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [84u8; 32];

        let err = call_create_mailbox(
            state,
            mta,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Foo".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn create_mailbox_short_actor_id_returns_malformed() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let err = call_create_mailbox(
            state,
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: vec![1u8; 16],
                name: "Foo".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── DELETE ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_mailbox_succeeds_for_empty_user_mailbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [90u8; 32];

        // Pre-create the user mailbox.
        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "TempDir".into(),
            },
        )
        .await
        .unwrap();

        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "TempDir".into(),
            },
        )
        .await
        .expect("delete handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::Deleted
        );

        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        assert!(
            rows.iter().all(|r| r.name != "TempDir"),
            "TempDir row removed"
        );
    }

    #[tokio::test]
    async fn delete_mailbox_reserved_name_rejects() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [91u8; 32];

        for reserved in ["INBOX", "Archive", "Drafts", "Sent", "Trash", "Junk"] {
            let reply = call_delete_mailbox(
                state.clone(),
                mda,
                fauna_protocol::bridge_routing::DeleteMailboxRequest {
                    actor_id: target.to_vec(),
                    name: reserved.into(),
                },
            )
            .await
            .expect("delete handler ok");
            assert_eq!(
                reply,
                fauna_protocol::bridge_routing::DeleteMailboxReply::Reserved,
                "{reserved} must be Reserved"
            );
        }
    }

    #[tokio::test]
    async fn delete_mailbox_no_such_mailbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [92u8; 32];

        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "DoesNotExist".into(),
            },
        )
        .await
        .expect("delete handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::NoSuchMailbox
        );
    }

    #[tokio::test]
    async fn delete_mailbox_not_empty_under_forbidden_policy() {
        // The default `ImapPolicy::delete_nonempty = "forbidden"` is
        // what the handler reads, so we just have to seed a non-empty
        // user mailbox and confirm DELETE rejects it.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [93u8; 32];

        // CREATE the user mailbox.
        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Busy".into(),
            },
        )
        .await
        .unwrap();
        // Plant one placement directly via the DB helper.
        state
            .db
            .place_inbound_mail(
                &target,
                &[7u8; 32],
                "Busy",
                1_700_000_000,
                "",
                "example.com",
                true,
            )
            .await
            .unwrap();

        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "Busy".into(),
            },
        )
        .await
        .expect("delete handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::NotEmpty
        );

        // State row still exists (un-touched).
        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        assert!(
            rows.iter().any(|r| r.name == "Busy"),
            "Busy row still present after rejected delete"
        );
    }

    #[tokio::test]
    async fn delete_mailbox_not_empty_under_allowed_override_deletes() {
        // An admin `put_imap_policy { delete_nonempty: "allowed" }` must
        // actually bind: the same non-empty DELETE that's rejected under the
        // catalog default now succeeds. Proves the deployment override is
        // read (not write-only) on the delete-policy path.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                delete_nonempty: Some("allowed".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let target = [94u8; 32];

        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Busy".into(),
            },
        )
        .await
        .unwrap();
        state
            .db
            .place_inbound_mail(
                &target,
                &[7u8; 32],
                "Busy",
                1_700_000_000,
                "",
                "example.com",
                true,
            )
            .await
            .unwrap();

        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "Busy".into(),
            },
        )
        .await
        .expect("delete handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::Deleted,
            "non-empty DELETE must succeed once the override allows it"
        );
        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        assert!(
            !rows.iter().any(|r| r.name == "Busy"),
            "Busy row removed after allowed delete"
        );
    }

    #[tokio::test]
    async fn delete_mailbox_mta_caller_returns_permission_denied() {
        let state = fixture_state().await;
        let mta = [11u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [94u8; 32];

        let err = call_delete_mailbox(
            state,
            mta,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "Foo".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── RENAME ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn rename_mailbox_flat_preserves_uid_validity() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [100u8; 32];

        // CREATE the source mailbox.
        let create_reply = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "OldName".into(),
            },
        )
        .await
        .unwrap();
        let src_uid_validity = match create_reply {
            fauna_protocol::bridge_routing::CreateMailboxReply::Created { uid_validity } => {
                uid_validity
            }
            other => panic!("create failed: {other:?}"),
        };

        // RENAME.
        let reply = call_rename_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "OldName".into(),
                new_name: "NewName".into(),
            },
        )
        .await
        .expect("rename handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::Renamed
        );

        // Verify the row moved + uid_validity preserved.
        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.name != "OldName"));
        let new_row = rows
            .iter()
            .find(|r| r.name == "NewName")
            .expect("NewName row present");
        assert_eq!(
            new_row.uid_validity, src_uid_validity,
            "uid_validity must be preserved on flat rename (RFC 9051 §6.3.6)"
        );
    }

    #[tokio::test]
    async fn rename_mailbox_inbox_special_case_reseeds_empty_inbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [101u8; 32];

        // Seed INBOX + place two messages there.
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        state
            .db
            .place_inbound_mail(
                &target,
                &[1u8; 32],
                "INBOX",
                1_700_000_000,
                "",
                "example.com",
                true,
            )
            .await
            .unwrap();
        state
            .db
            .place_inbound_mail(
                &target,
                &[2u8; 32],
                "INBOX",
                1_700_000_001,
                "",
                "example.com",
                true,
            )
            .await
            .unwrap();

        let reply = call_rename_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "INBOX".into(),
                new_name: "Archived-INBOX".into(),
            },
        )
        .await
        .expect("rename handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::Renamed
        );

        // INBOX still exists (re-seeded empty, fresh uid_validity).
        let rows = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap();
        let inbox = rows.iter().find(|r| r.name == "INBOX").expect("INBOX row");
        assert!(
            inbox.uid_validity >= 2,
            "INBOX uid_validity must be freshly allocated, got {}",
            inbox.uid_validity
        );
        assert_eq!(inbox.uid_next, 1, "re-seeded INBOX uid_next = 1");
        assert_eq!(inbox.highestmodseq, 1, "re-seeded INBOX highestmodseq = 1");
        let (exists_inbox, _) = state
            .db
            .count_bridge_imap_mailbox(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(exists_inbox, 0, "re-seeded INBOX must be empty");

        // The migrated mailbox carries both messages.
        let (exists_new, _) = state
            .db
            .count_bridge_imap_mailbox(&target, "Archived-INBOX")
            .await
            .unwrap();
        assert_eq!(exists_new, 2, "migrated mailbox carries both messages");
    }

    #[tokio::test]
    async fn rename_mailbox_reserved_source_rejects_except_inbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [102u8; 32];
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();

        for reserved in ["Archive", "Drafts", "Sent", "Trash", "Junk"] {
            let reply = call_rename_mailbox(
                state.clone(),
                mda,
                fauna_protocol::bridge_routing::RenameMailboxRequest {
                    actor_id: target.to_vec(),
                    old_name: reserved.into(),
                    new_name: format!("{reserved}-renamed"),
                },
            )
            .await
            .expect("rename handler ok");
            assert_eq!(
                reply,
                fauna_protocol::bridge_routing::RenameMailboxReply::ReservedSource,
                "{reserved} must be ReservedSource"
            );
        }
    }

    #[tokio::test]
    async fn rename_mailbox_no_such_source() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [103u8; 32];

        let reply = call_rename_mailbox(
            state,
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "DoesNotExist".into(),
                new_name: "AlsoMissing".into(),
            },
        )
        .await
        .expect("rename handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::NoSuchSource
        );
    }

    #[tokio::test]
    async fn rename_mailbox_target_reserved() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [104u8; 32];

        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Source".into(),
            },
        )
        .await
        .unwrap();

        let reply = call_rename_mailbox(
            state,
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "Source".into(),
                new_name: "Archive".into(),
            },
        )
        .await
        .expect("rename handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::TargetReserved
        );
    }

    #[tokio::test]
    async fn rename_mailbox_target_exists() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [105u8; 32];

        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Source".into(),
            },
        )
        .await
        .unwrap();
        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Target".into(),
            },
        )
        .await
        .unwrap();

        let reply = call_rename_mailbox(
            state,
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "Source".into(),
                new_name: "Target".into(),
            },
        )
        .await
        .expect("rename handler ok");
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::TargetExists
        );
    }

    #[tokio::test]
    async fn rename_mailbox_invalid_name_rejected() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [106u8; 32];

        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Source".into(),
            },
        )
        .await
        .unwrap();

        let reply = call_rename_mailbox(
            state,
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "Source".into(),
                new_name: "bad\0name".into(),
            },
        )
        .await
        .expect("rename handler ok");
        match reply {
            fauna_protocol::bridge_routing::RenameMailboxReply::InvalidName { reason } => {
                assert_eq!(reason, "contains NUL");
            }
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rename_mailbox_mta_caller_returns_permission_denied() {
        let state = fixture_state().await;
        let mta = [11u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [107u8; 32];

        let err = call_rename_mailbox(
            state,
            mta,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "Foo".into(),
                new_name: "Bar".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── pure-backup destination refusal ────────────────────────────

    #[tokio::test]
    async fn list_mailboxes_refuses_on_pure_backup_destination() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));

        let mda = [0xEEu8; 32];
        approve_bridge(&db, &mda, BridgeRole::Mda).await;

        // Target whose __mail is in pure-backup mode on this nest.
        let target = [0xD1u8; 32];
        db.create_folder_with_options(
            "__mail",
            &target,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_mailboxes_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.pure_backup_destination");
    }

    #[tokio::test]
    async fn fetch_message_ciphertext_refuses_on_pure_backup_destination() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));

        let mda = [0xEFu8; 32];
        approve_bridge(&db, &mda, BridgeRole::Mda).await;

        let target = [0xD2u8; 32];
        db.create_folder_with_options(
            "__mail",
            &target,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let req = FetchMessageCiphertextRequest {
            actor_id: target.to_vec(),
            message_id: [0u8; 32].to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_message_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.pure_backup_destination");
    }

    /// Slice 2 (deployment-home-with-public-relay.md § MUA reach): an actor who
    /// turned IMAP serving OFF is rejected by the MDA, while another actor on the
    /// same nest (default-on) still serves. Proves the per-actor serving gate is
    /// caller-scoped — disabling A does not disable B.
    #[tokio::test]
    async fn list_mailboxes_rejects_actor_with_serving_disabled_only() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));

        let mda = [0xEEu8; 32];
        approve_bridge(&db, &mda, BridgeRole::Mda).await;

        // Actor A opts OUT of serving on this nest.
        let a = [0xA1u8; 32];
        db.set_actor_mail_serving_enabled(&a, false).await.unwrap();
        // Actor B never sets the flag (absent ⇒ default-on).
        let b = [0xB1u8; 32];

        // A → rejected with the distinct serving-disabled code (NOT pure-backup;
        // neither actor has a backup folder).
        let req_a = ListMailboxesRequest {
            actor_id: a.to_vec(),
            ..Default::default()
        };
        let payload_a = Bytes::from(encode_canonical(&req_a).unwrap().to_vec());
        let err = list_mailboxes_handler()(state.clone(), mda, payload_a)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.mail_serving_disabled");

        // B → passes the gate and serves (bootstraps standard mailboxes).
        let req_b = ListMailboxesRequest {
            actor_id: b.to_vec(),
            ..Default::default()
        };
        let payload_b = Bytes::from(encode_canonical(&req_b).unwrap().to_vec());
        list_mailboxes_handler()(state, mda, payload_b)
            .await
            .expect("actor B (serving default-on) is still served");

        // Re-enabling A restores serving.
        db.set_actor_mail_serving_enabled(&a, true).await.unwrap();
        let state2 = Arc::new(AppState::for_test(db.clone()));
        let req_a2 = ListMailboxesRequest {
            actor_id: a.to_vec(),
            ..Default::default()
        };
        let payload_a2 = Bytes::from(encode_canonical(&req_a2).unwrap().to_vec());
        list_mailboxes_handler()(state2, mda, payload_a2)
            .await
            .expect("actor A is served again after re-enabling");
    }

    // ── I5 Phase D.7 — SUBSCRIBE / UNSUBSCRIBE handler tests ──────────

    /// Helper: count rows in `bridge_imap_subscriptions` for an actor.
    async fn subscription_count(db: &CacheDb, actor: &[u8; 32]) -> i64 {
        let conn = db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM bridge_imap_subscriptions WHERE actor_id = ?1",
            rusqlite::params![&actor[..]],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    }

    /// Helper: subscribe `actor` to `mailbox` by going through the
    /// handler entry point (so the test exercises the full request
    /// path, not just the DB helper).
    async fn subscribe_via_handler(
        state: Arc<AppState>,
        caller: [u8; 32],
        target: &[u8; 32],
        mailbox: &str,
    ) {
        let req = SubscribeMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: mailbox.into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = subscribe_mailbox_handler()(state, caller, payload)
            .await
            .expect("handler ok");
        let reply: SubscribeMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, SubscribeMailboxReply::Subscribed);
    }

    /// Helper: unsubscribe `actor` from `mailbox` by going through the
    /// handler entry point. Mirror of `subscribe_via_handler`.
    async fn unsubscribe_via_handler(
        state: Arc<AppState>,
        caller: [u8; 32],
        target: &[u8; 32],
        mailbox: &str,
    ) {
        let req = UnsubscribeMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: mailbox.into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = unsubscribe_mailbox_handler()(state, caller, payload)
            .await
            .expect("handler ok");
        let reply: UnsubscribeMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, UnsubscribeMailboxReply::Unsubscribed);
    }

    #[tokio::test]
    async fn subscribe_mailbox_persists_and_is_idempotent() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x71u8; 32];

        subscribe_via_handler(state.clone(), mda, &target, "Saved Searches").await;
        assert_eq!(subscription_count(&state.db, &target).await, 1);

        // Second call must succeed (RFC 9051 §6.3.7) and not duplicate.
        subscribe_via_handler(state.clone(), mda, &target, "Saved Searches").await;
        assert_eq!(
            subscription_count(&state.db, &target).await,
            1,
            "INSERT OR IGNORE collapses PK conflict"
        );
    }

    #[tokio::test]
    async fn subscribe_mailbox_unknown_mailbox_is_allowed() {
        // RFC 9051 §6.3.7: SUBSCRIBE may target a mailbox that does
        // not (yet) exist. MUAs frequently do this at first connect.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x72u8; 32];

        // No ensure_bridge_imap_mailboxes call — the target mailbox
        // genuinely does not exist in `bridge_imap_mailbox_state`.
        subscribe_via_handler(state.clone(), mda, &target, "Outbox-not-yet-created").await;
        assert_eq!(subscription_count(&state.db, &target).await, 1);
    }

    #[tokio::test]
    async fn subscribe_mailbox_requires_mda_class() {
        let state = fixture_state().await;
        let mta = [2u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [0x73u8; 32];

        let req = SubscribeMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: "INBOX".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = subscribe_mailbox_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn unsubscribe_mailbox_is_idempotent() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x74u8; 32];

        // Pre-seed a subscription.
        subscribe_via_handler(state.clone(), mda, &target, "Pinned").await;
        assert_eq!(subscription_count(&state.db, &target).await, 1);

        // First UNSUBSCRIBE removes it.
        let req = UnsubscribeMailboxRequest {
            actor_id: target.to_vec(),
            mailbox: "Pinned".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = unsubscribe_mailbox_handler()(state.clone(), mda, payload.clone())
            .await
            .expect("handler ok");
        let reply: UnsubscribeMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, UnsubscribeMailboxReply::Unsubscribed);
        assert_eq!(subscription_count(&state.db, &target).await, 0);

        // Second UNSUBSCRIBE on the now-absent row also succeeds
        // (RFC 9051 §6.3.8 — DELETE on zero rows is not an error).
        let bytes = unsubscribe_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok (idempotent)");
        let reply: UnsubscribeMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, UnsubscribeMailboxReply::Unsubscribed);
        assert_eq!(subscription_count(&state.db, &target).await, 0);
    }

    // ── T9.5 placement-journal wiring tests for SUBSCRIBE / UNSUBSCRIBE ──
    //
    // Mirrors the T9 create/delete/rename placement-journal tests above:
    // each handler appends a `MailPlacementRecord::{Subscribe,Unsubscribe}`
    // to `state.mail_placement` after its SQLite mutation returns Ok(()).
    // Unlike T8 (Move/Copy) and T9 (Create/Delete/Rename), Subscribe and
    // Unsubscribe emit unconditionally — the manifest-side
    // `apply_record_to_manifest` is fully idempotent for both variants
    // (Subscribe: `if !contains { push }`; Unsubscribe: `retain(!= s)`),
    // so duplicate emission cannot corrupt the manifest. Spec § D2
    // (record shapes), § D6 (ε) (atomic-with-SQL — see wiring-site
    // comments for the deferred crash window). Actor IDs `[0xB0..0xB2]`
    // to stay clear of every previously claimed range.

    #[tokio::test]
    async fn subscribe_mailbox_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xB0u8; 32];

        subscribe_via_handler(state.clone(), mda, &target, "Saved Searches").await;

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.subscriptions,
            vec!["Saved Searches".to_string()],
            "Subscribe record must land in the manifest's subscriptions Vec",
        );
    }

    #[tokio::test]
    async fn unsubscribe_mailbox_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xB1u8; 32];

        // Subscribe first (one Subscribe record).
        subscribe_via_handler(state.clone(), mda, &target, "Pinned").await;
        // Verify the Subscribe wiring runs first — without it, the
        // manifest would already be empty here and the post-UNSUB
        // assertion would pass vacuously, hiding a missing UNSUB wiring.
        let mid = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            mid.subscriptions,
            vec!["Pinned".to_string()],
            "SUB wiring must seed the manifest before we test UNSUB drop",
        );

        // Then UNSUBSCRIBE — emits one Unsubscribe record that the
        // manifest-side apply uses to drop "Pinned" from `subscriptions`.
        unsubscribe_via_handler(state.clone(), mda, &target, "Pinned").await;

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert!(
            manifest.subscriptions.is_empty(),
            "Unsubscribe record must drop the mailbox from the manifest; \
             got subscriptions={:?}",
            manifest.subscriptions,
        );
    }

    #[tokio::test]
    async fn subscribe_then_unsubscribe_then_subscribe_round_trips_in_manifest() {
        // Proves the round-trip semantics through the journal: the
        // second Subscribe re-adds the mailbox after Unsubscribe
        // removed it. This is the load-bearing assumption behind T9.5's
        // "emit unconditionally on Ok(())" design decision — duplicate
        // Subscribe records are safe because `apply_record_to_manifest`
        // collapses them via `if !contains { push }`.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xB2u8; 32];

        // SUB → manifest has "Drafts".
        subscribe_via_handler(state.clone(), mda, &target, "Drafts").await;

        // UNSUB → manifest empty.
        unsubscribe_via_handler(state.clone(), mda, &target, "Drafts").await;

        // SUB again → manifest has "Drafts" once more.
        subscribe_via_handler(state.clone(), mda, &target, "Drafts").await;

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.subscriptions,
            vec!["Drafts".to_string()],
            "after SUB → UNSUB → SUB the mailbox must be present exactly once",
        );
    }

    #[tokio::test]
    async fn list_mailboxes_subscribed_only_filters_correctly() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x75u8; 32];

        // Seed standard mailboxes (six) then subscribe to two of them.
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        subscribe_via_handler(state.clone(), mda, &target, "INBOX").await;
        subscribe_via_handler(state.clone(), mda, &target, "Sent").await;

        // subscribed_only=true → exactly the two subscribed mailboxes.
        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            subscribed_only: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_mailboxes_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: ListMailboxesReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let names: Vec<&str> = reply.mailboxes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["INBOX", "Sent"], "alphabetical, filtered");
    }

    #[tokio::test]
    async fn list_mailboxes_subscribed_only_default_returns_all_mailboxes() {
        // An omitted `subscribed_only` decodes as `false` (per serde
        // default, the plain LIST shape) and yields the full Phase-C
        // mailbox set — no subscription filter applied.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x76u8; 32];

        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();
        subscribe_via_handler(state.clone(), mda, &target, "INBOX").await;
        // Note: no subscribed_only field on this request — Default::default()
        // leaves it `false`.
        let req = ListMailboxesRequest {
            actor_id: target.to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_mailboxes_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: ListMailboxesReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(
            reply.mailboxes.len(),
            6,
            "all six standard mailboxes even though only INBOX is subscribed"
        );
    }
    // ── T7 placement-journal wiring tests ───────────────────────────────────
    //
    // These verify that the three IMAP write-RPC handlers emit the
    // expected `MailPlacementRecord` to `state.mail_placement` after
    // their SQLite mutation completes. Spec § D2 (record shapes),
    // § D6 (ε) (atomic-with-SQL invariant — see code comments at the
    // wiring sites for the deferred crash window).

    #[tokio::test]
    async fn append_rpc_produces_append_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // Unique actor per test — MailPlacementSegmentManager keeps a
        // per-actor manifest, and the test fixture uses a process-unique
        // tempdir, so as long as each test picks a fresh actor we don't
        // bleed state between tests.
        let target = [0xF0u8; 32];

        let reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Drafts", vec!["\\Draft".into()]),
        )
        .await
        .expect("append handler ok");

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.placements.len(),
            1,
            "exactly one placement after one APPEND"
        );
        let p = &manifest.placements[0];
        assert_eq!(p.mailbox, "Drafts");
        assert_eq!(p.uid, reply.uid);
        assert_eq!(p.flags, vec!["\\Draft".to_string()]);
        assert_eq!(p.content_record_id, reply.message_id);
        assert_eq!(p.internal_date, 1_700_000_000);
        assert!(p.modseq >= 1, "modseq is at least 1");
        // Note: `manifest.mailboxes` is populated only by `Create`
        // placement events. Bootstrap of the six standard IMAP
        // mailboxes (`ensure_bridge_imap_mailboxes`) is outside the
        // three handlers wired in T7, so this test does not assert
        // mailbox-state-row presence in the manifest. Plan 1's
        // create_mailbox / rename_mailbox / delete_mailbox handler
        // wiring lands in a separate track.
    }

    #[tokio::test]
    async fn store_flags_rpc_produces_store_flags_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xF1u8; 32];

        // Seed via the APPEND handler so the placement journal is
        // populated end-to-end (not via `seed_message_for_handler`,
        // which bypasses the journal).
        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let uid = append_reply.uid;

        // Set \Seen on the appended message.
        let _ = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![uid],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store_flags ok");

        // The manifest's compacted state should reflect the new flags
        // on the same uid (apply_record_to_manifest handles StoreFlags
        // by updating placements with matching uid_set).
        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        let p = manifest
            .placements
            .iter()
            .find(|p| p.mailbox == "INBOX" && p.uid == uid)
            .expect("INBOX placement still present after STORE");
        assert_eq!(
            p.flags,
            vec!["\\Seen".to_string()],
            "after_flags applied to manifest placement"
        );
        // The STORE record's modseq must be strictly greater than the
        // APPEND's modseq — `apply_store_flags` bumps `highestmodseq`
        // by 1 in-transaction. The first APPEND in a fresh mailbox
        // gets modseq=1 (allocate_uid bumps from 0 → 1), so STORE
        // produces modseq >= 2.
        assert!(
            p.modseq >= 2,
            "STORE bumped modseq past APPEND's seed; got {}",
            p.modseq,
        );
        // STORE must never emit a tombstone — only EXPUNGE / Move do.
        assert!(
            manifest.tombstones.is_empty(),
            "STORE must not produce tombstones; got {:?}",
            manifest.tombstones,
        );
    }

    #[tokio::test]
    async fn expunge_rpc_produces_expunge_tombstone() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xF2u8; 32];

        // APPEND, then mark \Deleted, then EXPUNGE.
        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let uid = append_reply.uid;

        let _ = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![uid],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Deleted".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store_flags ok");

        let reply = call_expunge(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await
        .expect("expunge ok");
        assert_eq!(reply.expunged_uids, vec![uid]);

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert!(
            !manifest.placements.iter().any(|p| p.uid == uid),
            "expunged uid removed from placements"
        );
        // One tombstone for the expunged uid, modseq = post-expunge highestmodseq.
        let inbox_tombs: Vec<_> = manifest
            .tombstones
            .iter()
            .filter(|t| t.mailbox == "INBOX" && t.uid == uid)
            .collect();
        assert_eq!(inbox_tombs.len(), 1, "exactly one tombstone for uid");
        assert_eq!(inbox_tombs[0].modseq, reply.highestmodseq as u64);
        assert!(
            inbox_tombs[0].deleted_at > 0,
            "EXPUNGE stamps deleted_at eagerly (imap-server.md § Tombstone retention)"
        );
    }

    /// imap-server.md § Tombstone retention: EXPUNGE piggybacks a prune of
    /// tombstones past the effective retention window. A tombstone already
    /// past the window is dropped by the next EXPUNGE call; one still
    /// inside it survives.
    #[tokio::test]
    async fn expunge_rpc_prunes_tombstones_past_the_retention_window() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xF3u8; 32];

        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let uid = append_reply.uid;
        let _ = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![uid],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Deleted".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store_flags ok");
        call_expunge(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await
        .expect("expunge ok");

        // Simulate the tombstone aging past the (default 30-day) retention
        // window — real time can't move, so age the stamp directly.
        state
            .mail_placement
            .update_manifest(&target, |m| {
                for t in &mut m.tombstones {
                    t.deleted_at = 1;
                }
                true
            })
            .await
            .expect("age tombstone");

        // A second APPEND+EXPUNGE, on a different uid, piggybacks the prune.
        let append_reply2 = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let uid2 = append_reply2.uid;
        let _ = call_store_flags(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![uid2],
                op: fauna_protocol::bridge_routing::StoreFlagsOp::Set,
                flags: vec!["\\Deleted".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store_flags ok");
        call_expunge(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await
        .expect("second expunge ok");

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert!(
            !manifest.tombstones.iter().any(|t| t.uid == uid),
            "the aged tombstone was pruned"
        );
        assert!(
            manifest.tombstones.iter().any(|t| t.uid == uid2),
            "the fresh tombstone survives"
        );
    }

    // ── T8 placement-journal wiring tests (move / copy) ─────────────────────
    //
    // These verify that the two cross-mailbox IMAP write-RPC handlers
    // emit the expected `MailPlacementRecord::{Move, Copy}` to
    // `state.mail_placement` after their SQLite mutation completes.
    // Same atomic-with-SQL caveat as T7 — see § D6 (ε) note at each
    // wiring site for the deferred-to-Plan-2-T9 crash window.
    //
    // Actor IDs use the `[0xE0..0xE2]` range to stay clear of T7's
    // `[0xF0..0xF2]` and the MTA tests' `[0xC0..0xC2]` — the
    // MailPlacementSegmentManager keeps a per-actor manifest in a
    // process-unique tempdir, so distinct actors guarantee no state
    // bleed across tests.

    #[tokio::test]
    async fn move_rpc_produces_move_record_and_src_tombstone() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xE0u8; 32];

        // Seed via the APPEND handler so the placement journal is
        // populated end-to-end (mirrors the T7 pattern).
        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let src_uid = append_reply.uid;

        // MOVE INBOX:src_uid → Archive.
        let move_reply = call_move_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![src_uid],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect("move ok");
        assert_eq!(move_reply.moved.len(), 1);
        let dst_uid = move_reply.moved[0].dest_uid;

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // Destination has exactly one placement at the new (mailbox, uid)
        // with the bumped dest modseq.
        let archive: Vec<_> = manifest
            .placements
            .iter()
            .filter(|p| p.mailbox == "Archive")
            .collect();
        assert_eq!(archive.len(), 1, "exactly one Archive placement");
        assert_eq!(archive[0].uid, dst_uid);
        assert_eq!(archive[0].modseq, move_reply.dest_highestmodseq as u64);
        assert_eq!(archive[0].content_record_id, append_reply.message_id);

        // Source has zero placements (the moved row is gone).
        let inbox_placements: Vec<_> = manifest
            .placements
            .iter()
            .filter(|p| p.mailbox == "INBOX")
            .collect();
        assert!(
            inbox_placements.is_empty(),
            "INBOX placement removed by MOVE; got {}",
            inbox_placements.len(),
        );

        // Source tombstone for the moved UID at the bumped src modseq.
        let inbox_tombs: Vec<_> = manifest
            .tombstones
            .iter()
            .filter(|t| t.mailbox == "INBOX" && t.uid == src_uid)
            .collect();
        assert_eq!(inbox_tombs.len(), 1, "exactly one INBOX tombstone");
        assert_eq!(
            inbox_tombs[0].modseq,
            move_reply.source_highestmodseq as u64,
        );
    }

    #[tokio::test]
    async fn copy_rpc_produces_copy_record_no_tombstone() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xE1u8; 32];

        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");
        let src_uid = append_reply.uid;
        // Capture the APPEND's modseq before COPY for the src-unchanged
        // assertion below.
        let src_modseq_before_copy = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("manifest")
            .placements
            .iter()
            .find(|p| p.mailbox == "INBOX" && p.uid == src_uid)
            .expect("INBOX placement after APPEND")
            .modseq;

        let copy_reply = call_copy_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CopyMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![src_uid],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect("copy ok");
        assert_eq!(copy_reply.copied.len(), 1);
        let dst_uid = copy_reply.copied[0].dest_uid;

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // Two placements: one in INBOX (src, unchanged) + one in Archive
        // (dst, fresh) — sharing content_record_id.
        let inbox_p = manifest
            .placements
            .iter()
            .find(|p| p.mailbox == "INBOX" && p.uid == src_uid)
            .expect("INBOX placement still present after COPY");
        let archive_p = manifest
            .placements
            .iter()
            .find(|p| p.mailbox == "Archive" && p.uid == dst_uid)
            .expect("Archive placement created by COPY");
        assert_eq!(
            inbox_p.content_record_id, archive_p.content_record_id,
            "src+dst share the same content_record_id",
        );
        assert_eq!(
            inbox_p.content_record_id, append_reply.message_id,
            "src content_record_id matches the APPEND",
        );

        // Dst placement carries the COPY's bumped dst modseq.
        assert_eq!(archive_p.modseq, copy_reply.dest_highestmodseq as u64);

        // Src placement's modseq is unchanged from the APPEND (COPY does
        // not bump src per IMAP RFC 3501 / spec § D2).
        assert_eq!(
            inbox_p.modseq, src_modseq_before_copy,
            "COPY left src modseq untouched",
        );

        // No tombstones at all — Copy never emits one.
        assert!(
            manifest.tombstones.is_empty(),
            "COPY emits no tombstones; got {}",
            manifest.tombstones.len(),
        );
    }

    #[tokio::test]
    async fn move_rpc_empty_match_produces_no_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xE2u8; 32];

        // Ensure mailboxes exist (so the handler doesn't fail before
        // reaching apply_move) but seed nothing in INBOX.
        state
            .db
            .ensure_bridge_imap_mailboxes(&target)
            .await
            .unwrap();

        // Snapshot the manifest before — should have zero placements +
        // zero tombstones (mailbox bootstrap doesn't emit placement
        // records in T7/T8 scope).
        let before = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("manifest");
        assert!(before.placements.is_empty());
        assert!(before.tombstones.is_empty());

        // MOVE a UID that doesn't exist; apply_move silently skips →
        // moved = vec![], handler must suppress the placement record.
        let move_reply = call_move_messages(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![42],
                dest_mailbox: "Archive".into(),
            },
        )
        .await
        .expect("move ok");
        assert!(move_reply.moved.is_empty(), "no UIDs matched");

        // Manifest unchanged: no Move record was appended (otherwise
        // apply_record_to_manifest would push a tombstone for uid=42
        // unconditionally, per the QRESYNC-drift comment in
        // segments/mail_placement.rs).
        let after = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("manifest");
        assert!(
            after.placements.is_empty(),
            "no placements after no-op MOVE",
        );
        assert!(
            after.tombstones.is_empty(),
            "no tombstones after no-op MOVE — the empty-set skip in the \
             handler prevented an empty Move record from being appended",
        );
    }

    // ── T9 placement-journal wiring tests ───────────────────────────────────
    //
    // These verify that the three IMAP mailbox-CRUD handlers
    // (`create_mailbox`, `delete_mailbox`, `rename_mailbox`) emit the
    // expected `MailPlacementRecord` to `state.mail_placement` after
    // their SQLite mutation completes, AND that
    // `ensure_bridge_imap_mailboxes` (called by every IMAP-serving
    // handler to bootstrap the six standard mailboxes for a fresh
    // actor) emits one `Create` per newly-seeded mailbox before any
    // other placement event in the same handler — the T7-reviewer
    // seeder-gap closure. Spec § D2 (record shapes); § D6 (ε)
    // atomic-with-SQL note — same crash-window deferral to Plan 2
    // T9 as T7 / T8. Actor IDs `[0xD0..0xD4]` to stay clear of T7's
    // `[0xF0..0xF2]`, T7-MTA's `[0xC0..0xC2]`, and T8's
    // `[0xE0..0xE2]`.

    #[tokio::test]
    async fn create_mailbox_rpc_produces_create_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD0u8; 32];

        let reply = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Projects".into(),
            },
        )
        .await
        .expect("create handler ok");
        let created_uid_validity = match reply {
            fauna_protocol::bridge_routing::CreateMailboxReply::Created { uid_validity } => {
                uid_validity
            }
            other => panic!("expected Created, got {other:?}"),
        };

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        // Manifest holds exactly one mailbox-state row — the
        // user-created "Projects" — because no other handler ran
        // (create_mailbox does NOT call ensure_bridge_imap_mailboxes,
        // so the six standard mailboxes are NOT bootstrapped here).
        assert_eq!(
            manifest.mailboxes.len(),
            1,
            "exactly one mailbox in the manifest after one CREATE; got {:?}",
            manifest
                .mailboxes
                .iter()
                .map(|m| &m.name)
                .collect::<Vec<_>>(),
        );
        let m = &manifest.mailboxes[0];
        assert_eq!(m.name, "Projects");
        assert_eq!(m.uid_validity, created_uid_validity);
        assert_eq!(m.uid_next, 1);
        assert_eq!(m.highestmodseq, 1);
        // User-created mailboxes carry no SPECIAL-USE attrs per
        // imap-server.md § Standard mailboxes (RFC 6154 attributes
        // are reserved for the six bootstrap mailboxes).
        assert!(
            m.attrs.is_empty(),
            "user-created mailbox has no SPECIAL-USE attrs; got {:?}",
            m.attrs,
        );
        // No placements, no tombstones — Create touches only the
        // mailboxes Vec.
        assert!(manifest.placements.is_empty());
        assert!(manifest.tombstones.is_empty());
    }

    #[tokio::test]
    async fn create_mailbox_rpc_already_exists_emits_no_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD1u8; 32];

        // First CREATE — emits one Create record.
        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Snippets".into(),
            },
        )
        .await
        .expect("first create ok");

        let manifest_after_first = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(manifest_after_first.mailboxes.len(), 1);

        // Repeat CREATE — DB returns AlreadyExists; handler must
        // skip the placement append. Manifest unchanged.
        let reply = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Snippets".into(),
            },
        )
        .await
        .expect("second create ok");
        assert!(matches!(
            reply,
            fauna_protocol::bridge_routing::CreateMailboxReply::AlreadyExists,
        ));

        let manifest_after_second = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        // Exactly the same shape — no duplicate Create record was
        // appended on the idempotent-repeat call.
        assert_eq!(
            manifest_after_second.mailboxes, manifest_after_first.mailboxes,
            "idempotent CREATE must not emit a duplicate Create record",
        );
    }

    #[tokio::test]
    async fn delete_mailbox_rpc_produces_delete_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD2u8; 32];

        // CREATE → manifest now has the user mailbox.
        call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Tmp".into(),
            },
        )
        .await
        .expect("create ok");
        let mid = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(mid.mailboxes.len(), 1);
        assert!(mid.mailboxes.iter().any(|m| m.name == "Tmp"));

        // DELETE → manifest-side Delete cascade drops the mailbox
        // state row.
        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "Tmp".into(),
            },
        )
        .await
        .expect("delete ok");
        assert!(matches!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::Deleted,
        ));

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert!(
            !manifest.mailboxes.iter().any(|m| m.name == "Tmp"),
            "Tmp mailbox should be dropped from manifest after DELETE",
        );
        // Delete cascade clears placements + tombstones for the
        // deleted mailbox; this test seeded none, so both stay empty.
        assert!(manifest.placements.iter().all(|p| p.mailbox != "Tmp"));
        assert!(manifest.tombstones.iter().all(|t| t.mailbox != "Tmp"));
    }

    #[tokio::test]
    async fn delete_mailbox_rpc_no_such_mailbox_emits_no_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD3u8; 32];

        // DELETE on a never-created user mailbox — DB returns
        // NoSuchMailbox; handler must skip the placement append.
        let reply = call_delete_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::DeleteMailboxRequest {
                actor_id: target.to_vec(),
                name: "NeverExisted".into(),
            },
        )
        .await
        .expect("delete ok");
        assert!(matches!(
            reply,
            fauna_protocol::bridge_routing::DeleteMailboxReply::NoSuchMailbox,
        ));

        // Manifest is fully empty — no handler called
        // ensure_bridge_imap_mailboxes for this actor.
        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert!(
            manifest.mailboxes.is_empty(),
            "no Create / no Delete record was emitted for an absent target",
        );
        assert!(manifest.placements.is_empty());
        assert!(manifest.tombstones.is_empty());
    }

    #[tokio::test]
    async fn rename_mailbox_rpc_produces_rename_record() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD4u8; 32];

        // CREATE the source mailbox.
        let create_reply = call_create_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Old".into(),
            },
        )
        .await
        .expect("create ok");
        let created_uid_validity = match create_reply {
            fauna_protocol::bridge_routing::CreateMailboxReply::Created { uid_validity } => {
                uid_validity
            }
            other => panic!("expected Created, got {other:?}"),
        };

        // APPEND a message into Old so the Rename relabel can be
        // observed on a non-empty placement row.
        let append_reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "Old", vec![]),
        )
        .await
        .expect("append ok");
        let append_uid = append_reply.uid;

        // RENAME Old → New.
        let reply = call_rename_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "Old".into(),
                new_name: "New".into(),
            },
        )
        .await
        .expect("rename ok");
        assert!(matches!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::Renamed,
        ));

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // Old is gone, New is present.
        assert!(
            !manifest.mailboxes.iter().any(|m| m.name == "Old"),
            "Old must be absent after RENAME",
        );
        let new_row = manifest
            .mailboxes
            .iter()
            .find(|m| m.name == "New")
            .expect("New mailbox row present");
        // Non-INBOX rename preserves uid_validity (per RFC 9051 §6.3.6
        // — no MUA re-sync) AND the seeded `\Inbox`-attr cousin
        // pattern doesn't apply, so attrs remain empty.
        assert_eq!(new_row.uid_validity, created_uid_validity);
        assert!(new_row.attrs.is_empty());

        // The single placement was relabeled (per
        // apply_record_to_manifest for Rename) from "Old" → "New",
        // preserving the original UID + modseq + content_record_id.
        let placement = manifest
            .placements
            .iter()
            .find(|p| p.uid == append_uid)
            .expect("placement for the appended uid");
        assert_eq!(placement.mailbox, "New");
        assert_eq!(placement.content_record_id, append_reply.message_id);

        // Rename emits no tombstones.
        assert!(manifest.tombstones.is_empty());
    }

    #[tokio::test]
    async fn rename_mailbox_rpc_inbox_special_case_reseeds_inbox() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD5u8; 32];

        // Trigger ensure_bridge_imap_mailboxes via an APPEND so the
        // manifest's INBOX row is seeded (mirrors what would happen
        // before a real INBOX rename would land at a fresh actor).
        call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");

        // Sanity: INBOX is in the manifest.
        let pre = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        let pre_inbox_uidv = pre
            .mailboxes
            .iter()
            .find(|m| m.name == "INBOX")
            .expect("INBOX row present after bootstrap")
            .uid_validity;
        assert_eq!(pre_inbox_uidv, 1, "INBOX seeded with uid_validity=1");

        // RENAME INBOX → MyInbox. The DB layer:
        //   1. Migrates every INBOX placement into MyInbox with
        //      renumbered UIDs starting at 1.
        //   2. Re-seeds an empty INBOX row with a fresh
        //      `inbox_uid_validity` (handler-supplied).
        //
        // The handler emits two placement events: a `Rename` (which
        // relabels INBOX→MyInbox in the manifest) followed by a
        // `Create` for the re-seeded empty INBOX. Known divergence
        // — the renamed mailbox's uid_validity in the manifest
        // remains the seeded value (1), not the DB's freshly
        // assigned `new_uid_validity`. Plan 2 T9's divergence
        // detection at SELECT / QRESYNC time is the design's repair
        // mechanism (spec § D6 (γ)).
        let reply = call_rename_mailbox(
            state.clone(),
            mda,
            fauna_protocol::bridge_routing::RenameMailboxRequest {
                actor_id: target.to_vec(),
                old_name: "INBOX".into(),
                new_name: "MyInbox".into(),
            },
        )
        .await
        .expect("rename ok");
        assert!(matches!(
            reply,
            fauna_protocol::bridge_routing::RenameMailboxReply::Renamed,
        ));

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // MyInbox row exists (relabeled from the seeded INBOX).
        assert!(
            manifest.mailboxes.iter().any(|m| m.name == "MyInbox"),
            "MyInbox row present after INBOX rename",
        );

        // INBOX is re-seeded with a fresh uid_validity ≠ 1, AND
        // carries the SPECIAL-USE \Inbox attr per the imap-server.md
        // § Standard mailboxes contract.
        let new_inbox = manifest
            .mailboxes
            .iter()
            .find(|m| m.name == "INBOX")
            .expect("INBOX re-seeded after rename");
        assert!(
            new_inbox.uid_validity > 1,
            "INBOX uid_validity must be freshly allocated after rename; got {}",
            new_inbox.uid_validity,
        );
        assert_eq!(
            new_inbox.attrs,
            vec!["\\Inbox".to_string()],
            "re-seeded INBOX retains the RFC 6154 SPECIAL-USE attr",
        );
    }

    // ── Seeder-gap closure tests (T9 part B) ────────────────────────

    #[tokio::test]
    async fn append_to_fresh_actor_emits_six_standard_create_records() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // Never-seen actor — first APPEND triggers
        // ensure_bridge_imap_mailboxes which now emits a Create per
        // newly-seeded mailbox before the Append record applies.
        let target = [0xD6u8; 32];

        let reply = call_append_message(
            state.clone(),
            mda,
            sample_append_req(&target, "INBOX", vec![]),
        )
        .await
        .expect("append ok");

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // Six standard mailboxes present in the manifest, each
        // carrying its RFC 6154 SPECIAL-USE attr.
        let expected: &[(&str, &str)] = &[
            ("INBOX", "\\Inbox"),
            ("Archive", "\\Archive"),
            ("Drafts", "\\Drafts"),
            ("Sent", "\\Sent"),
            ("Trash", "\\Trash"),
            ("Junk", "\\Junk"),
        ];
        for (name, attr) in expected {
            let m = manifest
                .mailboxes
                .iter()
                .find(|m| m.name == *name)
                .unwrap_or_else(|| {
                    panic!(
                        "standard mailbox {name} missing from manifest; \
                         got {:?}",
                        manifest
                            .mailboxes
                            .iter()
                            .map(|m| &m.name)
                            .collect::<Vec<_>>(),
                    )
                });
            assert_eq!(m.uid_validity, 1, "seeded uid_validity for {name}");
            assert_eq!(
                m.attrs,
                vec![(*attr).to_string()],
                "SPECIAL-USE attr for {name}",
            );
        }

        // The Append record also landed — exactly one placement.
        // (Apply-order is Creates first, Append second; the Append
        // record's manifest apply finds the INBOX state row and
        // bumps its uid_next + highestmodseq.)
        assert_eq!(
            manifest.placements.len(),
            1,
            "exactly one placement from the APPEND",
        );
        let p = &manifest.placements[0];
        assert_eq!(p.mailbox, "INBOX");
        assert_eq!(p.uid, reply.uid);
        assert_eq!(p.content_record_id, reply.message_id);

        // The INBOX state row reflects the Append's uid_next bump.
        let inbox = manifest
            .mailboxes
            .iter()
            .find(|m| m.name == "INBOX")
            .unwrap();
        assert!(
            inbox.uid_next > 1,
            "INBOX.uid_next bumped by the APPEND; got {}",
            inbox.uid_next,
        );
    }

    #[tokio::test]
    async fn ensure_bridge_imap_mailboxes_second_call_emits_no_create_records() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0xD7u8; 32];

        // First handler call — triggers the bootstrap, emits six
        // Create records.
        let _first = list_mailboxes_handler()(
            state.clone(),
            mda,
            Bytes::from(
                encode_canonical(&fauna_protocol::bridge_routing::ListMailboxesRequest {
                    actor_id: target.to_vec(),
                    subscribed_only: false,
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("first list ok");

        let after_first = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            after_first.mailboxes.len(),
            6,
            "six standard mailboxes after first handler call",
        );

        // Second handler call — DB's `INSERT OR IGNORE` matches every
        // row, returns empty Vec. Manifest must be byte-equal.
        let _second = list_mailboxes_handler()(
            state.clone(),
            mda,
            Bytes::from(
                encode_canonical(&fauna_protocol::bridge_routing::ListMailboxesRequest {
                    actor_id: target.to_vec(),
                    subscribed_only: false,
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("second list ok");

        let after_second = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            after_second.mailboxes, after_first.mailboxes,
            "second ensure must not duplicate or alter the manifest rows",
        );
    }

    // ── T12 end-to-end placement journal round-trip ──
    //
    // Whole-system test: drive a representative MUA workflow through
    // the WS-RPC handler entry points (create / subscribe / append /
    // store_flags / move / expunge) and assert the resulting compacted
    // placement manifest — both in-memory via `current_manifest` and on
    // disk via `MailPlacementManifest::load(path)` — matches the expected
    // final state from the CalDAV/IMAP restore design (tracked internally).
    //
    // Actor `[0x50; 32]` to stay clear of T7 (`[0xF0..]`), T7-MTA
    // (`[0xC0..]`), T8 (`[0xE0..]`), T9 (`[0xD0..]`), and T9.5
    // (`[0xB0..]`). Single MDA bridge `[9u8; 32]` reused per the
    // module's existing fixture pattern.

    #[tokio::test]
    async fn placement_journal_round_trip_mail_full_workflow() {
        use fauna_mail::segments::placement::{
            MailPlacementManifest, mail_placement_manifest_path,
        };
        use fauna_protocol::bridge_routing::{
            AppendMessageRequest, CreateMailboxReply, CreateMailboxRequest, ExpungeRequest,
            MoveMessagesRequest, StoreFlagsOp, StoreFlagsRequest,
        };

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [0x50u8; 32];

        // 1. CREATE Projects.
        let create_reply = call_create_mailbox(
            state.clone(),
            mda,
            CreateMailboxRequest {
                actor_id: target.to_vec(),
                name: "Projects".into(),
            },
        )
        .await
        .expect("create Projects ok");
        assert!(
            matches!(create_reply, CreateMailboxReply::Created { .. }),
            "Projects must be created fresh; got {create_reply:?}",
        );

        // 2. SUBSCRIBE INBOX.
        subscribe_via_handler(state.clone(), mda, &target, "INBOX").await;

        // 3. SUBSCRIBE Projects.
        subscribe_via_handler(state.clone(), mda, &target, "Projects").await;

        // 4. APPEND m1 to INBOX, no flags → uid 1.
        let m1_body = sealed(b"From: a@b\r\n\r\nm1\r\n");
        let append_m1 = call_append_message(
            state.clone(),
            mda,
            AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                flags: vec![],
                encrypted_body: m1_body.clone(),
                encrypted_index_hint: sealed(b"hint-m1"),
                timestamp: 1_700_000_001,
                ciphertext_size: m1_body.len() as u32,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .expect("append m1 ok");
        assert_eq!(append_m1.uid, 1, "first INBOX message must be uid 1");

        // 5. APPEND m2 to INBOX, no flags → uid 2.
        let m2_body = sealed(b"From: a@b\r\n\r\nm2\r\n");
        let append_m2 = call_append_message(
            state.clone(),
            mda,
            AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                flags: vec![],
                encrypted_body: m2_body.clone(),
                encrypted_index_hint: sealed(b"hint-m2"),
                timestamp: 1_700_000_002,
                ciphertext_size: m2_body.len() as u32,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .expect("append m2 ok");
        assert_eq!(append_m2.uid, 2, "second INBOX message must be uid 2");

        // 6. STORE +FLAGS \Seen on INBOX uid 1.
        let _ = call_store_flags(
            state.clone(),
            mda,
            StoreFlagsRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![1],
                op: StoreFlagsOp::Add,
                flags: vec!["\\Seen".into()],
                ..Default::default()
            },
        )
        .await
        .expect("store +FLAGS \\Seen on uid 1 ok");

        // 7. MOVE INBOX uid 2 → Projects.
        let move_reply = call_move_messages(
            state.clone(),
            mda,
            MoveMessagesRequest {
                actor_id: target.to_vec(),
                source_mailbox: "INBOX".into(),
                uids: vec![2],
                dest_mailbox: "Projects".into(),
            },
        )
        .await
        .expect("move INBOX:2 → Projects ok");
        assert_eq!(move_reply.moved.len(), 1, "exactly one row moved");
        assert_eq!(move_reply.moved[0].source_uid, 2);
        // Projects is fresh — the moved row lands as Projects uid 1.
        assert_eq!(
            move_reply.moved[0].dest_uid, 1,
            "moved row must be Projects uid 1 (fresh mailbox)",
        );

        // 8. APPEND m3 to INBOX with \Deleted → uid 3 (uid 2 was moved
        //    out, so the next allocation is 3 — uids are not reused).
        let m3_body = sealed(b"From: c@d\r\n\r\nm3\r\n");
        let append_m3 = call_append_message(
            state.clone(),
            mda,
            AppendMessageRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                flags: vec!["\\Deleted".into()],
                encrypted_body: m3_body.clone(),
                encrypted_index_hint: sealed(b"hint-m3"),
                timestamp: 1_700_000_003,
                ciphertext_size: m3_body.len() as u32,
                sender_domain: String::new(),
                dedup_key: "env:v1:fixture".into(),
                envelope_key: "env:v1:fixture".into(),
                ..Default::default()
            },
        )
        .await
        .expect("append m3 ok");
        assert_eq!(
            append_m3.uid, 3,
            "INBOX uid 3 — uid 2 was moved out, uids never recycle",
        );

        // 9. EXPUNGE INBOX with empty uid set → catches all \Deleted.
        let expunge_reply = call_expunge(
            state.clone(),
            mda,
            ExpungeRequest {
                actor_id: target.to_vec(),
                mailbox: "INBOX".into(),
                uids: vec![],
            },
        )
        .await
        .expect("expunge INBOX ok");
        assert_eq!(
            expunge_reply.expunged_uids,
            vec![3],
            "only uid 3 carried \\Deleted",
        );

        // ── Final-state assertions on the in-memory manifest ──
        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");

        // Projects is in the mailbox table. (The six standard mailboxes
        // are also there — emitted as bootstrap Create records on the
        // first APPEND — so we don't assert total count, just presence.)
        assert!(
            manifest.mailboxes.iter().any(|m| m.name == "Projects"),
            "Projects mailbox-state row must be in the manifest; got {:?}",
            manifest
                .mailboxes
                .iter()
                .map(|m| &m.name)
                .collect::<Vec<_>>(),
        );

        // Two subscriptions: INBOX and Projects.
        assert_eq!(
            manifest.subscriptions.len(),
            2,
            "exactly two subscriptions; got {:?}",
            manifest.subscriptions,
        );
        assert!(manifest.subscriptions.iter().any(|s| s == "INBOX"));
        assert!(manifest.subscriptions.iter().any(|s| s == "Projects"));

        // Two surviving placements: INBOX uid 1 (marked \Seen) and
        // Projects uid 1 (moved from INBOX uid 2). The move dropped
        // INBOX uid 2's placement; the expunge dropped INBOX uid 3's.
        assert_eq!(
            manifest.placements.len(),
            2,
            "two surviving placements: INBOX:1 + Projects:1; got {:?}",
            manifest
                .placements
                .iter()
                .map(|p| (p.mailbox.as_str(), p.uid))
                .collect::<Vec<_>>(),
        );
        let inbox_p = manifest
            .placements
            .iter()
            .find(|p| p.mailbox == "INBOX" && p.uid == 1)
            .expect("INBOX uid 1 placement present");
        assert_eq!(
            inbox_p.flags,
            vec!["\\Seen".to_string()],
            "\\Seen applied by STORE +FLAGS",
        );
        let projects_p = manifest
            .placements
            .iter()
            .find(|p| p.mailbox == "Projects" && p.uid == 1)
            .expect("Projects uid 1 placement present");
        assert_eq!(
            projects_p.content_record_id, append_m2.message_id,
            "Projects:1 carries the same content as the moved INBOX:2",
        );

        // Two tombstones: INBOX uid 2 (moved out) and INBOX uid 3
        // (expunged). MOVE emits a source tombstone, EXPUNGE emits one
        // tombstone per expunged uid; neither STORE nor APPEND emits
        // tombstones, so these two are the full set.
        assert_eq!(
            manifest.tombstones.len(),
            2,
            "exactly two tombstones (INBOX:2 moved + INBOX:3 expunged); got {:?}",
            manifest
                .tombstones
                .iter()
                .map(|t| (t.mailbox.as_str(), t.uid))
                .collect::<Vec<_>>(),
        );
        assert!(
            manifest
                .tombstones
                .iter()
                .any(|t| t.mailbox == "INBOX" && t.uid == 2),
            "INBOX uid 2 tombstone (move source) must be in the manifest",
        );
        assert!(
            manifest
                .tombstones
                .iter()
                .any(|t| t.mailbox == "INBOX" && t.uid == 3),
            "INBOX uid 3 tombstone (expunge) must be in the manifest",
        );

        // ── On-disk manifest equals the in-memory snapshot ──
        // `save_atomic` runs after every `append_event`, so the on-disk
        // bytes must round-trip back to byte-equal of `manifest` above.
        let manifest_path = mail_placement_manifest_path(state.mail_placement.data_dir(), &target);
        let on_disk = MailPlacementManifest::load(&manifest_path)
            .expect("load manifest from disk")
            .expect("manifest file exists after all writes");
        assert_eq!(
            on_disk, manifest,
            "on-disk manifest must equal the in-memory snapshot after every save_atomic",
        );
    }

    // ── T8 restore-divergence tests ────────────────────────────────────────

    /// When the client supplies a QRESYNC hint whose `last_modseq` is strictly
    /// ahead of the server's `highestmodseq` for INBOX (the post-DR-restore
    /// "MUA ahead" case, spec § D6 (γ)), the handler must:
    ///   1. Write one `bridge_restore_divergence` row keyed to the most recent
    ///      `restore_history` row for the actor.
    ///   2. Return `SelectMailboxReply::Selected { .. }` unchanged — server
    ///      state wins; RFC 7162 §3.2.5.2 stale-modseq handling at the MDA
    ///      drives the client into full resync.
    #[tokio::test]
    async fn select_mailbox_writes_divergence_when_client_modseq_ahead() {
        use fauna_protocol::bridge_routing::QResyncHint;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        let actor = [0x79u8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Seed INBOX state via a baseline SELECT (no qresync). This triggers
        // ensure_bridge_imap_mailboxes and gives INBOX highestmodseq = 1.
        let baseline_req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let baseline_payload = Bytes::from(encode_canonical(&baseline_req).unwrap().to_vec());
        select_mailbox_handler()(state.clone(), mda, baseline_payload)
            .await
            .expect("baseline select ok");

        // Seed a restore_history row so write_divergence_row can key to it.
        let fs_id = state
            .db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .expect("get_or_create_reserved_folder");
        let snap_id = state
            .db
            .create_message_kind_snapshot_row(fs_id, "mail", None, None)
            .await
            .expect("create_message_kind_snapshot_row");
        state
            .db
            .insert_restore_history(&actor, snap_id, "mail", None)
            .await
            .expect("insert_restore_history");

        // Call the handler with client_qresync.last_modseq = 999 — ahead of
        // INBOX's hms = 1.
        let req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            client_qresync: Some(QResyncHint {
                last_uid_validity: 1,
                last_modseq: 999,
            }),
            mua_id: Some("Thunderbird/115".into()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Reply must be Selected — server state wins.
        match &reply {
            SelectMailboxReply::Selected { .. } => {}
            other => panic!("expected Selected, got {other:?}"),
        }

        // Exactly one bridge_restore_divergence row must be written.
        let (count, client_ms, server_ms, lost, stored_snap_id, mua): (
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<String>,
        ) = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*),
                        coalesce(MAX(client_modseq), -1),
                        coalesce(MAX(server_modseq), -1),
                        coalesce(MAX(lost_event_count), -1),
                        coalesce(MAX(snapshot_id), -1),
                        MAX(mua_id)
                 FROM bridge_restore_divergence
                 WHERE actor_id = ?1 AND protocol = 'imap'",
                rusqlite::params![actor.as_slice()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("count bridge_restore_divergence")
        };
        assert_eq!(count, 1, "exactly one divergence row must be written");
        assert_eq!(client_ms, 999, "client_modseq must be 999");
        assert_eq!(server_ms, 1, "server_modseq must be 1 (INBOX hms)");
        assert_eq!(
            lost, 998,
            "lost_event_count = client_modseq(999) - server_modseq(1) = 998"
        );
        assert_eq!(
            stored_snap_id, snap_id,
            "divergence row must key to snap_id"
        );
        assert_eq!(
            mua.as_deref(),
            Some("Thunderbird/115"),
            "mua_id must be forwarded from the request"
        );
    }

    /// When the client's QRESYNC last_modseq is at or behind the server's
    /// highestmodseq, the handler must NOT write a divergence row (no-op).
    /// The SELECT reply is still Selected — regression guard.
    #[tokio::test]
    async fn select_mailbox_writes_no_divergence_when_client_modseq_behind() {
        use fauna_protocol::bridge_routing::QResyncHint;

        let state = fixture_state().await;
        let mda = [9u8; 32];
        let actor = [0x7au8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Baseline SELECT to seed INBOX at hms = 1.
        let baseline_req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let baseline_payload = Bytes::from(encode_canonical(&baseline_req).unwrap().to_vec());
        select_mailbox_handler()(state.clone(), mda, baseline_payload)
            .await
            .expect("baseline select ok");

        // Seed restore_history (needed for completeness; no row should be
        // written because client_modseq = 0 <= server_modseq = 1).
        let fs_id = state
            .db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .expect("get_or_create_reserved_folder");
        let snap_id = state
            .db
            .create_message_kind_snapshot_row(fs_id, "mail", None, None)
            .await
            .expect("create_message_kind_snapshot_row");
        state
            .db
            .insert_restore_history(&actor, snap_id, "mail", None)
            .await
            .expect("insert_restore_history");

        // client_modseq = 0 — behind the server's hms = 1.
        let req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            client_qresync: Some(QResyncHint {
                last_uid_validity: 1,
                last_modseq: 0,
            }),
            mua_id: Some("Thunderbird/115".into()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match &reply {
            SelectMailboxReply::Selected { .. } => {}
            other => panic!("expected Selected, got {other:?}"),
        }

        // No divergence row must be written.
        let count: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM bridge_restore_divergence WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count bridge_restore_divergence")
        };
        assert_eq!(
            count, 0,
            "no divergence row must be written when client_modseq is behind server"
        );
    }

    /// When no QRESYNC hint is supplied (client_qresync = None — the normal
    /// path from the Go bridge today), the handler must NOT write a divergence
    /// row. Reply is still Selected. No-op baseline test.
    #[tokio::test]
    async fn select_mailbox_writes_no_divergence_when_no_qresync_hint() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        let actor = [0x7bu8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Baseline SELECT to seed INBOX (no qresync — the default).
        let req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match &reply {
            SelectMailboxReply::Selected { .. } => {}
            other => panic!("expected Selected, got {other:?}"),
        }

        // No divergence row must be written.
        let count: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM bridge_restore_divergence WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("count bridge_restore_divergence")
        };
        assert_eq!(
            count, 0,
            "no divergence row must be written when client_qresync is None"
        );
    }

    /// When SQLite's highestmodseq is ahead of the placement manifest's
    /// per-mailbox highestmodseq (the crash-window (ε) divergence from
    /// spec § D6), the handler must:
    ///   1. Still return Selected { highestmodseq: <sqlite_value>, .. }
    ///      (server state wins — no panic, no error).
    ///   2. NOT advance the manifest's highestmodseq — synthesis is
    ///      deferred to a future track (see imap-server.md § Architectural
    ///      rules). The manifest hms must stay at its original value after
    ///      the SELECT call.
    ///
    /// T9 (Plan 2) — placement-vs-SQLite (ε) detection + WARN log.
    #[tokio::test]
    async fn select_mailbox_logs_warning_when_placement_manifest_lags_sqlite() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        let actor = [0x7cu8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Seed INBOX at hms = 1 via a baseline SELECT.
        let baseline_req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let baseline_payload = Bytes::from(encode_canonical(&baseline_req).unwrap().to_vec());
        select_mailbox_handler()(state.clone(), mda, baseline_payload)
            .await
            .expect("baseline select ok");

        // Read the manifest's INBOX highestmodseq — should be 1 after the
        // bootstrap emit_bootstrap_create_records call.
        let manifest_hms_before = {
            let manifest = state
                .mail_placement
                .load_manifest(&actor)
                .await
                .expect("load_manifest ok");
            manifest
                .mailboxes
                .iter()
                .find(|m| m.name == "INBOX")
                .map(|m| m.highestmodseq)
                .unwrap_or(0)
        };

        // Simulate the crash window: bump SQLite's highestmodseq to 99
        // without touching the manifest (mimics a crash after the SQLite
        // commit but before the manifest fsync).
        {
            let conn = state.db.conn().await;
            conn.execute(
                "UPDATE bridge_imap_mailbox_state \
                 SET highestmodseq = 99 \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![actor.as_slice()],
            )
            .expect("UPDATE highestmodseq");
        }

        // Call select_mailbox_handler — should succeed and return SQLite's
        // value (99), not error out.
        let req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("select after crash-window simulation must not error");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Reply must be Selected with server-side (SQLite) hms = 99.
        match &reply {
            SelectMailboxReply::Selected { highestmodseq, .. } => {
                assert_eq!(
                    *highestmodseq, 99,
                    "handler must return SQLite's highestmodseq (server state wins)"
                );
            }
            other => panic!("expected Selected, got {other:?}"),
        }

        // The manifest must NOT be advanced — synthesis is deferred.
        // Manifest INBOX hms must still equal its pre-crash-window value.
        let manifest_hms_after = {
            let manifest = state
                .mail_placement
                .load_manifest(&actor)
                .await
                .expect("load_manifest after select ok");
            manifest
                .mailboxes
                .iter()
                .find(|m| m.name == "INBOX")
                .map(|m| m.highestmodseq)
                .unwrap_or(0)
        };
        assert_eq!(
            manifest_hms_after, manifest_hms_before,
            "manifest INBOX hms must not be advanced by T9 detection (synthesis deferred)"
        );
        // The gap must still exist — the detection path does not close it.
        assert!(
            (manifest_hms_after as i64) < 99,
            "manifest hms ({manifest_hms_after}) must still lag SQLite (99) — gap was not closed"
        );
    }

    /// When the placement manifest and SQLite agree on highestmodseq, the
    /// handler must return Selected normally with no side-effects. Baseline
    /// regression guard for the (ε) no-op path. Actor 0x7D.
    #[tokio::test]
    async fn select_mailbox_no_warning_when_placement_matches_sqlite() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        let actor = [0x7du8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Baseline SELECT seeds INBOX at hms = 1 in both SQLite and manifest.
        let req = SelectMailboxRequest {
            actor_id: actor.to_vec(),
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = select_mailbox_handler()(state.clone(), mda, payload)
            .await
            .expect("baseline select ok");
        let reply: SelectMailboxReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Reply must be Selected — both sides agree, no divergence.
        match &reply {
            SelectMailboxReply::Selected { highestmodseq, .. } => {
                assert_eq!(*highestmodseq, 1, "initial hms must be 1");
            }
            other => panic!("expected Selected, got {other:?}"),
        }

        // Manifest must reflect hms = 1 as well (in-sync path).
        let manifest = state
            .mail_placement
            .load_manifest(&actor)
            .await
            .expect("load_manifest ok");
        let manifest_hms = manifest
            .mailboxes
            .iter()
            .find(|m| m.name == "INBOX")
            .map(|m| m.highestmodseq)
            .unwrap_or(0);
        assert_eq!(
            manifest_hms, 1,
            "manifest hms must equal SQLite hms = 1 (in-sync)"
        );
    }
}
