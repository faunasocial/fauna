//! WS-RPC handlers for the I2b CalDAV encrypted-mode surface (Phase D).
//!
//! Allowed caller classes: `BridgeMda` (D.2–D.6) and `BridgeMda | User` (D.1).
//!
//! Phase D tasks add to this file:
//!   D.1 — `provision_calendar`
//!   D.2 — `list_calendars`
//!   D.3 — `query_events`
//!   D.4 — `put_event_ciphertext`
//!   D.5 — `delete_event`
//!   D.6 — `sync_calendar_since`

use std::sync::Arc;
use std::time::Duration;

use fauna_calendar::segments::CalFloorMetadata;
use fauna_calendar::segments::placement::CalPlacementRecord;
use fauna_protocol::{
    RpcError,
    bridge_routing::{
        CalendarEntry, DeleteEventReply, DeleteEventRequest, EventEntry, ExpungedEntry,
        ListCalendarsReply, ListCalendarsRequest, PlaceInboundInviteReply,
        PlaceInboundInviteRequest, ProvisionCalendarReply, ProvisionCalendarRequest,
        PutEventCiphertextReply, PutEventCiphertextRequest, QueryEventsReply, QueryEventsRequest,
        SyncCalendarSinceReply, SyncCalendarSinceRequest,
    },
    decode_strict as decode,
};

use crate::bridge_routing_handlers::{
    encode_reply, internal, malformed, placement_journal_diverged, require_class,
    require_dav_caller_scope,
};
use crate::db::bridge_caldav::{
    DeleteCaldavEventOutcome, EventRow, ExpungedRow, ProvisionOutcome, ReplaceCaldavEventOutcome,
    derive_caldav_event_id,
};
use crate::db::now_epoch_secs;
use crate::routes::AppState;
use crate::rpc_router::{RpcKindMeta, RpcRouterBuilder};

/// Map a DB-layer `EventRow` into the wire-shape `EventEntry`, given the sealed
/// body already resolved from wherever it rests. Kept at module scope for reuse
/// in D.6 (`sync_calendar_since`).
fn row_to_event_entry(r: EventRow, encrypted_body: Vec<u8>) -> EventEntry {
    EventEntry {
        event_id: r.event_id.to_vec(),
        uid_hash: r.uid_hash,
        encrypted_body,
        encrypted_index_hint: r.encrypted_index_hint,
        etag: r.etag,
        modseq: r.modseq,
        ciphertext_size: r.ciphertext_size,
        internal_date: r.internal_date,
        encrypted_fauna_ext: r.encrypted_fauna_ext,
    }
}

/// Resolve each row's sealed body through the `__calendar` segment store and map
/// the rows onto the wire shape (S6.6 serve cutover).
///
/// Body resolution is [`crate::segments::cal::load_event_body`]'s contract, not
/// restated here: a row's body lives only in the segment, addressed by the
/// row's stored `record_cid` (the row itself carries no body).
///
/// A row whose body cannot be resolved (a missing `record_cid`, or a segment
/// miss) is served with an empty body and logged, rather than failing the
/// whole REPORT: mirroring `read_envelopes_bulk`'s tolerance, one divergent
/// record must never 500 a MUA's entire sync.
async fn rows_to_event_entries(
    state: &AppState,
    actor: &[u8; 32],
    rows: Vec<EventRow>,
) -> anyhow::Result<Vec<EventEntry>> {
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let body =
            crate::segments::cal::load_event_body(&state.cal_segments, &state.db, actor, &row)
                .await?;
        let body = body.unwrap_or_else(|| {
            tracing::warn!(
                actor = %hex::encode(actor),
                event_id = %hex::encode(row.event_id),
                "calendar event body unresolvable (missing record_cid or a \
                 segment miss); serving empty"
            );
            Vec::new()
        });
        entries.push(row_to_event_entry(row, body));
    }
    Ok(entries)
}

/// Map a DB-layer `ExpungedRow` into the wire-shape `ExpungedEntry`.
fn row_to_expunged_entry(r: ExpungedRow) -> ExpungedEntry {
    ExpungedEntry {
        event_id: r.event_id.to_vec(),
        uid_hash: r.uid_hash,
        modseq: r.modseq,
    }
}

// Entry gate for the `BridgeMda | User` CalDAV r/w RPCs (D.1–D.6): caller scope
// **and** the per-actor serving opt-out — `require_dav_caller_scope` (shared
// with the CardDAV twin) called below with `resource_kind = "calendar"`,
// `resource_noun = "calendar"`.
//
// **Caller scope.** Only the MDA bridge may act on behalf of a *served* user
// (`target != caller`); it AUTH'd the MUA and is trusted to carry the AUTH'd
// actor's id. Every non-bridge caller — `User`, and (via the `Admin ⊇ User`
// promotion in `is_permitted`) `Admin` — may touch only their OWN actor's
// calendars (`target == caller`). This is the load-bearing invariant that makes
// the `BridgeMda | User` allowlist safe for the direct Fauna-app path
// (events.md Decision B, 2026-06-01): a client seals locally and writes under
// its own `actor_id`, so nest never sees plaintext and no caller can reach
// another actor's calendar (caldav-server.md § Threat model, architectural rule
// "MUA-AUTH never grants write access beyond the AUTH'd actor's calendars").
//
// **Serving opt-out.** For the MDA-serving path only (`class == BridgeMda`),
// also refuse when `target` has turned IMAP/CalDAV serving OFF on this nest
// (per-actor, user-set; default ON). This is the CalDAV twin of the IMAP-side
// `require_local_mail_serving` gate — one user-set flag covers both protocols
// the MDA hosts. The user's OWN client path (`User`/`Admin`, `target == caller`)
// is **never** gated by the serving flag: it governs where the *MDA* serves
// external CalDAV clients, not whether the user can read their own calendar via
// their Fauna app (events.md Decision B). Spec:
// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
// § MUA reach.

/// Emit `fauna.calendar.changed` at the calendar owner's own connected
/// clients after a durable calendar write (put Created/Updated, delete
/// Deleted, provision Created/metadata-Updated — never Idempotent /
/// PreconditionFailed / CalendarMissing / AlreadyExists, which leave the DB
/// unchanged; same rule the placement-record appends follow). The caldav
/// twin of `segments::notify_mail_received`: best-effort, own-device fanout
/// (`notify_push`'s single-actor axis), carrying only plaintext the write
/// path already holds. Consumers re-fetch/re-sync; the quick-appearance
/// poll + reconnect re-pull are the correctness backstop
/// (`docs/goal/architecture/transport.md` § Push events, ratified
/// 2026-07-17).
pub(crate) fn notify_calendar_changed(
    state: &std::sync::Arc<crate::routes::AppState>,
    owner: &[u8; 32],
    calendar_id: &[u8; 32],
) {
    state.ws.notify_push(
        owner,
        fauna_protocol::PushEvent::CalendarChanged(
            fauna_protocol::push_events::CalendarChangedPayload {
                actor_id: hex::encode(owner),
                calendar_id: hex::encode(calendar_id),
                extra: std::collections::BTreeMap::new(),
            },
        ),
    );
}

// ── provision_calendar ────────────────────────────────────────────

fn provision_calendar_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.provision_calendar").await?;
            let req: ProvisionCalendarRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;

            // Caller-scoping (see `require_dav_caller_scope`): a non-bridge caller —
            // `User`, or `Admin` via the `Admin ⊇ User` promotion (memory
            // `admin-is-a-user-with-extra-role`) — may provision only its OWN
            // calendar; only the MDA may provision for a served user.
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal: [u8; 32] =
                crate::rpc_errors::require_bytes32("calendar_id", req.calendar_id.as_slice())
                    .map_err(malformed)?;

            // Validate: encrypted_metadata must be non-empty (the client must
            // seal *something* even if just a `{}`-equivalent ciphertext).
            if req.encrypted_metadata.is_empty() {
                return Err(malformed("encrypted_metadata must not be empty"));
            }

            // Branch on `update_metadata`: false = MKCOL-style insert, true =
            // PROPPATCH-style metadata overwrite. The two paths have disjoint
            // outcome spaces (insert: Created/AlreadyExists/Conflict; update:
            // Updated/NotFound), so the same `ProvisionOutcome` enum routes
            // both with no ambiguity.
            let reply = if req.update_metadata {
                let outcome_before = state
                    .db
                    .caldav_calendar_highestmodseq(&target, &cal)
                    .await
                    .map_err(internal)?;
                let outcome = state
                    .db
                    .update_bridge_caldav_calendar_metadata(&target, &cal, &req.encrypted_metadata)
                    .await
                    .map_err(internal)?;
                match outcome {
                    ProvisionOutcome::Updated => {
                        // Emit only when bytes actually changed. The DB layer
                        // signals byte-identical retries by leaving
                        // highestmodseq unchanged; the placement journal must
                        // mirror that no-op (matches MKCOL AlreadyExists).
                        let hms_after = state
                            .db
                            .caldav_calendar_highestmodseq(&target, &cal)
                            .await
                            .map_err(internal)?;
                        if outcome_before != hms_after
                            && let Some(new_hms) = hms_after
                        {
                            let record = CalPlacementRecord::UpdateCalendarMetadata {
                                calendar_id: cal,
                                encrypted_metadata: req.encrypted_metadata.clone(),
                                modseq: new_hms as u64,
                            };
                            state
                                .cal_placement
                                .append_event(&target, &record)
                                .await
                                .map_err(placement_journal_diverged)?;
                            notify_calendar_changed(&state, &target, &cal);
                        }
                        ProvisionCalendarReply::Updated
                    }
                    ProvisionOutcome::NotFound => ProvisionCalendarReply::NotFound,
                    other => unreachable!(
                        "update_bridge_caldav_calendar_metadata cannot return {:?}",
                        other,
                    ),
                }
            } else {
                insert_calendar(&state, target, cal, &req.encrypted_metadata).await?
            };
            encode_reply(&reply)
        })
    })
}

/// Create `target`'s calendar `cal` with its sealed metadata — the MKCOL half of
/// `provision_calendar`, shared with the inbound-invitation placement, which
/// lazily provisions the Personal calendar exactly as a calendar app's first
/// PROPFIND would.
pub(crate) async fn insert_calendar(
    state: &Arc<AppState>,
    target: [u8; 32],
    cal: [u8; 32],
    encrypted_metadata: &[u8],
) -> Result<ProvisionCalendarReply, RpcError> {
    let now = now_epoch_secs();
    let outcome = state
        .db
        .insert_bridge_caldav_calendar(&target, &cal, encrypted_metadata, now)
        .await
        .map_err(internal)?;
    Ok(match outcome {
        ProvisionOutcome::Created => {
            // Spec § D2 (ProvisionCalendar record shape):
            // `calendar_id` + `encrypted_metadata` — the
            // manifest-side apply seeds a fresh `CalendarState`
            // with `highestmodseq = 1` (see
            // `apply_record_to_manifest` in
            // `segments/cal_placement.rs`), matching the DB
            // layer's "post-provision modseq = 1" semantics.
            //
            // Spec § D6 (ε) atomic-with-SQL: the SQLite
            // transaction inside
            // `insert_bridge_caldav_calendar` (the row INSERT)
            // has committed by the time the placement append
            // below runs. The narrow commit-then-append crash
            // window is closed by Plan 2 T9's divergence
            // detection at sync-collection / REPORT time —
            // same pattern as T7's APPEND / STORE / EXPUNGE,
            // T8's MOVE / COPY, T9's CREATE / DELETE / RENAME,
            // T9.5's SUBSCRIBE / UNSUBSCRIBE.
            //
            // No emit on AlreadyExists / Conflict: the DB row
            // is unchanged in both branches, so the journal
            // must mirror the no-op.
            let record = CalPlacementRecord::ProvisionCalendar {
                calendar_id: cal,
                encrypted_metadata: encrypted_metadata.to_vec(),
            };
            state
                .cal_placement
                .append_event(&target, &record)
                .await
                .map_err(placement_journal_diverged)?;
            notify_calendar_changed(state, &target, &cal);
            ProvisionCalendarReply::Created
        }
        ProvisionOutcome::AlreadyExists => ProvisionCalendarReply::AlreadyExists,
        ProvisionOutcome::Conflict => ProvisionCalendarReply::Conflict,
        other => {
            unreachable!("insert_bridge_caldav_calendar cannot return {:?}", other,)
        }
    })
}

// ── list_calendars ────────────────────────────────────────────────

fn list_calendars_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.list_calendars").await?;
            let req: ListCalendarsRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal_rows = state
                .db
                .list_bridge_caldav_calendars(&target)
                .await
                .map_err(internal)?;

            let mut calendars = Vec::with_capacity(cal_rows.len());
            for row in cal_rows {
                let event_count = state
                    .db
                    .count_bridge_caldav_events(&target, &row.calendar_id)
                    .await
                    .map_err(internal)?;
                calendars.push(CalendarEntry {
                    calendar_id: row.calendar_id.to_vec(),
                    encrypted_metadata: row.encrypted_metadata,
                    ctag: row.ctag,
                    highestmodseq: row.highestmodseq,
                    event_count,
                    created_at: row.created_at,
                });
            }
            encode_reply(&ListCalendarsReply { calendars })
        })
    })
}

// ── query_events ──────────────────────────────────────────────────

fn query_events_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.query_events").await?;
            let req: QueryEventsRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal: [u8; 32] =
                crate::rpc_errors::require_bytes32("calendar_id", req.calendar_id.as_slice())
                    .map_err(malformed)?;

            // Validate after_event_id length if present.
            if let Some(ref b) = req.after_event_id
                && b.len() != 32
            {
                return Err(malformed("after_event_id must be 32 bytes"));
            }

            // Check calendar exists; capture highestmodseq without a second query.
            let hms = match state
                .db
                .caldav_calendar_highestmodseq(&target, &cal)
                .await
                .map_err(internal)?
            {
                None => return encode_reply(&QueryEventsReply::CalendarNotFound),
                Some(h) => h,
            };

            // Pass wire_limit + 1 to DB so pagination detection works inside
            // query_caldav_events (it trims and sets EventPage::more).
            let fetch_limit = if req.limit == 0 {
                0
            } else {
                req.limit.saturating_add(1)
            };

            let after_owned: Option<[u8; 32]> = req
                .after_event_id
                .as_ref()
                .map(|b| b.as_ref().try_into().unwrap()); // safe: validated above

            let page = state
                .db
                .query_caldav_events(
                    &target,
                    &cal,
                    req.since_modseq,
                    after_owned.as_ref(),
                    fetch_limit,
                )
                .await
                .map_err(internal)?;

            let events = rows_to_event_entries(&state, &target, page.events)
                .await
                .map_err(internal)?;

            encode_reply(&QueryEventsReply::Ok {
                events,
                highestmodseq: hms,
                more: page.more,
            })
        })
    })
}

// ── put_event_ciphertext ──────────────────────────────────────────

fn put_event_ciphertext_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.put_event_ciphertext").await?;
            let req: PutEventCiphertextRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal: [u8; 32] =
                crate::rpc_errors::require_bytes32("calendar_id", req.calendar_id.as_slice())
                    .map_err(malformed)?;

            if req.uid_hash.len() != 32 {
                return Err(malformed("uid_hash must be 32 bytes"));
            }
            if req.encrypted_body.is_empty() {
                return Err(malformed("encrypted_body must not be empty"));
            }
            if req.encrypted_index_hint.is_empty() {
                return Err(malformed("encrypted_index_hint must not be empty"));
            }
            if req.ciphertext_size as usize != req.encrypted_body.len() {
                return Err(malformed("ciphertext_size must equal encrypted_body.len()"));
            }
            // S6.12: the at-rest seal is structural, not a caller convention.
            // This RPC is allowlisted `BridgeMda | User`, and the body goes
            // straight into the backup-eligible `__calendar` segment store —
            // so the seal must be *proven* at the wire edge, not assumed of
            // the caller. Every production caller already seals both halves
            // (Go MDA `caldav/put.go`, client `seal_and_put_event`); anything
            // else is refused before any state is touched. The typed values
            // are the only currency `segments::cal` appends accept.
            let sealed_body =
                fauna_mls::wrapped_blob::SealedRecordBytes::verify(req.encrypted_body.clone())
                    .map_err(|_| malformed("encrypted_body is not a sealed recipient envelope"))?;
            let sealed_hint = fauna_mls::wrapped_blob::SealedRecordBytes::verify(
                req.encrypted_index_hint.clone(),
            )
            .map_err(|_| malformed("encrypted_index_hint is not a sealed recipient envelope"))?;

            let reply =
                store_sealed_event(&state, target, cal, &req, sealed_body, sealed_hint).await?;
            encode_reply(&reply)
        })
    })
}

/// Store one sealed event body into `target`'s calendar `cal` — the durable
/// half of `put_event_ciphertext`, shared with the inbound-invitation placement
/// (`place_sealed_invite`) so an emailed invitation lands on the calendar by
/// exactly the path a calendar app's PUT does: the segment append first, then
/// the row, then the placement journal and the change notification. The caller
/// has already authorized the write and proven both halves sealed.
pub(crate) async fn store_sealed_event(
    state: &Arc<AppState>,
    target: [u8; 32],
    cal: [u8; 32],
    req: &PutEventCiphertextRequest,
    sealed_body: fauna_mls::wrapped_blob::SealedRecordBytes,
    sealed_hint: fauna_mls::wrapped_blob::SealedRecordBytes,
) -> Result<PutEventCiphertextReply, RpcError> {
    // The shared storage quota (`caldav-server.md` § QUOTA → § Enforcement
    // points), checked before the segment append below so a refusal writes
    // nothing. Here rather than in the PUT handler so an emailed invitation's
    // placement is held to it too.
    let replaced = state
        .db
        .caldav_event_ciphertext_size(&target, &cal, &req.uid_hash)
        .await
        .map_err(internal)?;
    crate::bridge_imap_handlers::enforce_dav_write_quota(
        state,
        &target,
        req.ciphertext_size,
        replaced,
    )
    .await?;

    let now = now_epoch_secs();

    // S6.6 content cutover (design § D7). The sealed body rests only in
    // the actor's `__calendar` segment store; the `bridge_caldav_events`
    // row carries metadata and the record's `record_cid`, never the body.
    //
    // Ordering is load-bearing and cannot be reversed: **the content
    // record must be durable before the metadata row exists.** A row
    // committed first would reference a body the segment does not hold,
    // and a client retry — deriving the same `event_id` from
    // the same bytes — would take the idempotent path and never
    // re-append it. That is silent, user-irrecoverable loss (alpha
    // no-data-loss). The reverse crash window is benign: an appended
    // record with no row is unreachable (every read starts from a row)
    // and compaction reclaims it.
    //
    // `event_id` is therefore derived here, at the ingest perimeter,
    // and handed to the DAO — which never sees the body, so cannot hash
    // it. `ensure_in_segment` is idempotent on the
    // record's content-hash CID (a retry files byte-identical envelope
    // bytes and re-derives the same identity), so a retry of an
    // already-appended body is a no-op rather than a duplicate record.
    let event_id = derive_caldav_event_id(&target, req.timestamp, &req.encrypted_body);
    // `created_at: now` (→ the row's `received_at`) MUST stay
    // server-assigned: the orphan reaper's 1h in-flight watermark
    // keys on it, so honoring a client timestamp here would let a
    // client backdate a record into instant reapability. The
    // client's `req.timestamp` goes to `internal_date` only.
    let floor = CalFloorMetadata {
        calendar_id: cal,
        event_id,
        uid_hash: req.uid_hash.clone(),
        ciphertext_size: req.ciphertext_size,
        internal_date: req.timestamp,
        created_at: now,
        ..Default::default()
    };
    let record_cid = crate::segments::cal::ensure_in_segment(
        &state.cal_segments,
        &state.db,
        &target,
        &sealed_body,
        &sealed_hint,
        &floor,
    )
    .await
    .map_err(internal)?;

    let outcome = state
        .db
        .replace_caldav_event_by_uid(
            &target,
            &cal,
            &req.uid_hash,
            req.if_match.as_deref(),
            &event_id,
            &record_cid,
            &req.encrypted_index_hint,
            req.encrypted_fauna_ext.as_deref(),
            req.timestamp,
            req.ciphertext_size,
            now,
        )
        .await
        .map_err(internal)?;

    let reply = match outcome {
        ReplaceCaldavEventOutcome::Created {
            event_id,
            etag,
            modseq,
            encrypted_fauna_ext,
        } => {
            // Spec § D2 (PutEvent record shape):
            // `calendar_id` + `uid_hash` + `etag` + `modseq` +
            // `ciphertext_size`. The manifest-side apply
            // dedups on (calendar_id, uid_hash) and bumps the
            // calendar's highestmodseq (see
            // `apply_record_to_manifest` in
            // `segments/cal_placement.rs`).
            //
            // Spec § D6 (ε) atomic-with-SQL: the SQLite
            // transaction inside `replace_caldav_event_by_uid`
            // (the event INSERT/UPDATE + modseq bump) has
            // committed by the time the placement append
            // below runs. The narrow commit-then-append crash
            // window is closed by Plan 2 T9's divergence
            // detection at sync-collection / REPORT time —
            // same pattern as T7 / T8 / T9 / T9.5.
            //
            // No emit on Idempotent / PreconditionFailed /
            // CalendarMissing: Idempotent retries do not bump
            // the row (the existing "no bump was done"
            // comment below documents this), and the latter
            // two paths leave the DB unchanged.
            let record = CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: req
                    .uid_hash
                    .as_slice()
                    .try_into()
                    .expect("uid_hash validated to length 32 above"),
                etag: etag.clone(),
                modseq: modseq as u64,
                ciphertext_size: req.ciphertext_size,
                // v2 (S6.9): the content-record id (the placement→
                // content join restore needs) + the row's EFFECTIVE
                // sidecar — the DAO's value, not the request's (a MUA
                // write preserves the prior row's sidecar).
                event_id,
                encrypted_fauna_ext: encrypted_fauna_ext.clone(),
            };
            state
                .cal_placement
                .append_event(&target, &record)
                .await
                .map_err(placement_journal_diverged)?;
            notify_calendar_changed(state, &target, &cal);
            PutEventCiphertextReply::Created {
                event_id: event_id.to_vec(),
                etag,
                modseq,
            }
        }
        ReplaceCaldavEventOutcome::Updated {
            event_id,
            etag,
            modseq,
            encrypted_fauna_ext,
        } => {
            // See Created arm: same PutEvent record shape +
            // atomicity contract. An Updated outcome differs
            // from Created only in the DB row being replaced
            // (rather than newly inserted) and a tombstone
            // for the prior event_id being recorded inside
            // the same SQLite tx — neither alters the
            // placement-record schema.
            let record = CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: req
                    .uid_hash
                    .as_slice()
                    .try_into()
                    .expect("uid_hash validated to length 32 above"),
                etag: etag.clone(),
                modseq: modseq as u64,
                ciphertext_size: req.ciphertext_size,
                // v2 (S6.9): the content-record id (the placement→
                // content join restore needs) + the row's EFFECTIVE
                // sidecar — the DAO's value, not the request's (a MUA
                // write preserves the prior row's sidecar).
                event_id,
                encrypted_fauna_ext: encrypted_fauna_ext.clone(),
            };
            state
                .cal_placement
                .append_event(&target, &record)
                .await
                .map_err(placement_journal_diverged)?;
            notify_calendar_changed(state, &target, &cal);
            PutEventCiphertextReply::Updated {
                event_id: event_id.to_vec(),
                etag,
                modseq,
            }
        }
        // Idempotent transport retry: the new body hashes to the same event_id as the
        // prior row, so no bump was done. The wire shape has no Idempotent variant;
        // reporting Updated is semantically coherent because the row state already
        // matches what the caller intended — the caller can use the returned etag
        // for subsequent conditional requests without distinguishing new-update from retry.
        // No placement record emitted (the row state is unchanged) — see Created arm.
        ReplaceCaldavEventOutcome::Idempotent {
            event_id,
            etag,
            modseq,
            encrypted_fauna_ext: _,
        } => PutEventCiphertextReply::Updated {
            event_id: event_id.to_vec(),
            etag,
            modseq,
        },
        ReplaceCaldavEventOutcome::PreconditionFailed { current_etag } => {
            PutEventCiphertextReply::PreconditionFailed { current_etag }
        }
        ReplaceCaldavEventOutcome::CalendarMissing => PutEventCiphertextReply::CalendarNotFound,
    };
    Ok(reply)
}

// ── delete_event ──────────────────────────────────────────────────

fn delete_event_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.delete_event").await?;
            let req: DeleteEventRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal: [u8; 32] =
                crate::rpc_errors::require_bytes32("calendar_id", req.calendar_id.as_slice())
                    .map_err(malformed)?;

            if req.uid_hash.len() != 32 {
                return Err(malformed("uid_hash must be 32 bytes"));
            }

            let now = now_epoch_secs();
            let outcome = state
                .db
                .delete_caldav_event_by_uid(
                    &target,
                    &cal,
                    &req.uid_hash,
                    req.if_match.as_deref(),
                    now,
                )
                .await
                .map_err(internal)?;

            let reply = match outcome {
                DeleteCaldavEventOutcome::Deleted { event_id, modseq } => {
                    // Spec § D2 (DeleteEvent record shape):
                    // `calendar_id` + `uid_hash` + `modseq`. The
                    // manifest-side apply drops the event placement
                    // and pushes a tombstone (`EventTombstoneRef`)
                    // unconditionally — even if the local manifest
                    // is drifted vs the upstream store — so that
                    // WebDAV-Sync REPORT surfaces the deletion to
                    // the client (see `apply_record_to_manifest`
                    // in `segments/cal_placement.rs`).
                    //
                    // Spec § D6 (ε) atomic-with-SQL: the SQLite
                    // transaction inside
                    // `delete_caldav_event_by_uid` (the row DELETE
                    // + tombstone insert + modseq bump) has
                    // committed by the time the placement append
                    // below runs. The narrow commit-then-append
                    // crash window is closed by Plan 2 T9's
                    // divergence detection at sync-collection /
                    // REPORT time — same pattern as T7 / T8 / T9
                    // / T9.5.
                    //
                    // No emit on NotFound / PreconditionFailed:
                    // neither path mutates the DB.
                    let record = CalPlacementRecord::DeleteEvent {
                        calendar_id: cal,
                        uid_hash: req
                            .uid_hash
                            .as_slice()
                            .try_into()
                            .expect("uid_hash validated to length 32 above"),
                        modseq: modseq as u64,
                        // v2 (S6.9): the deleted row's content-record id
                        // (restore rebuilds the `bridge_caldav_expunged` row
                        // from it) + the delete time that makes the S6.8d2
                        // retention prune possible.
                        event_id,
                        deleted_at: now,
                    };
                    state
                        .cal_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    // S6.8d2 — age out tombstones for UIDs deleted long ago
                    // and never re-added, below the same effective retention
                    // window the sync-collection serve path enforces on
                    // `bridge_caldav_expunged` (past it a client is told
                    // `stale` and full-resyncs, so the prune is
                    // unobservable). Piggybacked on DELETE because it is the
                    // only operation that grows the tombstone set.
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
                    state
                        .cal_placement
                        .prune_tombstones(&target, now - retention_days * 86_400)
                        .await
                        .map_err(internal)?;
                    notify_calendar_changed(&state, &target, &cal);
                    DeleteEventReply::Deleted {
                        event_id: event_id.to_vec(),
                        modseq,
                    }
                }
                DeleteCaldavEventOutcome::NotFound => DeleteEventReply::NotFound,
                DeleteCaldavEventOutcome::PreconditionFailed { current_etag } => {
                    DeleteEventReply::PreconditionFailed { current_etag }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── sync_calendar_since ───────────────────────────────────────────

fn sync_calendar_since_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.sync_calendar_since").await?;
            let req: SyncCalendarSinceRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "calendar", "calendar")
                .await?;

            let cal: [u8; 32] =
                crate::rpc_errors::require_bytes32("calendar_id", req.calendar_id.as_slice())
                    .map_err(malformed)?;

            // Parse sync_token: "0" = full sync; anything else must be a
            // decimal i64 modseq. Negative values are rejected — modseq is
            // never negative, and an MDA passing "-1" is buggy; coercing
            // silently to 0 would mask the bug.
            let since_modseq: i64 = if req.sync_token == "0" {
                0
            } else {
                let n = req
                    .sync_token
                    .parse::<i64>()
                    .map_err(|_| malformed("sync_token must be a decimal i64 or \"0\""))?;
                if n < 0 {
                    return Err(malformed("sync_token must be a decimal i64 or \"0\""));
                }
                n
            };

            // Check calendar exists; capture highestmodseq (becomes new_sync_token).
            let hms = match state
                .db
                .caldav_calendar_highestmodseq(&target, &cal)
                .await
                .map_err(internal)?
            {
                None => return encode_reply(&SyncCalendarSinceReply::CalendarNotFound),
                Some(h) => h,
            };

            // Restore-divergence detection (spec § D6 (γ)). When the
            // client's sync_token is ahead of the calendar's current
            // highestmodseq, the most plausible cause is a DR restore
            // that landed the calendar at an earlier modseq than the
            // MUA had observed. Log the divergence + return Stale so
            // the MUA falls through to full PROPFIND per RFC 6578 §3.8.
            if since_modseq > hms {
                let now = fauna_core::data::Timestamp::now_secs_or_zero();
                let cal_hex = hex::encode(cal);
                {
                    let conn = state.db.conn().await;
                    let tx = conn.unchecked_transaction().map_err(internal)?;
                    crate::restore::divergence::write_divergence_row(
                        &tx,
                        &target,
                        "caldav",
                        &cal_hex,
                        req.mua_id.as_deref(),
                        since_modseq,
                        hms,
                        now,
                    )
                    .map_err(internal)?;
                    tx.commit().map_err(internal)?;
                }
                return encode_reply(&SyncCalendarSinceReply::Stale { server_modseq: hms });
            }

            // Stale-sync-token-past-retention detection (caldav-server.md
            // § Stale sync-token handling). The token is valid (<= hms) but
            // may predate the tombstone-retention window: if any tombstone
            // newer than since_modseq was expunged before the retention
            // cutoff, nest can no longer honestly enumerate the deletions
            // since the token, so it signals stale and the MUA full-resyncs
            // (PROPFIND + per-event GET). Distinct from the Stale (MUA-ahead)
            // branch above — retention expiry is expected, not forensic, so
            // NO bridge_restore_divergence row is written. A full sync
            // (since_modseq == 0) asks for everything and can never be stale,
            // so the check is scoped to incremental syncs.
            if since_modseq > 0 {
                // The retention window is the *effective* deployment policy
                // (catalog ⊕ admin `put_imap_policy` override) with the
                // nest-enforced 7-day floor (imap-server.md § Tombstone
                // retention). Reads the same `mail_imap_policy` source as the
                // IMAP quota/delete handlers so an admin lowering retention
                // binds here too.
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
                let cutoff_ts = now_epoch_secs() - retention_days * 86_400;
                if state
                    .db
                    .caldav_has_expunged_past_retention(&target, &cal, since_modseq, cutoff_ts)
                    .await
                    .map_err(internal)?
                {
                    return encode_reply(&SyncCalendarSinceReply::Ok {
                        changed: vec![],
                        expunged: vec![],
                        new_sync_token: hms.to_string(),
                        more: false,
                        stale: true,
                    });
                }
            }

            // Pass wire_limit + 1 to DB so pagination detection works inside
            // query_caldav_changes_since (it trims and sets EventPage::more).
            let fetch_limit = if req.limit == 0 {
                0
            } else {
                req.limit.saturating_add(1)
            };

            // query_caldav_changes_since orders by modseq ASC so the modseq
            // cursor is always valid for resuming the next page.
            let page = state
                .db
                .query_caldav_changes_since(&target, &cal, since_modseq, fetch_limit)
                .await
                .map_err(internal)?;

            let expunged_rows = state
                .db
                .query_caldav_expunged_since(&target, &cal, since_modseq)
                .await
                .map_err(internal)?;

            let changed = rows_to_event_entries(&state, &target, page.events)
                .await
                .map_err(internal)?;
            let expunged: Vec<ExpungedEntry> = expunged_rows
                .into_iter()
                .map(row_to_expunged_entry)
                .collect();

            // When more == true: use the modseq of the last returned event as
            // new_sync_token so the next paged call resumes from exactly the
            // right place (modseq > last_returned_modseq).  Safety: `more ==
            // true` is only set when `events.len() == wire_limit >= 1`, so
            // `changed.last()` is always Some here.
            // When more == false: use the calendar-wide highestmodseq so that
            // future incremental syncs pick up all changes made after this call.
            let new_sync_token = if page.more {
                changed.last().unwrap().modseq.to_string()
            } else {
                hms.to_string()
            };
            encode_reply(&SyncCalendarSinceReply::Ok {
                changed,
                expunged,
                new_sync_token,
                more: page.more,
                // Steady-state reply: the token is within the retention
                // window (the past-retention branch above already returned).
                stale: false,
            })
        })
    })
}

// ── place_inbound_invite ──────────────────────────────────────────

/// Place an emailed invitation, already sealed to `target`, on their calendar —
/// the one placement both delivery paths share (`caldav-server.md` §
/// Server-side auto-schedule, "Inbound invite"): the MTA's `place_inbound_invite`
/// for a sender on another server, and [`place_local_invite`] for a sender on
/// this nest's own domain.
///
/// **The mail gate first:** the invitation goes wherever its mail went, so a
/// sender the recipient's guardian mail gate holds (`family-safety.md` § The mail
/// gate) gets `Withheld` — recomputed here from the stored policy, as every
/// sealed delivery's verdict is. **Then create-only:** an event with the same
/// `UID` on any of the actor's calendars wins and is left byte-for-byte
/// untouched, because email proves nothing about who sent it (the invitation
/// stays in the inbox either way). Otherwise the event lands in the lazy
/// `Personal` calendar — provisioned first if the actor has none, its metadata
/// sealed here to the recipient (encrypt-only, the public key is all it takes)
/// — through the same storage path a calendar app's PUT takes.
pub(crate) async fn place_sealed_invite(
    state: &Arc<AppState>,
    target: [u8; 32],
    uid_hash: [u8; 32],
    body: fauna_mls::wrapped_blob::SealedRecordBytes,
    hint: fauna_mls::wrapped_blob::SealedRecordBytes,
    timestamp: i64,
    ingress: fauna_core::data::MailIngress<'_>,
) -> Result<PlaceInboundInviteReply, RpcError> {
    let gate =
        crate::bridge_routing_handlers::guardian_mail_verdict(state, &target, ingress).await?;
    if gate.verdict != fauna_core::data::MailVerdict::Deliver {
        return Ok(PlaceInboundInviteReply::Withheld);
    }
    if state
        .db
        .caldav_uid_on_any_calendar(&target, &uid_hash)
        .await
        .map_err(internal)?
    {
        return Ok(PlaceInboundInviteReply::AlreadyOnCalendar);
    }
    let cal = fauna_protocol::dav_identity::personal_calendar_id();
    if !state
        .db
        .ensure_bridge_caldav_calendar_exists(&target, &cal)
        .await
        .map_err(internal)?
    {
        let keys = recipient_calendar_keys(state, &target).await?;
        let metadata = fauna_protocol::encode_canonical(
            &fauna_protocol::dav_identity::CalendarMetadata::personal(),
        )
        .map_err(internal)?;
        let metadata = crate::bridge_routing_handlers::seal_recipient_blob(
            &metadata,
            &keys.mls_pubkey,
            Some(keys.mlkem_ek.as_slice()),
            "calendar metadata",
        )?;
        // Created, or AlreadyExists / Conflict when a calendar app provisioned
        // it concurrently — the calendar exists in every case.
        insert_calendar(state, target, cal, &metadata).await?;
    }
    let req = PutEventCiphertextRequest {
        actor_id: target.to_vec(),
        calendar_id: cal.to_vec(),
        uid_hash: uid_hash.to_vec(),
        encrypted_body: body.as_slice().to_vec(),
        encrypted_index_hint: hint.as_slice().to_vec(),
        timestamp,
        ciphertext_size: body.len() as u32,
        if_match: None,
        encrypted_fauna_ext: None,
    };
    let stored = match store_sealed_event(state, target, cal, &req, body, hint).await {
        // No room on the account (`caldav-server.md` § QUOTA → § Enforcement
        // points): the invitation is not placed. The mail copy is delivered
        // already, so this is an outcome the MTA logs, never an error that
        // could read as a failed delivery.
        Err(e) if e.code == RpcError::CODE_BRIDGES_OVER_QUOTA => {
            return Ok(PlaceInboundInviteReply::OverQuota);
        }
        stored => stored?,
    };
    match stored {
        PutEventCiphertextReply::Created { .. } => Ok(PlaceInboundInviteReply::Placed),
        // A calendar app stored the same UID between the check above and the
        // write; its body lost the race to the organizer's, which is what the
        // invitation said anyway. Nothing the user wrote is at stake: they had
        // not seen this event before they stored it.
        PutEventCiphertextReply::Updated { .. } => Ok(PlaceInboundInviteReply::Placed),
        PutEventCiphertextReply::PreconditionFailed { .. } => {
            Ok(PlaceInboundInviteReply::AlreadyOnCalendar)
        }
        PutEventCiphertextReply::CalendarNotFound => Err(internal(
            "the Personal calendar vanished between provisioning and the write",
        )),
        // The nest's own store never returns `Unknown`; if it ever did, the
        // placement is not confirmed, so report it rather than claim it landed.
        PutEventCiphertextReply::Unknown => {
            Err(internal("the event store answered with an unknown outcome"))
        }
    }
}

/// The keys a calendar object for `target` seals to — the actor's non-epoch
/// recipient key, the one a CalDAV PUT's body and the calendar metadata seal to
/// (the MDA reads the same row at AUTH).
async fn recipient_calendar_keys(
    state: &Arc<AppState>,
    target: &[u8; 32],
) -> Result<crate::db::bridge_routing::RecipientSealKey, RpcError> {
    state
        .db
        .get_recipient_seal_key(target)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("recipient has no calendar key on file"))
}

/// Place an invitation carried by mail this nest delivers itself — a sender on
/// the nest's own domain, which never crosses the MTA (the in-domain partition
/// of `enqueue_outbound_mail` and `fauna.email.send`). The nest holds the
/// message in cleartext for exactly as long as the mail seal already does, and
/// seals the event the way the MDA seals a CalDAV PUT: body to the actor's
/// calendar key, index hint to their index key. Best-effort, like the delivery
/// it rides on: a message that is not an invitation, or any failure, leaves the
/// mail delivered and the calendar as it was.
pub(crate) async fn place_local_invite(
    state: &Arc<AppState>,
    target: &[u8; 32],
    raw: &[u8],
    ingress: fauna_core::data::MailIngress<'_>,
) {
    let now = now_epoch_secs();
    let Some(invite) = fauna_mail::icalendar::invite_from_mail(raw.to_vec(), now) else {
        return;
    };
    let outcome = async {
        let keys = recipient_calendar_keys(state, target).await?;
        let index_key = state
            .db
            .get_actor_index_pubkey(target)
            .await
            .map_err(internal)?
            .unwrap_or(keys.mls_pubkey);
        // The hint goes post-quantum only when it seals to the same key as the
        // body — the MDA's `IndexHintMlkemEk` rule.
        let hint_ek = (index_key == keys.mls_pubkey).then_some(keys.mlkem_ek.as_slice());
        let seal = crate::bridge_routing_handlers::seal_recipient_blob;
        let body = seal(
            invite.ics.as_bytes(),
            &keys.mls_pubkey,
            Some(keys.mlkem_ek.as_slice()),
            "invite body",
        )?;
        let tokens = fauna_mail::tokenizer::tokenize(&invite.ics).canonical_bytes;
        let hint = seal(&tokens, &index_key, hint_ek, "invite index-hint")?;
        place_sealed_invite(
            state,
            *target,
            fauna_protocol::dav_identity::uid_hash(&invite.uid),
            fauna_mls::wrapped_blob::SealedRecordBytes::verify(body).map_err(internal)?,
            fauna_mls::wrapped_blob::SealedRecordBytes::verify(hint).map_err(internal)?,
            now,
            ingress,
        )
        .await
    }
    .await;
    match outcome {
        Err(e) => tracing::warn!(
            actor = %hex::encode(target),
            error = %e.code,
            "an emailed invitation was delivered to the inbox but not placed on the calendar"
        ),
        Ok(PlaceInboundInviteReply::OverQuota) => tracing::info!(
            actor = %hex::encode(target),
            "an emailed invitation was delivered to the inbox but not placed: the account's storage is full"
        ),
        Ok(_) => {}
    }
}

fn place_inbound_invite_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.place_inbound_invite").await?;
            let req: PlaceInboundInviteRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            let uid_hash: [u8; 32] =
                crate::rpc_errors::require_bytes32("uid_hash", req.uid_hash.as_slice())
                    .map_err(malformed)?;
            // The same wire-edge proof `put_event_ciphertext` demands: everything
            // this kind stores rests sealed, so it is verified here rather than
            // assumed of the caller.
            let sealed = |bytes: Vec<u8>, what: &str| {
                fauna_mls::wrapped_blob::SealedRecordBytes::verify(bytes)
                    .map_err(|_| malformed(format!("{what} is not a sealed recipient envelope")))
            };
            let body = sealed(req.encrypted_body, "encrypted_body")?;
            let hint = sealed(req.encrypted_index_hint, "encrypted_index_hint")?;
            let reply = place_sealed_invite(
                &state,
                target,
                uid_hash,
                body,
                hint,
                req.timestamp,
                fauna_core::data::MailIngress::from_envelope(&req.sender_address, None),
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

// ── Registration entry point ──────────────────────────────────────

pub fn register_bridge_caldav_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.place_inbound_invite",
        RpcKindMeta {
            forbid_replay: false,
            // 60 s routing deadline — it carries a sealed event body like a PUT
            // does (matches the `routing` deadline `kind.rs`'s KindRegistry
            // registers for this kind; `rpc_router::tests::
            // router_and_kind_registry_agree_on_every_kind` pins the two together).
            default_deadline: Duration::from_secs(60),
            handler: place_inbound_invite_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_calendar",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: provision_calendar_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_calendars",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_calendars_handler(),
        },
    );
    b.add(
        "fauna.bridges.query_events",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: query_events_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_event_ciphertext",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: put_event_ciphertext_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_event",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_event_handler(),
        },
    );
    b.add(
        "fauna.bridges.sync_calendar_since",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: sync_calendar_since_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use super::*;
    use crate::bridge_approval_test_support::approve_bridge;
    use crate::db::bridge_service_users::BridgeRole;
    use crate::routes::AppState;
    use crate::test_support::expect_no_push;
    use fauna_protocol::encode_canonical;
    use fauna_segment_store::VersionedManifest;

    // ── Test fixtures ─────────────────────────────────────────────

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    // ── provision_calendar tests ──────────────────────────────────

    #[tokio::test]
    async fn provision_calendar_created_inserts_row() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [20u8; 32];
        let cal_id = [30u8; 32];
        let meta = b"sealed-calendar-metadata".to_vec();

        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: meta,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // Row must exist.
        let exists = state
            .db
            .ensure_bridge_caldav_calendar_exists(&actor, &cal_id)
            .await
            .unwrap();
        assert!(exists, "calendar row must be present after Created");
    }

    #[tokio::test]
    async fn provision_calendar_identical_bytes_returns_already_exists() {
        let state = fixture_state().await;
        let mda = [11u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [21u8; 32];
        let cal_id = [31u8; 32];
        let meta = b"same-metadata".to_vec();

        let make_payload = || {
            let req = ProvisionCalendarRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                encrypted_metadata: meta.clone(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created.
        let bytes = provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // Second call with identical bytes → AlreadyExists.
        let bytes = provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::AlreadyExists);

        // Only one row.
        let rows = state.db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "must be exactly one calendar row after idempotent retry"
        );
    }

    #[tokio::test]
    async fn provision_calendar_differing_bytes_returns_conflict_and_preserves_original() {
        let state = fixture_state().await;
        let mda = [12u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [22u8; 32];
        let cal_id = [32u8; 32];
        let meta_orig = b"original-metadata".to_vec();
        let meta_new = b"different-metadata".to_vec();

        let make_payload = |meta: Vec<u8>| {
            let req = ProvisionCalendarRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                encrypted_metadata: meta,
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created.
        let bytes =
            provision_calendar_handler()(state.clone(), mda, make_payload(meta_orig.clone()))
                .await
                .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // Second call with different bytes → Conflict.
        let bytes = provision_calendar_handler()(state.clone(), mda, make_payload(meta_new))
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Conflict);

        // Original metadata bytes are unchanged.
        let rows = state.db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].encrypted_metadata, meta_orig,
            "original metadata must be preserved on Conflict"
        );
    }

    #[tokio::test]
    async fn provision_calendar_update_metadata_returns_updated() {
        let state = fixture_state().await;
        let mda = [25u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [26u8; 32];
        let cal_id = [27u8; 32];

        // Provision first (MKCOL path).
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"meta-v1".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // PROPPATCH path: update_metadata=true with new bytes.
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"meta-v2-resealed".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .expect("update handler ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Updated);

        let rows = state.db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].encrypted_metadata, b"meta-v2-resealed");
    }

    #[tokio::test]
    async fn provision_calendar_update_metadata_returns_not_found_when_calendar_missing() {
        let state = fixture_state().await;
        let mda = [28u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // No prior insert — calendar_id [29u8; 32] does not exist.
        let req = ProvisionCalendarRequest {
            actor_id: vec![29u8; 32],
            calendar_id: vec![29u8; 32],
            encrypted_metadata: b"meta-resealed".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .expect("update handler ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::NotFound);

        // Update must not create a row.
        let rows = state
            .db
            .list_bridge_caldav_calendars(&[29u8; 32])
            .await
            .unwrap();
        assert!(rows.is_empty(), "NotFound path must not insert rows");
    }

    #[tokio::test]
    async fn provision_calendar_malformed_wrong_length_calendar_id() {
        let state = fixture_state().await;
        let mda = [13u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![1u8; 16], // wrong length
            encrypted_metadata: b"some-metadata".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_calendar_malformed_empty_encrypted_metadata() {
        let state = fixture_state().await;
        let mda = [14u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![1u8; 32],
            encrypted_metadata: vec![], // empty — invalid
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_calendar_malformed_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [15u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionCalendarRequest {
            actor_id: vec![1u8; 16], // wrong length
            calendar_id: vec![1u8; 32],
            encrypted_metadata: b"some-metadata".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_calendar_user_class_accepted() {
        let state = fixture_state().await;
        // No bridge service-user row, no admin row, but a `users` row → the
        // authority gate resolves it to User class.
        let user_actor = [50u8; 32];
        let cal_id = [60u8; 32];
        state
            .db
            .create_user(&user_actor, "free", "test")
            .await
            .unwrap();

        let req = ProvisionCalendarRequest {
            actor_id: user_actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"user-sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), user_actor, payload)
            .await
            .expect("User class must be accepted");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);
    }

    #[tokio::test]
    async fn provision_calendar_bridge_mta_denied() {
        let state = fixture_state().await;
        let mta = [70u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn provision_calendar_admin_other_actor_denied() {
        // admin ⊇ user, but the kind is caller-scoped: an admin inherits the
        // User grant and may provision its OWN calendar, never another actor's.
        // Targeting a different actor is denied (only the MDA bridge may
        // provision on behalf of a served user). The guard is what keeps the
        // `Admin ⊇ User` inheritance safe.
        let state = fixture_state().await;
        let admin_actor = [80u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

        let req = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32], // a DIFFERENT actor
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn provision_calendar_admin_own_accepted() {
        // admin ⊇ user: an admin may provision its OWN calendar (target ==
        // caller), inheriting the User grant.
        let state = fixture_state().await;
        let admin_actor = [80u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

        let req = ProvisionCalendarRequest {
            actor_id: admin_actor.to_vec(), // SELF
            calendar_id: [2u8; 32].to_vec(),
            encrypted_metadata: b"admin-own-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state, admin_actor, payload)
            .await
            .expect("an admin provisioning its own calendar must be accepted");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);
    }

    #[tokio::test]
    async fn provision_calendar_user_other_actor_denied() {
        // Caller-scoping guard: a plain User may provision only its OWN
        // calendar — targeting another actor is denied. (Before the guard this
        // was an unguarded cross-actor write — a user could write any actor's
        // calendar metadata.)
        let state = fixture_state().await;
        // No bridge service-user row, no admin row → User class.
        let user_actor = [50u8; 32];

        let req = ProvisionCalendarRequest {
            actor_id: vec![99u8; 32], // a DIFFERENT actor
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_calendar_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── list_calendars tests ──────────────────────────────────────────

    fn make_list_payload(actor_id: &[u8; 32]) -> Bytes {
        use fauna_protocol::bridge_routing::ListCalendarsRequest;
        let req = ListCalendarsRequest {
            actor_id: actor_id.to_vec(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn list_calendars_empty_returns_empty_vec() {
        let state = fixture_state().await;
        let mda = [90u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [91u8; 32];

        let bytes = list_calendars_handler()(state, mda, make_list_payload(&actor))
            .await
            .expect("handler ok");
        let reply: fauna_protocol::bridge_routing::ListCalendarsReply =
            fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            reply.calendars.is_empty(),
            "fresh actor must have no calendars"
        );
    }

    #[tokio::test]
    async fn list_calendars_after_two_provisions_one_put_returns_two_with_correct_counts() {
        use fauna_protocol::bridge_routing::ListCalendarsReply;

        let state = fixture_state().await;
        let mda = [92u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [93u8; 32];
        let cal_1 = [101u8; 32];
        let cal_2 = [102u8; 32];
        let meta_1 = b"sealed-meta-cal-1".to_vec();
        let meta_2 = b"sealed-meta-cal-2".to_vec();
        let now_ts = 1_700_000_000i64;

        // Provision two calendars.
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_1, &meta_1, now_ts)
            .await
            .unwrap();
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_2, &meta_2, now_ts + 1)
            .await
            .unwrap();

        // PUT one event into cal_1 only.
        state
            .db
            .place_caldav_event(
                &actor,
                &cal_1,
                &[50u8; 32],
                b"encrypted-body",
                b"encrypted-hint",
                now_ts + 2,
                14,
                now_ts + 100,
            )
            .await
            .unwrap();

        let bytes = list_calendars_handler()(state, mda, make_list_payload(&actor))
            .await
            .expect("handler ok");
        let reply: ListCalendarsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        assert_eq!(reply.calendars.len(), 2, "two calendars expected");

        // Order is created_at ASC — cal_1 first.
        let e1 = &reply.calendars[0];
        let e2 = &reply.calendars[1];

        assert_eq!(e1.calendar_id, cal_1.to_vec());
        assert_eq!(e1.encrypted_metadata, meta_1);
        // After provision: ctag=0, highestmodseq=1.
        // After PUT: ctag and highestmodseq both bump to 2 (lockstep).
        assert_eq!(e1.ctag, 2, "cal_1 got a PUT → ctag bumps in lockstep to 2");
        assert_eq!(e1.highestmodseq, 2, "cal_1 got a PUT → highestmodseq==2");
        assert_eq!(e1.event_count, 1, "cal_1 has one event");

        assert_eq!(e2.calendar_id, cal_2.to_vec());
        assert_eq!(e2.encrypted_metadata, meta_2);
        assert_eq!(e2.ctag, 0, "cal_2 is provision-only → ctag still 0");
        assert_eq!(
            e2.highestmodseq, 1,
            "cal_2 is provision-only → highestmodseq==1"
        );
        assert_eq!(e2.event_count, 0, "cal_2 has no events");
    }

    #[tokio::test]
    async fn list_calendars_bridge_mta_class_denied() {
        // The MTA role is neither BridgeMda nor User → still denied (only the
        // MDA + the actor's own Fauna app reach the CalDAV r/w RPCs).
        let state = fixture_state().await;
        let mta = [94u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [95u8; 32];

        let err = list_calendars_handler()(state, mta, make_list_payload(&actor))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_calendars_user_other_actor_denied() {
        // Decision B widened this RPC to `User`, but the caller-scope guard
        // still denies a user listing ANOTHER actor's calendars.
        let state = fixture_state().await;
        // No bridge row, no admin row → User class.
        let user_actor = [96u8; 32];
        let other = [200u8; 32];

        let err = list_calendars_handler()(state, user_actor, make_list_payload(&other))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_calendars_admin_other_actor_denied() {
        // admin ⊇ user, but still caller-scoped: an admin may not list another
        // actor's calendars.
        let state = fixture_state().await;
        let admin_actor = [97u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let err = list_calendars_handler()(state, admin_actor, make_list_payload(&other))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_calendars_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [98u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::ListCalendarsRequest;
        let req = ListCalendarsRequest {
            actor_id: vec![1u8; 16], // wrong length
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_calendars_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── query_events tests ────────────────────────────────────────────

    fn make_query_events_payload(
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        since_modseq: Option<i64>,
        after_event_id: Option<serde_bytes::ByteBuf>,
        limit: u32,
    ) -> Bytes {
        use fauna_protocol::bridge_routing::QueryEventsRequest;
        let req = QueryEventsRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            since_modseq,
            after_event_id,
            limit,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// S6.10 gate 1: a nest that is a pure-backup destination for this
    /// actor's `__calendar` set holds opaque sealed chunks only and must
    /// refuse to serve (or mutate) the kind — the mail twin is
    /// `require_local_mail_serving`'s pure-backup arm.
    #[tokio::test]
    async fn query_events_refuses_on_pure_backup_destination() {
        let state = fixture_state().await;
        let mda = [113u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [114u8; 32];
        state
            .db
            .create_folder_with_options(
                "__calendar",
                &target,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let payload = make_query_events_payload(&target, &[115u8; 32], None, None, 0);
        let err = query_events_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.pure_backup_destination");
    }

    #[tokio::test]
    async fn query_events_unknown_calendar_returns_calendar_not_found() {
        let state = fixture_state().await;
        let mda = [110u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [111u8; 32];
        let cal_id = [112u8; 32];

        let payload = make_query_events_payload(&actor, &cal_id, None, None, 0);
        let bytes = query_events_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: fauna_protocol::bridge_routing::QueryEventsReply =
            fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::QueryEventsReply::CalendarNotFound
        );
    }

    #[tokio::test]
    async fn query_events_returns_single_event_after_put() {
        use fauna_protocol::bridge_routing::QueryEventsReply;

        let state = fixture_state().await;
        let mda = [113u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [114u8; 32];
        let cal_id = [115u8; 32];
        let uid_hash = [116u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision calendar.
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place one event. After provision modseq=1; after PUT modseq=2.
        let outcome = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_hash,
                b"encrypted-event-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let (expected_event_id, expected_etag, expected_modseq) = match outcome {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created {
                event_id,
                etag,
                modseq,
            } => (event_id, etag, modseq),
            other => panic!("expected Created, got {other:?}"),
        };

        // `place_caldav_event` writes the ROW only, and the row carries no body:
        // the serve path resolves a body exclusively through the row's stored
        // `record_cid`, so the record has to exist for the query to return anything — append it with
        // the same body+hint the row was seeded from, which is what makes the
        // derived cid match. `carried_at_rest_unchecked` because this test
        // asserts what the query CARRIES, not seal genuineness.
        crate::segments::cal::append_record(
            &state.cal_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"encrypted-event-body".to_vec(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"encrypted-index-hint".to_vec(),
            ),
            &fauna_calendar::segments::CalFloorMetadata {
                calendar_id: cal_id,
                event_id: expected_event_id,
                uid_hash: uid_hash.to_vec(),
                ciphertext_size: 20,
                internal_date: now_ts + 1,
                created_at: now_ts + 100,
                ..Default::default()
            },
        )
        .await
        .expect("append the row's content record");

        let payload = make_query_events_payload(&actor, &cal_id, None, None, 0);
        let bytes = query_events_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: QueryEventsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            QueryEventsReply::Ok {
                events,
                highestmodseq,
                more,
            } => {
                assert_eq!(events.len(), 1, "one event expected");
                assert_eq!(events[0].event_id, expected_event_id.to_vec());
                assert_eq!(events[0].encrypted_body, b"encrypted-event-body".to_vec());
                assert_eq!(events[0].uid_hash, uid_hash.to_vec());
                assert_eq!(events[0].etag, expected_etag);
                assert_eq!(events[0].modseq, expected_modseq);
                assert_eq!(highestmodseq, 2, "highestmodseq must be 2 after one PUT");
                assert!(!more, "more must be false");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_events_since_modseq_filters() {
        use fauna_protocol::bridge_routing::QueryEventsReply;

        let state = fixture_state().await;
        let mda = [120u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [121u8; 32];
        let cal_id = [122u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision calendar (modseq=1 after provisioning).
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place two events. Event 1 → modseq=2, Event 2 → modseq=3.
        state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &[50u8; 32],
                b"encrypted-body-1",
                b"encrypted-hint-1",
                now_ts + 1,
                16,
                now_ts + 100,
            )
            .await
            .unwrap();
        let outcome2 = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &[51u8; 32],
                b"encrypted-body-2",
                b"encrypted-hint-2",
                now_ts + 2,
                16,
                now_ts + 200,
            )
            .await
            .unwrap();
        let expected_event2_id = match outcome2 {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        };

        // Query with since_modseq=2 → only event 2 (modseq=3).
        let payload = make_query_events_payload(&actor, &cal_id, Some(2), None, 0);
        let bytes = query_events_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: QueryEventsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            QueryEventsReply::Ok { events, .. } => {
                assert_eq!(events.len(), 1, "only event with modseq > 2 expected");
                assert_eq!(events[0].event_id, expected_event2_id.to_vec());
                assert_eq!(events[0].modseq, 3);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_events_pagination_with_after_event_id() {
        use fauna_protocol::bridge_routing::QueryEventsReply;

        let state = fixture_state().await;
        let mda = [130u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [131u8; 32];
        let cal_id = [132u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision calendar.
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place 3 events with distinct timestamps and bodies so event_ids differ.
        let mut event_ids = Vec::new();
        for i in 0u8..3 {
            let outcome = state
                .db
                .place_caldav_event(
                    &actor,
                    &cal_id,
                    &[200u8 + i; 32],
                    &[b"encrypted-body-", &[b'a' + i][..], &[0u8; 14][..]].concat(),
                    b"encrypted-hint",
                    now_ts + i as i64, // distinct timestamps
                    16,
                    now_ts + 100,
                )
                .await
                .unwrap();
            match outcome {
                crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => {
                    event_ids.push(event_id);
                }
                other => panic!("expected Created, got {other:?}"),
            }
        }
        // Sort event_ids ascending (DB returns in event_id ASC order).
        event_ids.sort();

        // First page: limit=2 → expect 2 events, more=true.
        let payload = make_query_events_payload(&actor, &cal_id, None, None, 2);
        let bytes = query_events_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok page 1");
        let reply1: QueryEventsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        let last_event_id = match reply1 {
            QueryEventsReply::Ok {
                ref events, more, ..
            } => {
                assert_eq!(events.len(), 2, "page 1 must have 2 events");
                assert!(more, "more must be true after page 1");
                events.last().unwrap().event_id.clone()
            }
            other => panic!("expected Ok, got {other:?}"),
        };

        // Second page: after_event_id = last_event_id, limit=2 → expect 1 event, more=false.
        let payload2 = make_query_events_payload(
            &actor,
            &cal_id,
            None,
            Some(serde_bytes::ByteBuf::from(last_event_id)),
            2,
        );
        let bytes2 = query_events_handler()(state, mda, payload2)
            .await
            .expect("handler ok page 2");
        let reply2: QueryEventsReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            QueryEventsReply::Ok { events, more, .. } => {
                assert_eq!(events.len(), 1, "page 2 must have 1 event");
                assert!(!more, "more must be false on last page");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_events_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [140u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [141u8; 32];
        let cal_id = [142u8; 32];

        let payload = make_query_events_payload(&actor, &cal_id, None, None, 0);
        let err = query_events_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_events_user_other_actor_denied() {
        // Decision B: `User` may query its OWN calendar's events, but the
        // caller-scope guard denies querying ANOTHER actor's events.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [143u8; 32];
        let other = [200u8; 32];
        let cal_id = [144u8; 32];

        let payload = make_query_events_payload(&other, &cal_id, None, None, 0);
        let err = query_events_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_events_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [145u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];
        let cal_id = [146u8; 32];

        let payload = make_query_events_payload(&other, &cal_id, None, None, 0);
        let err = query_events_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_events_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [150u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryEventsRequest;
        let req = QueryEventsRequest {
            actor_id: vec![1u8; 16], // wrong length
            calendar_id: vec![2u8; 32],
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_events_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn query_events_rejects_malformed_calendar_id() {
        let state = fixture_state().await;
        let mda = [151u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryEventsRequest;
        let req = QueryEventsRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 16], // wrong length
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_events_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn query_events_rejects_malformed_after_event_id() {
        let state = fixture_state().await;
        let mda = [152u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryEventsRequest;
        let req = QueryEventsRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            since_modseq: None,
            after_event_id: Some(serde_bytes::ByteBuf::from(vec![1u8; 16])), // wrong length
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_events_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── put_event_ciphertext tests ────────────────────────────────────

    /// Provision a calendar; helper reused across multiple tests.
    async fn provision_one(
        state: &Arc<AppState>,
        mda: &[u8; 32],
        actor: &[u8; 32],
        cal_id: &[u8; 32],
    ) {
        use fauna_protocol::bridge_routing::ProvisionCalendarRequest;
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        provision_calendar_handler()(state.clone(), *mda, payload)
            .await
            .expect("provision_one: handler ok");
    }

    /// Seal `plaintext` as a genuine recipient envelope — the shape every
    /// production caller produces (Go MDA `EncryptToRecipientHybrid`,
    /// client `seal_event_body`; both emit a canonical `MailRecordEnvelope`).
    /// S6.12 makes the PUT handler refuse anything else, so test bodies and
    /// hints must be real seals, not byte literals. NOT deterministic (HPKE
    /// encapsulation is randomized) — seal once and reuse the returned bytes
    /// when a test needs the same body twice (idempotency/retry paths).
    fn sealed(plaintext: &[u8]) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};
        let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
        seal_to_recipient(plaintext, &pubkey)
            .expect("seal test fixture")
            .to_canonical_bytes()
            .expect("canonical test fixture")
    }

    fn make_put_payload(
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
        encrypted_body: &[u8],
        encrypted_index_hint: &[u8],
        timestamp: i64,
        if_match: Option<String>,
    ) -> Bytes {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;
        let req = PutEventCiphertextRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body: encrypted_body.to_vec(),
            encrypted_index_hint: encrypted_index_hint.to_vec(),
            timestamp,
            ciphertext_size: encrypted_body.len() as u32,
            if_match,
            // MUA-style write (no Fauna sidecar); the sidecar round-trip is
            // covered by the crate + protocol tests + the DB preserve test.
            ..Default::default()
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    fn invite_payload(actor: &[u8; 32], uid_hash: &[u8; 32], sender: &str) -> Bytes {
        let req = PlaceInboundInviteRequest {
            actor_id: actor.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body: sealed(b"BEGIN:VCALENDAR invite"),
            encrypted_index_hint: sealed(b"hint"),
            timestamp: 1_700_000_000,
            sender_address: sender.to_string(),
            extra: Default::default(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// `caldav-server.md` § Server-side auto-schedule, "Inbound invite": the
    /// MTA's placement of an emailed invitation provisions the lazy Personal
    /// calendar for a user who has none, lands the event there, and never
    /// touches an event already carrying that UID — create-only.
    #[tokio::test]
    async fn an_emailed_invitation_lands_on_personal_once_and_never_overwrites() {
        let state = fixture_state().await;
        let mta = [150u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [151u8; 32];
        let uid_hash = [152u8; 32];
        let personal = fauna_protocol::dav_identity::personal_calendar_id();
        // The Personal calendar's metadata is sealed to the recipient's key.
        crate::test_support::seed_recipient_seal_key(&state.db, &actor, &[0x5Eu8; 32]).await;

        let place = |payload| place_inbound_invite_handler()(state.clone(), mta, payload);
        let first: PlaceInboundInviteReply = fauna_cbor::decode_strict(
            &place(invite_payload(&actor, &uid_hash, "org@example.com"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first, PlaceInboundInviteReply::Placed);
        let calendars = state.db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(
            calendars.iter().map(|c| c.calendar_id).collect::<Vec<_>>(),
            vec![personal],
            "the invitation must provision exactly the lazy Personal calendar"
        );
        let hms_after_first = state
            .db
            .caldav_calendar_highestmodseq(&actor, &personal)
            .await
            .unwrap();

        let again: PlaceInboundInviteReply = fauna_cbor::decode_strict(
            &place(invite_payload(&actor, &uid_hash, "org@example.com"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(again, PlaceInboundInviteReply::AlreadyOnCalendar);
        assert_eq!(
            state
                .db
                .caldav_calendar_highestmodseq(&actor, &personal)
                .await
                .unwrap(),
            hms_after_first,
            "a second invitation for the same UID must leave the calendar untouched"
        );
    }

    /// The invitation goes where its mail went (`family-safety.md` § The mail
    /// gate): a ward whose guardian holds unknown senders gets the invitation
    /// withheld, not placed.
    #[tokio::test]
    async fn an_emailed_invitation_waits_behind_the_guardian_mail_gate() {
        let state = fixture_state().await;
        let mta = [153u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let guardian = [154u8; 32];
        let ward = [155u8; 32];
        state
            .db
            .create_user_with_handle(&guardian, "personal", "parent", None)
            .await
            .unwrap();
        state
            .db
            .create_user_with_handle(&ward, "personal", "kid", Some(&guardian[..]))
            .await
            .unwrap();
        state
            .db
            .update_guardian_policy(
                &ward[..],
                false,
                "hold",
                true,
                "allow",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let reply: PlaceInboundInviteReply = fauna_cbor::decode_strict(
            &place_inbound_invite_handler()(
                state.clone(),
                mta,
                invite_payload(&ward, &[156u8; 32], "stranger@example.com"),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reply, PlaceInboundInviteReply::Withheld);
        assert!(
            state
                .db
                .list_bridge_caldav_calendars(&ward)
                .await
                .unwrap()
                .is_empty(),
            "a withheld invitation must not even provision a calendar"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_unknown_calendar_returns_calendar_not_found() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [160u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [161u8; 32];
        let cal_id = [162u8; 32];

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &[170u8; 32],
            &sealed(b"encrypted-body"),
            &sealed(b"encrypted-hint"),
            1_700_000_000,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, PutEventCiphertextReply::CalendarNotFound);
    }

    /// `caldav-server.md` § QUOTA — shared with IMAP: an event's bytes draw on
    /// the SAME storage number mail does. Read through `imap_quota_usage`, the
    /// one usage source both GETQUOTA reporting and APPEND/COPY/MOVE/inbound
    /// enforcement share — so this pins both. The expected figure is the
    /// record's own CARv2 block length (the size the mail half uses), and a
    /// body replacement counts the new record only: the row names one record.
    #[tokio::test]
    async fn an_events_bytes_count_toward_the_shared_storage_quota() {
        let state = fixture_state().await;
        let mda = [240u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [241u8; 32];
        let cal_id = [242u8; 32];
        let uid_hash = [243u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;

        let usage = |state: Arc<AppState>| async move {
            crate::bridge_imap_handlers::imap_quota_usage(&state, &actor)
                .await
                .expect("quota usage")
        };
        let record_size = |state: Arc<AppState>| async move {
            let page = state
                .db
                .query_caldav_events(&actor, &cal_id, None, None, 0)
                .await
                .unwrap();
            assert_eq!(page.events.len(), 1, "one uid, one row");
            let cid = page.events[0]
                .record_cid()
                .unwrap()
                .expect("row names its record");
            let sr = state
                .db
                .segment_records_lookup_record(&actor, crate::segments::cal::KIND, &cid)
                .await
                .unwrap()
                .expect("mirror row");
            crate::segments::record_sizes(&state.cal_segments, &actor, &[(sr.segment_id, cid)])
                .await
                .unwrap()[0]
                .expect("real segment size")
        };
        assert_eq!(
            usage(state.clone()).await,
            (0, 0),
            "a fresh calendar holds nothing"
        );

        let put = |body: Vec<u8>| {
            let state = state.clone();
            async move {
                let payload = make_put_payload(
                    &actor,
                    &cal_id,
                    &uid_hash,
                    &body,
                    &sealed(b"hint"),
                    1_700_000_000,
                    None,
                );
                put_event_ciphertext_handler()(state, mda, payload)
                    .await
                    .expect("put ok");
            }
        };
        put(sealed(&[b'e'; 400])).await;
        let first = record_size(state.clone()).await;
        assert!(
            first > 400,
            "the record carries at least its sealed body: {first}"
        );
        assert_eq!(
            usage(state.clone()).await,
            (first, 0),
            "the event's record counts toward STORAGE, never toward MESSAGE"
        );

        put(sealed(&[b'f'; 1200])).await;
        let second = record_size(state.clone()).await;
        assert!(second > first);
        assert_eq!(
            usage(state.clone()).await,
            (second, 0),
            "a replaced body counts once — the record the row now names"
        );
    }

    /// `caldav-server.md` § QUOTA — shared with IMAP → § Enforcement points: a
    /// PUT that would take the account past its storage ceiling is refused
    /// with the typed `fauna.bridges.over_quota` BEFORE the segment append, so
    /// the refusal writes nothing — no row, no record, no usage change.
    #[tokio::test]
    async fn an_event_put_past_the_storage_ceiling_is_refused_and_appends_nothing() {
        let state = fixture_state().await;
        let mda = [244u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [245u8; 32];
        let cal_id = [246u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;
        let put = |uid: u8, body: Vec<u8>| {
            let payload = make_put_payload(
                &actor,
                &cal_id,
                &[uid; 32],
                &body,
                &sealed(b"hint"),
                1_700_000_000,
                None,
            );
            put_event_ciphertext_handler()(state.clone(), mda, payload)
        };
        put(1, sealed(&[b'a'; 100]))
            .await
            .expect("first event fits");
        let (used, _) = crate::bridge_imap_handlers::imap_quota_usage(&state, &actor)
            .await
            .unwrap();
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(used + 5),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = put(2, sealed(&[b'b'; 400]))
            .await
            .expect_err("an event past the ceiling must be refused");
        assert_eq!(err.code, "fauna.bridges.over_quota");
        assert_eq!(
            state
                .db
                .segment_records_count_live(&actor, crate::segments::cal::KIND)
                .await
                .unwrap(),
            1,
            "the refused body must never reach the segment"
        );
        assert_eq!(
            state
                .db
                .query_caldav_events(&actor, &cal_id, None, None, 0)
                .await
                .unwrap()
                .events
                .len(),
            1,
            "the refused event must never get a row"
        );
        assert_eq!(
            crate::bridge_imap_handlers::imap_quota_usage(&state, &actor)
                .await
                .unwrap()
                .0,
            used
        );
    }

    /// § Enforcement points: an update is checked on its size delta, and a
    /// shrinking (or same-size) write always passes — even for an account an
    /// admin has already put over its ceiling, the one state where a zero
    /// delta fed to the ceiling check would wrongly refuse. A growing update
    /// past the ceiling is refused like a new event.
    #[tokio::test]
    async fn a_shrinking_event_update_passes_even_over_the_ceiling() {
        let state = fixture_state().await;
        let mda = [247u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [248u8; 32];
        let cal_id = [249u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;
        let put = |body: Vec<u8>| {
            let payload = make_put_payload(
                &actor,
                &cal_id,
                &[250u8; 32],
                &body,
                &sealed(b"hint"),
                1_700_000_000,
                None,
            );
            put_event_ciphertext_handler()(state.clone(), mda, payload)
        };
        put(sealed(&[b'a'; 1200])).await.expect("the event fits");
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        put(sealed(&[b'b'; 1200]))
            .await
            .expect("a same-size update is never refused");
        put(sealed(&[b'c'; 400]))
            .await
            .expect("a shrinking update is never refused");
        let err = put(sealed(&[b'd'; 800]))
            .await
            .expect_err("a growing update past the ceiling is refused");
        assert_eq!(err.code, "fauna.bridges.over_quota");
    }

    /// An emailed invitation for an account with no room is not placed — the
    /// mail copy is already delivered, so it is a typed placement outcome the
    /// MTA logs, never an RPC error that could read as a failed delivery
    /// (`caldav-server.md` § QUOTA → § Enforcement points).
    #[tokio::test]
    async fn an_emailed_invitation_past_the_ceiling_is_not_placed() {
        let state = fixture_state().await;
        let mta = [251u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [252u8; 32];
        crate::test_support::seed_recipient_seal_key(&state.db, &actor, &[0x5Eu8; 32]).await;
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        let reply: PlaceInboundInviteReply = fauna_cbor::decode_strict(
            &place_inbound_invite_handler()(
                state.clone(),
                mta,
                invite_payload(&actor, &[253u8; 32], "org@example.com"),
            )
            .await
            .expect("an over-quota invitation is an outcome, not an error"),
        )
        .unwrap();
        assert_eq!(reply, PlaceInboundInviteReply::OverQuota);
        assert_eq!(
            state
                .db
                .segment_records_count_live(&actor, crate::segments::cal::KIND)
                .await
                .unwrap(),
            0,
            "nothing of the invitation may rest on the calendar"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_new_event_returns_created() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [163u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [164u8; 32];
        let cal_id = [165u8; 32];
        let uid_hash = [166u8; 32];
        // event_id derives from the body bytes, so bind the seal once.
        let encrypted_body = sealed(b"encrypted-icalendar-v1");
        let encrypted_hint = sealed(b"encrypted-hint-v1");
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Expected event_id, via the same derivation the DB layer itself uses.
        let expected_event_id: Vec<u8> =
            derive_caldav_event_id(&actor, timestamp, &encrypted_body).to_vec();

        match reply {
            PutEventCiphertextReply::Created {
                event_id,
                etag,
                modseq,
            } => {
                assert_eq!(
                    event_id, expected_event_id,
                    "event_id must match blake3 derivation"
                );
                assert_eq!(modseq, 2, "modseq must be 2 after provision(1) + PUT(2)");
                assert_eq!(
                    etag,
                    format!("{:016x}", 2),
                    "etag must be hex-formatted modseq"
                );
            }
            other => panic!("expected Created, got {other:?}"),
        }

        // Verify the row exists in the DB.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "one event row must exist after Created"
        );
        assert_eq!(page.events[0].event_id.to_vec(), expected_event_id);
    }

    #[tokio::test]
    async fn put_event_ciphertext_update_same_uid_hash_returns_updated_with_tombstone() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [167u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [168u8; 32];
        let cal_id = [169u8; 32];
        let uid_hash = [171u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // PUT first body.
        let payload1 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_event_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .expect("handler ok 1");
        let reply1: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let old_event_id = match reply1 {
            PutEventCiphertextReply::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second different body to same uid_hash.
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            None,
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .expect("handler ok 2");
        let reply2: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutEventCiphertextReply::Updated {
                event_id,
                etag,
                modseq,
            } => {
                assert_ne!(
                    event_id, old_event_id,
                    "new event_id must differ (different body)"
                );
                assert_eq!(
                    modseq, 3,
                    "modseq must be 3 after provision(1)+PUT(2)+UPDATE(3)"
                );
                assert_eq!(etag, format!("{:016x}", 3));
            }
            other => panic!("expected Updated, got {other:?}"),
        }

        // Tombstone for old_event_id must exist.
        let expunged = state
            .db
            .query_caldav_expunged_since(&actor, &cal_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 1, "one tombstone must exist after update");
        assert_eq!(
            expunged[0].event_id.to_vec(),
            old_event_id,
            "tombstone must be for the old event_id"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_if_match_matching_returns_updated() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [172u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [173u8; 32];
        let cal_id = [174u8; 32];
        let uid_hash = [175u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // PUT first event, capture etag.
        let payload1 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_event_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let first_etag = match reply1 {
            PutEventCiphertextReply::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second event with matching if_match.
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            Some(first_etag),
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        assert!(
            matches!(reply2, PutEventCiphertextReply::Updated { .. }),
            "matching if_match must return Updated, got {reply2:?}"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_if_match_mismatch_returns_precondition_failed() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [176u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [177u8; 32];
        let cal_id = [178u8; 32];
        let uid_hash = [179u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // PUT first event, capture its etag.
        let payload1 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_event_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let first_etag = match reply1 {
            PutEventCiphertextReply::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second event with wrong if_match.
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            Some("0000000000000000".to_string()), // wrong etag
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutEventCiphertextReply::PreconditionFailed { current_etag } => {
                assert_eq!(
                    current_etag, first_etag,
                    "current_etag must be the first event's etag"
                );
            }
            other => panic!("expected PreconditionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_event_ciphertext_transport_retry_same_body_returns_updated_no_bump() {
        use fauna_protocol::bridge_routing::PutEventCiphertextReply;

        let state = fixture_state().await;
        let mda = [180u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [181u8; 32];
        let cal_id = [182u8; 32];
        let uid_hash = [183u8; 32];
        // Idempotent retry: the two PUTs must carry the *same* sealed bytes
        // (seals are randomized, and event_id derives from the body), so bind
        // once and reuse.
        let encrypted_body = sealed(b"encrypted-body-retry");
        let encrypted_hint = sealed(b"encrypted-hint-retry");
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // First PUT.
        let payload1 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes1 = put_event_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let (event_id_a, modseq_a) = match reply1 {
            PutEventCiphertextReply::Created {
                event_id, modseq, ..
            } => (event_id, modseq),
            other => panic!("expected Created, got {other:?}"),
        };

        // Retry with identical bytes — same uid_hash, same body, same timestamp.
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutEventCiphertextReply::Updated {
                event_id,
                modseq,
                etag,
            } => {
                assert_eq!(event_id, event_id_a, "retry must return the same event_id");
                assert_eq!(modseq, modseq_a, "modseq must not bump on idempotent retry");
                assert_eq!(etag, format!("{:016x}", modseq_a));
            }
            other => panic!("expected Updated (idempotent retry), got {other:?}"),
        }

        // Exactly one event row.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "must have exactly one event row after retry"
        );

        // No tombstones.
        let expunged = state
            .db
            .query_caldav_expunged_since(&actor, &cal_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 0, "no tombstone on idempotent retry");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_size_mismatch() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [184u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"ten-bytes!".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 999, // wrong
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_empty_body() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [185u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: vec![],
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 0,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_empty_index_hint() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [186u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: vec![],
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_an_unsealed_body() {
        // S6.12: the at-rest seal is structural, not a caller convention. The
        // RPC is allowlisted `BridgeMda | User`, so a User-class client can
        // hand the nest arbitrary bytes — and since S6.6 those bytes would go
        // straight into the backup-eligible `__calendar` segment store. A body
        // that is not a sealed recipient envelope is refused at the wire edge,
        // BEFORE the segment append (so the rejection also leaks no orphan
        // content record).
        let state = fixture_state().await;
        let mda = [200u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [201u8; 32];
        let cal_id = [202u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;

        let raw_body =
            b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &[203u8; 32],
            raw_body,
            &sealed(b"hint-tokens"),
            1_700_000_000,
            None,
        );
        let err = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // The rejection happened before the segment append. Since the
        // record-identity cutover a record's id IS the hash of its envelope
        // bytes, so there is no id to look up for a record that was never
        // built — the assertion is now the stronger one it always meant: this
        // actor's calendar mirror holds no content record at all.
        let records: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM segment_records WHERE scope_id = ?1 AND kind = ?2",
                rusqlite::params![actor.as_slice(), crate::segments::cal::KIND],
                |r| r.get(0),
            )
            .expect("count segment_records")
        };
        assert_eq!(
            records, 0,
            "a rejected PUT must not leave an orphan content record"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_an_unsealed_index_hint() {
        // Twin of the body gate: the index hint is content-derived (the
        // tokenized word set), rests in the same envelope, and every
        // production caller seals it — raw token bytes at rest would leak the
        // body's word set. Same wire-edge refusal.
        let state = fixture_state().await;
        let mda = [204u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [205u8; 32];
        let cal_id = [206u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &[207u8; 32],
            &sealed(b"BEGIN:VCALENDAR..."),
            b"raw-token-set-bytes",
            1_700_000_000,
            None,
        );
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_wrong_length_uid_hash() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [187u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 16], // wrong length
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_wrong_length_actor_id() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [188u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 16], // wrong length
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_rejects_wrong_length_calendar_id() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [189u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 16], // wrong length
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_event_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_event_ciphertext_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [190u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_put_payload(
            &[1u8; 32],
            &[2u8; 32],
            &[3u8; 32],
            b"some-body",
            b"hint",
            1_700_000_000,
            None,
        );
        let err = put_event_ciphertext_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn put_event_ciphertext_user_other_actor_denied() {
        // Decision B: `User` may write an event to its OWN calendar (the
        // client seals locally), but the caller-scope guard denies writing
        // under ANOTHER actor's id — without it a user could forge events into
        // a foreign calendar.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [191u8; 32];
        let other = [200u8; 32];

        let payload = make_put_payload(
            &other,
            &[2u8; 32],
            &[3u8; 32],
            b"some-body",
            b"hint",
            1_700_000_000,
            None,
        );
        let err = put_event_ciphertext_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── delete_event tests ────────────────────────────────────────────

    fn make_delete_payload(
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<String>,
    ) -> Bytes {
        let req = DeleteEventRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            if_match,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn delete_event_missing_calendar_returns_not_found() {
        let state = fixture_state().await;
        let mda = [200u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [201u8; 32];
        let cal_id = [202u8; 32];
        let uid_hash = [203u8; 32];

        // No calendar provisioned — expect NotFound.
        let payload = make_delete_payload(&actor, &cal_id, &uid_hash, None);
        let bytes = delete_event_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteEventReply::NotFound);
    }

    #[tokio::test]
    async fn delete_event_missing_event_returns_not_found() {
        let state = fixture_state().await;
        let mda = [204u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [205u8; 32];
        let cal_id = [206u8; 32];
        let uid_hash = [207u8; 32];

        // Provision the calendar but place no events.
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_delete_payload(&actor, &cal_id, &uid_hash, None);
        let bytes = delete_event_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteEventReply::NotFound);
    }

    #[tokio::test]
    async fn delete_event_success_returns_deleted_with_tombstone() {
        let state = fixture_state().await;
        let mda = [208u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [209u8; 32];
        let cal_id = [210u8; 32];
        let uid_hash = [211u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq baseline = 1).
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Place one event (modseq = 2 after placement).
        let place_outcome = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_hash,
                b"encrypted-event-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let (placed_event_id, placed_etag) = match place_outcome {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created {
                event_id,
                etag,
                modseq,
            } => {
                assert_eq!(modseq, 2, "modseq after place must be 2");
                (event_id, etag)
            }
            other => panic!("expected Created, got {other:?}"),
        };
        let _ = placed_etag; // not needed for this test

        // Delete the event (modseq = 3 after delete).
        let payload = make_delete_payload(&actor, &cal_id, &uid_hash, None);
        let bytes = delete_event_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            DeleteEventReply::Deleted { event_id, modseq } => {
                assert_eq!(
                    event_id,
                    placed_event_id.to_vec(),
                    "event_id must match placed row"
                );
                assert_eq!(
                    modseq, 3,
                    "modseq must be 3 after provision(1)+place(2)+delete(3)"
                );
            }
            other => panic!("expected Deleted, got {other:?}"),
        }

        // Event row must be gone.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 0)
            .await
            .unwrap();
        assert!(
            page.events.is_empty(),
            "event row must be removed after delete"
        );

        // Tombstone must exist with correct fields.
        let expunged = state
            .db
            .query_caldav_expunged_since(&actor, &cal_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 1, "one tombstone expected");
        assert_eq!(
            expunged[0].event_id.to_vec(),
            placed_event_id.to_vec(),
            "tombstone event_id must match deleted row"
        );
        assert_eq!(
            expunged[0].uid_hash,
            uid_hash.to_vec(),
            "tombstone uid_hash must match"
        );
        assert_eq!(expunged[0].modseq, 3, "tombstone modseq must be 3");
    }

    #[tokio::test]
    async fn delete_event_if_match_mismatch_returns_precondition_failed() {
        let state = fixture_state().await;
        let mda = [212u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [213u8; 32];
        let cal_id = [214u8; 32];
        let uid_hash = [215u8; 32];
        let now_ts = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // Place event; etag = format!("{:016x}", 2) = "0000000000000002".
        let place_outcome = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_hash,
                b"encrypted-event-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let placed_etag = match place_outcome {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };
        assert_eq!(placed_etag, "0000000000000002");

        // Delete with wrong if_match — should return PreconditionFailed.
        let payload = make_delete_payload(
            &actor,
            &cal_id,
            &uid_hash,
            Some("0000000000000000".to_string()),
        );
        let bytes = delete_event_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            DeleteEventReply::PreconditionFailed { current_etag } => {
                assert_eq!(
                    current_etag, placed_etag,
                    "current_etag must be the placed event's etag"
                );
            }
            other => panic!("expected PreconditionFailed, got {other:?}"),
        }

        // Event row must still exist.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "event row must survive if_match mismatch"
        );
    }

    #[tokio::test]
    async fn delete_event_if_match_matching_returns_deleted() {
        let state = fixture_state().await;
        let mda = [216u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [217u8; 32];
        let cal_id = [218u8; 32];
        let uid_hash = [219u8; 32];
        let now_ts = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        let place_outcome = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_hash,
                b"encrypted-event-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let placed_etag = match place_outcome {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // Delete with matching if_match — should return Deleted.
        let payload = make_delete_payload(&actor, &cal_id, &uid_hash, Some(placed_etag));
        let bytes = delete_event_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();

        assert!(
            matches!(reply, DeleteEventReply::Deleted { .. }),
            "matching if_match must return Deleted, got {reply:?}"
        );

        // Event row must be gone.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 0)
            .await
            .unwrap();
        assert!(
            page.events.is_empty(),
            "event row must be removed after delete with matching if_match"
        );
    }

    #[tokio::test]
    async fn delete_event_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [220u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_delete_payload(&[1u8; 32], &[2u8; 32], &[3u8; 32], None);
        let err = delete_event_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_event_user_other_actor_denied() {
        // Decision B: `User` may delete events from its OWN calendar, but the
        // caller-scope guard denies deleting from ANOTHER actor's calendar.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [221u8; 32];
        let other = [200u8; 32];

        let payload = make_delete_payload(&other, &[2u8; 32], &[3u8; 32], None);
        let err = delete_event_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_event_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [222u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let payload = make_delete_payload(&other, &[2u8; 32], &[3u8; 32], None);
        let err = delete_event_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_event_rejects_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [223u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteEventRequest {
            actor_id: vec![1u8; 16], // wrong length
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_event_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn delete_event_rejects_wrong_length_calendar_id() {
        let state = fixture_state().await;
        let mda = [224u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteEventRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 16], // wrong length
            uid_hash: vec![3u8; 32],
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_event_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn delete_event_rejects_wrong_length_uid_hash() {
        let state = fixture_state().await;
        let mda = [225u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteEventRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 16], // wrong length
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_event_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── sync_calendar_since tests ─────────────────────────────────────

    fn make_sync_payload(
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        sync_token: &str,
        limit: u32,
    ) -> Bytes {
        let req = SyncCalendarSinceRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            sync_token: sync_token.to_string(),
            limit,
            ..Default::default()
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn sync_calendar_since_unknown_calendar_returns_calendar_not_found() {
        let state = fixture_state().await;
        let mda = [230u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [231u8; 32];
        let cal_id = [232u8; 32];

        // No calendar provisioned — expect CalendarNotFound.
        let payload = make_sync_payload(&actor, &cal_id, "0", 0);
        let bytes = sync_calendar_since_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, SyncCalendarSinceReply::CalendarNotFound);
    }

    #[tokio::test]
    async fn sync_calendar_since_fresh_calendar_returns_all_changed_no_expunged() {
        let state = fixture_state().await;
        let mda = [233u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [234u8; 32];
        let cal_id = [235u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision calendar (modseq baseline = 1).
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Place 3 events (modseqs 2, 3, 4; highestmodseq = 4).
        let uid_a = [240u8; 32];
        let uid_b = [241u8; 32];
        let uid_c = [242u8; 32];
        let place_a = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_a,
                b"body-a",
                b"hint-a",
                now_ts + 1,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();
        let place_b = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_b,
                b"body-b",
                b"hint-b",
                now_ts + 2,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();
        let place_c = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_c,
                b"body-c",
                b"hint-c",
                now_ts + 3,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();

        let id_a = match place_a {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };
        let id_b = match place_b {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };
        let id_c = match place_c {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };

        // Full sync from "0", unbounded.
        let payload = make_sync_payload(&actor, &cal_id, "0", 0);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncCalendarSinceReply::Ok {
                changed,
                expunged,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(changed.len(), 3, "full sync must return all 3 events");
                assert!(expunged.is_empty(), "no tombstones on a fresh calendar");
                assert_eq!(
                    new_sync_token, "4",
                    "provision(1)+3 places = highestmodseq 4"
                );
                assert!(!more, "unbounded limit: more must be false");
                // Verify event_ids match.
                let returned_ids: Vec<Vec<u8>> =
                    changed.iter().map(|e| e.event_id.clone()).collect();
                assert!(
                    returned_ids.contains(&id_a.to_vec()),
                    "event A must be present"
                );
                assert!(
                    returned_ids.contains(&id_b.to_vec()),
                    "event B must be present"
                );
                assert!(
                    returned_ids.contains(&id_c.to_vec()),
                    "event C must be present"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_calendar_since_after_put_and_delete_returns_both_signals() {
        let state = fixture_state().await;
        let mda = [243u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [244u8; 32];
        let cal_id = [245u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq 1).
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Place 2 events (modseqs 2 + 3).
        let uid_keep = [250u8; 32];
        let uid_del = [251u8; 32];
        let place_keep = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_keep,
                b"keep-body",
                b"keep-hint",
                now_ts + 1,
                9,
                now_ts + 100,
            )
            .await
            .unwrap();
        let place_del = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_del,
                b"del-body",
                b"del-hint",
                now_ts + 2,
                8,
                now_ts + 100,
            )
            .await
            .unwrap();
        let id_del = match place_del {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };
        let _ = place_keep;

        // Capture the sync_token after the two placements (highestmodseq = 3).
        let payload = make_sync_payload(&actor, &cal_id, "0", 0);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let first_reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let baseline_token = match first_reply {
            SyncCalendarSinceReply::Ok { new_sync_token, .. } => new_sync_token,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(
            baseline_token, "3",
            "after provision(1)+2 places: highestmodseq=3"
        );

        // Place 1 new event (modseq 4).
        let uid_new = [252u8; 32];
        let place_new = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_new,
                b"new-body",
                b"new-hint",
                now_ts + 3,
                8,
                now_ts + 100,
            )
            .await
            .unwrap();
        let id_new = match place_new {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };

        // Delete one existing event (modseq 5). Use a wall-clock-recent
        // expunged_at so the tombstone stays within the 30-day retention
        // window the handler checks against `now` — the fixed `now_ts`
        // (2023) base would otherwise read as past-retention and trip the
        // Ok { stale: true } signal, masking this test's both-signals intent.
        let del_now = now_epoch_secs();
        state
            .db
            .delete_caldav_event_by_uid(&actor, &cal_id, &uid_del, None, del_now)
            .await
            .unwrap();

        // Incremental sync from baseline_token (= "3").
        let payload = make_sync_payload(&actor, &cal_id, &baseline_token, 0);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncCalendarSinceReply::Ok {
                changed,
                expunged,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(
                    changed.len(),
                    1,
                    "only the new event (modseq 4) should appear"
                );
                assert_eq!(
                    expunged.len(),
                    1,
                    "deleted event should appear as tombstone"
                );
                assert_eq!(
                    new_sync_token, "5",
                    "provision(1)+2 places+1 new+1 delete = highestmodseq 5"
                );
                assert!(!more, "unbounded limit: more must be false");

                assert_eq!(
                    changed[0].event_id,
                    id_new.to_vec(),
                    "changed entry must be the newly placed event"
                );
                assert_eq!(
                    expunged[0].event_id,
                    id_del.to_vec(),
                    "expunged entry event_id must match deleted event"
                );
                assert_eq!(
                    expunged[0].uid_hash,
                    uid_del.to_vec(),
                    "expunged entry uid_hash must match deleted event's uid"
                );
                assert_eq!(expunged[0].modseq, 5, "tombstone modseq must be 5");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_calendar_since_rejects_malformed_sync_token_not_numeric() {
        let state = fixture_state().await;
        let mda = [253u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "abc", 0);
        let err = sync_calendar_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_calendar_since_rejects_malformed_sync_token_negative() {
        let state = fixture_state().await;
        let mda = [254u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "-1", 0);
        let err = sync_calendar_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_calendar_since_rejects_malformed_sync_token_empty() {
        let state = fixture_state().await;
        // Use a new byte value — 255 is u8::MAX, valid.
        let mda = [255u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "", 0);
        let err = sync_calendar_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_calendar_since_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [60u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "0", 0);
        let err = sync_calendar_since_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_calendar_since_user_other_actor_denied() {
        // Decision B: `User` may sync its OWN calendar, but the caller-scope
        // guard denies syncing ANOTHER actor's calendar.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [61u8; 32];
        let other = [200u8; 32];

        let payload = make_sync_payload(&other, &[2u8; 32], "0", 0);
        let err = sync_calendar_since_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_calendar_since_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [62u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let payload = make_sync_payload(&other, &[2u8; 32], "0", 0);
        let err = sync_calendar_since_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_calendar_since_rejects_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [63u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SyncCalendarSinceRequest {
            actor_id: vec![1u8; 16], // wrong length
            calendar_id: vec![2u8; 32],
            sync_token: "0".to_string(),
            limit: 0,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = sync_calendar_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_calendar_since_rejects_wrong_length_calendar_id() {
        let state = fixture_state().await;
        let mda = [64u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SyncCalendarSinceRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 16], // wrong length
            sync_token: "0".to_string(),
            limit: 0,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = sync_calendar_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_calendar_since_paginates_correctly_when_more_true_resumes_from_last_returned_modseq()
     {
        // Guards against the D.6 partial-page bug: with the pre-fix code,
        // new_sync_token = hms (calendar-wide highestmodseq) regardless of
        // `more`. A second paged call passing that token would use
        // `modseq > hms` as the filter, which skips every event in the
        // [last_returned_modseq+1..hms] window — i.e. event #3 gets silently
        // dropped. This test confirms the second call correctly returns it.
        //
        // Also confirms events are returned in modseq ASC order (not
        // event_id ASC), so the correct event is deferred to page 2.
        let state = fixture_state().await;
        let mda = [65u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [66u8; 32];
        let cal_id = [67u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq 1), then place 3 events: modseqs 2, 3, 4.
        provision_one(&state, &mda, &actor, &cal_id).await;

        let uid_a = [70u8; 32];
        let uid_b = [71u8; 32];
        let uid_c = [72u8; 32];
        state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_a,
                b"body-a",
                b"hint-a",
                now_ts + 1,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();
        state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_b,
                b"body-b",
                b"hint-b",
                now_ts + 2,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();
        let place_c = state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid_c,
                b"body-c",
                b"hint-c",
                now_ts + 3,
                6,
                now_ts + 100,
            )
            .await
            .unwrap();
        let id_c = match place_c {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("unexpected {other:?}"),
        };

        // highestmodseq is now 4.

        // ── Page 1: sync from "0" with limit=2 ──────────────────────────────
        let payload = make_sync_payload(&actor, &cal_id, "0", 2);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok - page 1");
        let reply1: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        let (changed1, token1, more1) = match reply1 {
            SyncCalendarSinceReply::Ok {
                changed,
                new_sync_token,
                more,
                ..
            } => (changed, new_sync_token, more),
            other => panic!("expected Ok on page 1, got {other:?}"),
        };

        assert_eq!(changed1.len(), 2, "page 1 must return exactly 2 events");
        assert!(more1, "page 1 must signal more == true");
        // new_sync_token must be the modseq of the LAST returned event (3),
        // NOT the calendar-wide highestmodseq (4). With the pre-fix code this
        // would be "4" and the second call would return empty.
        assert_eq!(
            token1, "3",
            "when more==true, new_sync_token must be modseq of last returned event (3), not hms (4)"
        );

        // ── Page 2: resume from the returned token ───────────────────────────
        let payload = make_sync_payload(&actor, &cal_id, &token1, 2);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok - page 2");
        let reply2: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply2 {
            SyncCalendarSinceReply::Ok {
                changed,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(
                    changed.len(),
                    1,
                    "page 2 must return the remaining 1 event (modseq 4)"
                );
                assert!(!more, "page 2 must signal more == false");
                assert_eq!(
                    new_sync_token, "4",
                    "when more==false, new_sync_token must be calendar-wide hms (4)"
                );
                assert_eq!(
                    changed[0].event_id,
                    id_c.to_vec(),
                    "the deferred event must be event C (modseq 4, the largest modseq)"
                );
            }
            other => panic!("expected Ok on page 2, got {other:?}"),
        }
    }

    // ── T10 placement-journal wiring tests ─────────────────────────────────
    //
    // The three CalDAV state-changing handlers — `provision_calendar`,
    // `put_event_ciphertext`, `delete_event` — must append the matching
    // `CalPlacementRecord` to `state.cal_placement` *after* their SQLite
    // mutation commits. Spec § D2 (record shapes), § D3 (manifest), § D6
    // (ε) (atomic-with-SQL invariant; see wiring-site comments for the
    // deferred commit-then-append crash window). Unlike T9.5's
    // unconditional-emit pattern, T10's DB layer signals mutation vs no-op
    // explicitly via the outcome enums, so emission gates on the mutating
    // variants only (Created/Updated/Deleted) — mirroring T7 (StoreFlags),
    // T8 (Move/Copy), T9 (Create/Delete/Rename). Actor IDs `[33u8..38u8;
    // 32]` to stay clear of every previously claimed range.

    #[tokio::test]
    async fn provision_calendar_created_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [33u8; 32];
        let cal_id = [101u8; 32];
        let meta = b"sealed-calendar-metadata-v1".to_vec();

        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: meta.clone(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.calendars.len(),
            1,
            "ProvisionCalendar record must land in the manifest's calendars Vec",
        );
        assert_eq!(manifest.calendars[0].calendar_id, cal_id);
        assert_eq!(manifest.calendars[0].encrypted_metadata, meta);
    }

    #[tokio::test]
    async fn provision_calendar_already_exists_appends_nothing() {
        // Idempotent re-provision (identical bytes) must NOT emit a second
        // ProvisionCalendar record. The DB layer absorbs the duplicate via
        // its identical-bytes check; the journal must mirror that no-op so
        // we don't pad the segment with retried-RPC noise.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [34u8; 32];
        let cal_id = [102u8; 32];
        let meta = b"identical-metadata".to_vec();

        let make_payload = || {
            let req = ProvisionCalendarRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                encrypted_metadata: meta.clone(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created (emits one record).
        let bytes = provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        let manifest_after_first = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest after first");
        assert_eq!(manifest_after_first.calendars.len(), 1);

        // Second call with identical bytes → AlreadyExists (no record emitted).
        let bytes = provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::AlreadyExists);

        let manifest_after_second = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest after second");

        // Full manifest equality — `kind_manifest.next_seg_id` would advance
        // if a duplicate ProvisionCalendar record had been appended, even
        // though the compacted `calendars` Vec would still dedupe to one
        // entry by calendar_id.
        assert_eq!(
            manifest_after_second, manifest_after_first,
            "idempotent re-provision must not emit a duplicate ProvisionCalendar record",
        );
    }

    #[tokio::test]
    async fn provision_calendar_update_metadata_appends_placement_record() {
        // PROPPATCH path: update_metadata=true with new bytes must emit one
        // CalPlacementRecord::UpdateCalendarMetadata record (same atomic-with-
        // SQL gating as the MKCOL Created branch).
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [39u8; 32];
        let cal_id = [113u8; 32];

        // Provision (MKCOL).
        provision_one(&state, &mda, &actor, &cal_id).await;
        let manifest_after_provision = state.cal_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(manifest_after_provision.calendars.len(), 1);
        let hms_after_provision = manifest_after_provision.calendars[0].highestmodseq;

        // PROPPATCH (update_metadata=true).
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"resealed-meta-v2".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Updated);

        let manifest_after_update = state.cal_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            manifest_after_update.calendars.len(),
            1,
            "manifest carries exactly one CalendarState for this calendar",
        );
        assert_eq!(
            manifest_after_update.calendars[0].encrypted_metadata, b"resealed-meta-v2",
            "manifest must carry the new metadata after UpdateCalendarMetadata apply",
        );
        assert!(
            manifest_after_update.calendars[0].highestmodseq > hms_after_provision,
            "manifest's highestmodseq must bump (was {}, now {})",
            hms_after_provision,
            manifest_after_update.calendars[0].highestmodseq,
        );
    }

    #[tokio::test]
    async fn provision_calendar_update_metadata_byte_identical_appends_nothing() {
        // Byte-identical PROPPATCH retry: the DB layer returns Updated without
        // bumping modseq; the journal must mirror the no-op (no duplicate
        // UpdateCalendarMetadata record). Same pattern as the MKCOL
        // AlreadyExists branch.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [40u8; 32];
        let cal_id = [116u8; 32];

        // Provision via the standard helper (b"sealed-meta").
        provision_one(&state, &mda, &actor, &cal_id).await;

        let make_update_payload = || {
            let req = ProvisionCalendarRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                encrypted_metadata: b"identical-resealed-meta".to_vec(),
                update_metadata: true,
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First PROPPATCH → Updated (emits one record).
        let bytes = provision_calendar_handler()(state.clone(), mda, make_update_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Updated);

        let manifest_after_first = state.cal_placement.current_manifest(&actor).await.unwrap();

        // Second PROPPATCH with identical bytes → Updated reply, no journal change.
        let bytes = provision_calendar_handler()(state.clone(), mda, make_update_payload())
            .await
            .unwrap();
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Updated);

        let manifest_after_second = state.cal_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            manifest_after_second, manifest_after_first,
            "byte-identical PROPPATCH retry must not emit a duplicate UpdateCalendarMetadata record",
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_created_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [35u8; 32];
        let cal_id = [103u8; 32];
        let uid_hash = [104u8; 32];
        let timestamp = 1_700_000_000i64;
        let encrypted_body = sealed(b"encrypted-icalendar-body-v1");

        // Provision the calendar first (emits a ProvisionCalendar record).
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let (expected_etag, expected_modseq) = match reply {
            PutEventCiphertextReply::Created { etag, modseq, .. } => (etag, modseq as u64),
            other => panic!("expected Created, got {other:?}"),
        };

        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.events.len(),
            1,
            "PutEvent record must land in the manifest's events Vec",
        );
        let placement = &manifest.events[0];
        assert_eq!(placement.calendar_id, cal_id);
        assert_eq!(placement.uid_hash, uid_hash);
        assert_eq!(placement.etag, expected_etag);
        assert_eq!(placement.modseq, expected_modseq);
        assert_eq!(placement.ciphertext_size, encrypted_body.len() as u32);
    }

    /// S6.9 v2 journal: the PutEvent record carries the content-record id
    /// and the row's EFFECTIVE sidecar. Discriminating on the MUA-preserve
    /// path: a MUA re-PUT (no sidecar on the wire) preserves the prior
    /// Fauna sidecar in the row, and the journal must record the
    /// *preserved* value — journaling the request's `None` would make a
    /// snapshot restore silently drop the sidecar.
    #[tokio::test]
    async fn put_event_journals_the_content_record_id_and_effective_sidecar() {
        use fauna_protocol::bridge_routing::PutEventCiphertextRequest;

        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [39u8; 32];
        let cal_id = [113u8; 32];
        let uid_hash = [114u8; 32];
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Fauna write: carries a sidecar.
        let body1 = sealed(b"icalendar-v1");
        // Struct-update fixture on a growing wire type: keep
        // `..Default::default()` even while it's a no-op, so concurrent
        // field-adds merge cleanly instead of colliding on the grown axis.
        #[allow(clippy::needless_update)]
        let req = PutEventCiphertextRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body: body1.clone(),
            encrypted_index_hint: sealed(b"hint-v1"),
            timestamp,
            ciphertext_size: body1.len() as u32,
            if_match: None,
            encrypted_fauna_ext: Some(b"fauna-sidecar".to_vec()),
            ..Default::default()
        };
        let bytes = put_event_ciphertext_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("fauna put ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let created_event_id = match reply {
            PutEventCiphertextReply::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        };

        let m = state.cal_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            m.events[0].event_id.to_vec(),
            created_event_id.clone(),
            "the journal carries the content-record id"
        );
        assert_eq!(
            m.events[0].encrypted_fauna_ext,
            Some(b"fauna-sidecar".to_vec())
        );

        // MUA re-PUT: new body, NO sidecar on the wire — the row preserves
        // the Fauna sidecar and the journal must record the preserved value.
        let body2 = sealed(b"icalendar-v2-mua-edit");
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body2,
            &sealed(b"hint-v2"),
            timestamp + 10,
            None,
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .expect("mua put ok");
        let reply2: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();
        let updated_event_id = match reply2 {
            PutEventCiphertextReply::Updated { event_id, .. } => event_id,
            other => panic!("expected Updated, got {other:?}"),
        };
        assert_ne!(updated_event_id, created_event_id, "new body, new id");

        let m = state.cal_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(m.events.len(), 1, "supersede, not duplicate");
        assert_eq!(
            m.events[0].event_id.to_vec(),
            updated_event_id,
            "the journal follows the superseding record id"
        );
        assert_eq!(
            m.events[0].encrypted_fauna_ext,
            Some(b"fauna-sidecar".to_vec()),
            "the EFFECTIVE (preserved) sidecar, not the request's None"
        );
    }

    /// S6.9 v2 journal + S6.8d2 prune: a DELETE journals the deleted row's
    /// record id and delete time, and ages out tombstones older than the
    /// effective retention window in the same operation.
    #[tokio::test]
    async fn delete_event_journals_deleted_at_and_prunes_expired_tombstones() {
        use fauna_calendar::segments::placement::EventTombstoneRef;
        use fauna_protocol::bridge_routing::{DeleteEventReply, DeleteEventRequest};

        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [40u8; 32];
        let cal_id = [115u8; 32];
        let uid_hash = [116u8; 32];
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &cal_id).await;

        // An ancient stamped tombstone that must be pruned by the DELETE
        // below (any effective retention window is far shorter than the
        // ~56 years between epoch 1000 and now).
        state
            .cal_placement
            .update_manifest(&actor, |m| {
                m.tombstones.push(EventTombstoneRef {
                    calendar_id: cal_id,
                    uid_hash: [0x0Fu8; 32],
                    modseq: 1,
                    event_id: [0x0Fu8; 32],
                    deleted_at: 1_000,
                });
                true
            })
            .await
            .unwrap();

        let body = sealed(b"icalendar-to-delete");
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body,
            &sealed(b"hint"),
            timestamp,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put ok");

        let del_req = DeleteEventRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            if_match: None,
        };
        let bytes = delete_event_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&del_req).unwrap().to_vec()),
        )
        .await
        .expect("delete ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let deleted_event_id = match reply {
            DeleteEventReply::Deleted { event_id, .. } => event_id,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let m = state.cal_placement.current_manifest(&actor).await.unwrap();
        let ts = m
            .tombstones
            .iter()
            .find(|t| t.uid_hash == uid_hash)
            .expect("fresh tombstone present");
        assert_eq!(
            ts.event_id.to_vec(),
            deleted_event_id,
            "the tombstone carries the deleted row's record id"
        );
        assert!(ts.deleted_at > 0, "the tombstone carries its time");
        assert!(
            !m.tombstones.iter().any(|t| t.uid_hash == [0x0F; 32]),
            "the expired tombstone was pruned by the DELETE (S6.8d2)"
        );
    }

    #[tokio::test]
    async fn put_event_ciphertext_idempotent_appends_nothing() {
        // A transport retry (identical body to an already-stored event)
        // returns ReplaceCaldavEventOutcome::Idempotent — no bump on the
        // SQLite side. The journal must mirror that no-op: emitting a
        // duplicate PutEvent record on a transport retry would advance
        // the manifest's compacted state's modseq to the latest record's
        // modseq (which is the *same* modseq as before, but the segment
        // would carry an extra entry — wasteful and contradicts the
        // existing "no bump was done" comment at the handler).
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [36u8; 32];
        let cal_id = [105u8; 32];
        let uid_hash = [106u8; 32];
        let timestamp = 1_700_000_000i64;
        // Idempotent retry: same sealed bytes across both PUTs (bind once).
        let encrypted_body = sealed(b"encrypted-body-retry");
        let encrypted_hint = sealed(b"encrypted-hint-retry");

        provision_one(&state, &mda, &actor, &cal_id).await;

        // First PUT → Created (one PutEvent record).
        let payload1 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes1 = put_event_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let modseq_before: u64 = match reply1 {
            PutEventCiphertextReply::Created { modseq, .. } => modseq as u64,
            other => panic!("expected Created, got {other:?}"),
        };

        // Identical retry → Idempotent (no record emitted, no modseq bump).
        let payload2 = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes2 = put_event_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        // The handler maps Idempotent to PutEventCiphertextReply::Updated
        // by design (no Idempotent wire variant; the comment at the
        // handler explains why), so we don't reply-pattern-match here —
        // the manifest contents are the load-bearing assertion.
        let _ = bytes2;

        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.events.len(),
            1,
            "Idempotent retry must NOT emit a duplicate PutEvent record",
        );
        assert_eq!(
            manifest.events[0].modseq, modseq_before,
            "Idempotent retry must NOT bump the placement modseq",
        );
        // Calendar-level highestmodseq must also stay pinned to the
        // post-PUT value (would advance if a second PutEvent landed,
        // even with identical bytes — `apply_record_to_manifest`'s
        // `update_modseq` would have run).
        assert_eq!(
            manifest.calendars[0].highestmodseq, modseq_before,
            "calendar highestmodseq must not advance on Idempotent retry",
        );
    }

    #[tokio::test]
    async fn delete_event_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [37u8; 32];
        let cal_id = [107u8; 32];
        let uid_hash = [108u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &cal_id).await;

        // PUT first so there's an event row to delete.
        let put_payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &sealed(b"encrypted-body-for-delete"),
            &sealed(b"encrypted-hint-for-delete"),
            timestamp,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, put_payload)
            .await
            .expect("seed put");

        // Verify the manifest seed: one event placement, no tombstones —
        // otherwise the post-DELETE assertion would pass vacuously.
        let mid = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest mid");
        assert_eq!(
            mid.events.len(),
            1,
            "PUT wiring must seed one event before DELETE test",
        );
        assert!(mid.tombstones.is_empty(), "no tombstones before DELETE",);

        // DELETE → emits a DeleteEvent record.
        let del_payload = make_delete_payload(&actor, &cal_id, &uid_hash, None);
        let bytes = delete_event_handler()(state.clone(), mda, del_payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let expected_modseq: u64 = match reply {
            DeleteEventReply::Deleted { modseq, .. } => modseq as u64,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert!(
            manifest
                .events
                .iter()
                .all(|e| !(e.calendar_id == cal_id && e.uid_hash == uid_hash)),
            "DeleteEvent record must drop the event from the manifest's events Vec",
        );
        assert_eq!(
            manifest.tombstones.len(),
            1,
            "DeleteEvent record must push exactly one tombstone",
        );
        let tomb = &manifest.tombstones[0];
        assert_eq!(tomb.calendar_id, cal_id);
        assert_eq!(tomb.uid_hash, uid_hash);
        assert_eq!(tomb.modseq, expected_modseq);
    }

    #[tokio::test]
    async fn delete_event_not_found_appends_nothing() {
        // DELETE on a never-existed event (or on a calendar with no
        // matching uid_hash) returns NotFound — idempotent, nothing to
        // delete, no journal entry. Mirrors T9's behaviour for DELETE on
        // a non-existent mailbox.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [38u8; 32];
        let cal_id = [109u8; 32];
        let uid_hash = [110u8; 32];

        provision_one(&state, &mda, &actor, &cal_id).await;

        // DELETE without ever PUTting → NotFound, no DeleteEvent record.
        let del_payload = make_delete_payload(&actor, &cal_id, &uid_hash, None);
        let bytes = delete_event_handler()(state.clone(), mda, del_payload)
            .await
            .expect("handler ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteEventReply::NotFound);

        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert!(
            manifest.events.is_empty(),
            "no event placement was ever emitted",
        );
        assert!(
            manifest.tombstones.is_empty(),
            "DeleteEvent NotFound must NOT emit a tombstone",
        );
    }

    // ── T12 end-to-end placement journal round-trip ──
    //
    // Whole-system test: drive a representative CalDAV client workflow
    // through the WS-RPC handler entry points (provision_calendar /
    // put_event_ciphertext / delete_event) and assert the resulting
    // compacted placement manifest — both in-memory via
    // `current_manifest` and on disk via `CalPlacementManifest::load(path)`
    // — matches the expected final state from the CalDAV/IMAP restore
    // design (tracked internally).
    //
    // Actor `[0x60; 32]` to stay clear of every previously claimed range
    // (T10 used `[33u8..38u8; 32]`; other handler tests use `[10u8..]`,
    // `[20u8..]`, `[30u8..]`, `[160u8..]`, `[200u8..]`).

    #[tokio::test]
    async fn placement_journal_round_trip_cal_full_workflow() {
        use fauna_calendar::segments::placement::{
            CalPlacementManifest, cal_placement_manifest_path,
        };

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [0x60u8; 32];
        let work_cal = [0x11u8; 32];
        let personal_cal = [0x22u8; 32];
        let event_a1 = [0xa1u8; 32]; // work_cal event 1 (survives)
        let event_a2 = [0xa2u8; 32]; // work_cal event 2 (deleted)
        let event_b1 = [0xb1u8; 32]; // personal_cal event

        // 1. ProvisionCalendar work_cal with metadata=[1].
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: work_cal.to_vec(),
            encrypted_metadata: vec![1u8],
            ..Default::default()
        };
        let bytes = provision_calendar_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("provision work_cal ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // 2. ProvisionCalendar personal_cal with metadata=[2].
        let req = ProvisionCalendarRequest {
            actor_id: actor.to_vec(),
            calendar_id: personal_cal.to_vec(),
            encrypted_metadata: vec![2u8],
            ..Default::default()
        };
        let bytes = provision_calendar_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("provision personal_cal ok");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // 3. PUT event_a1 into work_cal (ciphertext=[10], hint="e1").
        let payload = make_put_payload(
            &actor,
            &work_cal,
            &event_a1,
            &sealed(&[10u8]),
            &sealed(b"e1"),
            1_700_000_001,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put a1 ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutEventCiphertextReply::Created { .. }),
            "a1 must be Created; got {reply:?}",
        );

        // 4. PUT event_a2 into work_cal (ciphertext=[20], hint="e2").
        let payload = make_put_payload(
            &actor,
            &work_cal,
            &event_a2,
            &sealed(&[20u8]),
            &sealed(b"e2"),
            1_700_000_002,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put a2 ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutEventCiphertextReply::Created { .. }),
            "a2 must be Created; got {reply:?}",
        );

        // 5. PUT event_b1 into personal_cal (ciphertext=[30], hint="e3").
        let payload = make_put_payload(
            &actor,
            &personal_cal,
            &event_b1,
            &sealed(&[30u8]),
            &sealed(b"e3"),
            1_700_000_003,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put b1 ok");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutEventCiphertextReply::Created { .. }),
            "b1 must be Created; got {reply:?}",
        );

        // 6. DELETE event_a2 from work_cal.
        let payload = make_delete_payload(&actor, &work_cal, &event_a2, None);
        let bytes = delete_event_handler()(state.clone(), mda, payload)
            .await
            .expect("delete a2 ok");
        let reply: DeleteEventReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, DeleteEventReply::Deleted { .. }),
            "a2 must be Deleted; got {reply:?}",
        );

        // ── Final-state assertions on the in-memory manifest ──
        let manifest = state
            .cal_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");

        // Two calendars — work_cal and personal_cal.
        assert_eq!(
            manifest.calendars.len(),
            2,
            "exactly two calendars provisioned; got {:?}",
            manifest
                .calendars
                .iter()
                .map(|c| c.calendar_id)
                .collect::<Vec<_>>(),
        );
        assert!(
            manifest.calendars.iter().any(|c| c.calendar_id == work_cal),
            "work_cal calendar row must be present",
        );
        assert!(
            manifest
                .calendars
                .iter()
                .any(|c| c.calendar_id == personal_cal),
            "personal_cal calendar row must be present",
        );

        // Two surviving events: work_cal:a1 + personal_cal:b1.
        // (work_cal:a2 was deleted, so its event placement was dropped.)
        assert_eq!(
            manifest.events.len(),
            2,
            "two surviving events (work_cal:a1 + personal_cal:b1); got {:?}",
            manifest
                .events
                .iter()
                .map(|e| (e.calendar_id, e.uid_hash))
                .collect::<Vec<_>>(),
        );
        assert!(
            manifest
                .events
                .iter()
                .any(|e| e.calendar_id == work_cal && e.uid_hash == event_a1),
            "work_cal:a1 placement must survive",
        );
        assert!(
            manifest
                .events
                .iter()
                .any(|e| e.calendar_id == personal_cal && e.uid_hash == event_b1),
            "personal_cal:b1 placement must survive",
        );

        // One tombstone — work_cal:a2 from the DELETE.
        assert_eq!(
            manifest.tombstones.len(),
            1,
            "exactly one tombstone (work_cal:a2); got {:?}",
            manifest
                .tombstones
                .iter()
                .map(|t| (t.calendar_id, t.uid_hash))
                .collect::<Vec<_>>(),
        );
        let tomb = &manifest.tombstones[0];
        assert_eq!(tomb.calendar_id, work_cal);
        assert_eq!(tomb.uid_hash, event_a2);

        // ── On-disk manifest equals the in-memory snapshot ──
        // `save_atomic` runs after every `append_event`, so the on-disk
        // bytes must round-trip back to byte-equal of `manifest` above.
        let manifest_path = cal_placement_manifest_path(state.cal_placement.data_dir(), &actor);
        let on_disk = CalPlacementManifest::load(&manifest_path)
            .expect("load manifest from disk")
            .expect("manifest file exists after all writes");
        assert_eq!(
            on_disk, manifest,
            "on-disk manifest must equal the in-memory snapshot after every save_atomic",
        );
    }

    // ── T7 restore-divergence tests ────────────────────────────────────────

    /// When the client's sync_token is strictly ahead of the calendar's
    /// highestmodseq (the post-DR-restore "MUA ahead" case, spec § D6 (γ)),
    /// the handler must:
    ///   1. Write one `bridge_restore_divergence` row keyed to the most recent
    ///      `restore_history` row for the actor.
    ///   2. Return `SyncCalendarSinceReply::Stale { server_modseq }` rather
    ///      than the silent zero-rows Ok reply.
    #[tokio::test]
    async fn sync_calendar_since_returns_stale_when_token_ahead() {
        let state = fixture_state().await;
        let mda = [0x79u8; 32];
        let actor = [0x78u8; 32];
        let cal_id = [0x7au8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Provision the calendar (highestmodseq starts at 1).
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Verify hms is 1 so we know what "ahead" means.
        let hms = state
            .db
            .caldav_calendar_highestmodseq(&actor, &cal_id)
            .await
            .unwrap()
            .expect("calendar exists");
        assert_eq!(hms, 1, "freshly provisioned calendar hms must be 1");

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
            .insert_restore_history(&actor, snap_id, "calendar", None)
            .await
            .expect("insert_restore_history");

        // Call the handler with sync_token="5" — ahead of hms=1, so Stale.
        let req = SyncCalendarSinceRequest {
            actor_id: actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            sync_token: "5".to_string(),
            limit: 0,
            mua_id: Some("Apple Calendar/14.0".into()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Must be Stale with server_modseq == hms (1).
        match reply {
            SyncCalendarSinceReply::Stale { server_modseq } => {
                assert_eq!(
                    server_modseq, 1,
                    "Stale server_modseq must equal calendar hms"
                );
            }
            other => panic!("expected SyncCalendarSinceReply::Stale, got {other:?}"),
        }

        // Exactly one bridge_restore_divergence row must have been written.
        let (count, lost, stored_snap_id, mua): (i64, i64, i64, Option<String>) = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*),
                        coalesce(MAX(lost_event_count), -1),
                        coalesce(MAX(snapshot_id), -1),
                        MAX(mua_id)
                 FROM bridge_restore_divergence
                 WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("count bridge_restore_divergence")
        };
        assert_eq!(count, 1, "exactly one divergence row must be written");
        assert_eq!(
            stored_snap_id, snap_id,
            "divergence row must key to snap_id"
        );
        assert_eq!(
            lost, 4,
            "lost_event_count = client_modseq(5) - server_modseq(1) = 4"
        );
        assert_eq!(
            mua.as_deref(),
            Some("Apple Calendar/14.0"),
            "mua_id must be forwarded from the request"
        );
    }

    #[tokio::test]
    async fn sync_calendar_since_returns_stale_ok_when_token_past_retention() {
        // A valid (token <= hms) but past-retention sync-token: a tombstone
        // newer than the token was expunged before the retention cutoff, so
        // nest signals Ok { stale: true } (caldav-server.md § Stale sync-token
        // handling) and writes NO divergence row (retention expiry is normal,
        // not a restore anomaly — that distinguishes it from the Stale arm).
        let state = fixture_state().await;
        let mda = [0x7bu8; 32];
        let actor = [0x7cu8; 32];
        let cal_id = [0x7du8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        provision_one(&state, &mda, &actor, &cal_id).await; // modseq 1

        // Place (modseq 2) then expunge (modseq 3) one event, backdating the
        // tombstone's expunged_at to 31 days ago — past the 30-day retention
        // window the handler reads from the effective ImapPolicy (no
        // put_imap_policy override set in this test → catalog default 30).
        let uid = [0x7eu8; 32];
        state
            .db
            .place_caldav_event(
                &actor,
                &cal_id,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .expect("place");
        let aged_expunged_at = now_epoch_secs() - 31 * 86_400;
        let del = state
            .db
            .delete_caldav_event_by_uid(&actor, &cal_id, &uid, None, aged_expunged_at)
            .await
            .expect("delete");
        let tombstone_modseq = match del {
            crate::db::bridge_caldav::DeleteCaldavEventOutcome::Deleted { modseq, .. } => modseq,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let hms = state
            .db
            .caldav_calendar_highestmodseq(&actor, &cal_id)
            .await
            .unwrap()
            .expect("calendar exists");
        assert!(
            tombstone_modseq <= hms,
            "tombstone modseq {tombstone_modseq} must be <= hms {hms}"
        );

        // sync_token "1": behind the tombstone (3) and <= hms, so not the
        // MUA-ahead Stale branch — the retention branch must fire.
        let payload = make_sync_payload(&actor, &cal_id, "1", 0);
        let bytes = sync_calendar_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SyncCalendarSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncCalendarSinceReply::Ok {
                stale,
                new_sync_token,
                ..
            } => {
                assert!(
                    stale,
                    "past-retention token must signal Ok {{ stale: true }}"
                );
                assert_eq!(
                    new_sync_token,
                    hms.to_string(),
                    "stale reply's new_sync_token is the current hms"
                );
            }
            other => panic!("expected Ok {{ stale: true }}, got {other:?}"),
        }

        // No divergence row — retention expiry is expected, not forensic.
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
            "past-retention stale must NOT write a divergence row"
        );
    }

    // ── Decision B: direct Fauna-app (User class) own-actor path ────

    #[tokio::test]
    async fn caldav_user_own_actor_full_roundtrip() {
        // Decision B end-to-end (events.md § Persistence): a Fauna app —
        // User class (no bridge row, no admin row) — provisions its OWN
        // calendar, writes an event (sealed client-side; nest sees only
        // ciphertext), then reads it back via list_calendars + query_events.
        // This is the direct client path that replaces the legacy plaintext
        // `content`-table route. The caller-scope guard lets the user reach
        // exactly its own data and nothing else.
        let state = fixture_state().await;
        let user_actor = [42u8; 32];
        let cal_id = [43u8; 32];
        state
            .db
            .create_user(&user_actor, "free", "test")
            .await
            .unwrap();

        // 1. provision_calendar (BridgeMda | User, own actor → Created).
        let prov = ProvisionCalendarRequest {
            actor_id: user_actor.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"sealed-personal-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&prov).unwrap().to_vec());
        let bytes = provision_calendar_handler()(state.clone(), user_actor, payload)
            .await
            .expect("a user must be able to provision its own calendar");
        let reply: ProvisionCalendarReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionCalendarReply::Created);

        // 2. put_event_ciphertext (own actor → Created).
        let uid_hash = [7u8; 32];
        let body = sealed(b"sealed-vevent-bytes");
        let put = make_put_payload(
            &user_actor,
            &cal_id,
            &uid_hash,
            &body,
            &sealed(b"sealed-hint"),
            1_700_000_000,
            None,
        );
        let bytes = put_event_ciphertext_handler()(state.clone(), user_actor, put)
            .await
            .expect("a user must be able to write an event to its own calendar");
        let reply: PutEventCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutEventCiphertextReply::Created { .. }),
            "first write is Created, got {reply:?}"
        );

        // 3. list_calendars (own actor) shows the calendar with event_count == 1.
        let bytes =
            list_calendars_handler()(state.clone(), user_actor, make_list_payload(&user_actor))
                .await
                .expect("a user must be able to list its own calendars");
        let reply: ListCalendarsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.calendars.len(), 1);
        assert_eq!(reply.calendars[0].calendar_id, cal_id.to_vec());
        assert_eq!(reply.calendars[0].event_count, 1);

        // 4. query_events (own actor) returns the sealed body verbatim.
        let q = make_query_events_payload(&user_actor, &cal_id, None, None, 0);
        let bytes = query_events_handler()(state.clone(), user_actor, q)
            .await
            .expect("a user must be able to query its own calendar's events");
        let reply: QueryEventsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        match reply {
            QueryEventsReply::Ok { events, .. } => {
                assert_eq!(events.len(), 1, "the one event we wrote");
                assert_eq!(
                    events[0].encrypted_body,
                    body.to_vec(),
                    "nest returns the sealed body opaquely"
                );
                assert_eq!(events[0].uid_hash, uid_hash.to_vec());
            }
            other => panic!("expected Ok with one event, got {other:?}"),
        }
    }

    /// Slice 2 (deployment-home-with-public-relay.md § MUA reach): the per-actor
    /// serving opt-out gates ONLY the MDA-serving path. After actor A disables
    /// serving, the MDA is refused (`mail_serving_disabled`), but A's OWN Fauna
    /// app (User class, `target == caller`) still reads its calendar — the
    /// flag governs where the MDA serves external CalDAV clients, not whether the
    /// user can read their own data (events.md Decision B).
    #[tokio::test]
    async fn caldav_serving_optout_gates_mda_path_only() {
        let state = fixture_state().await;
        let mda = [12u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let user_a = [42u8; 32];
        let cal_id = [43u8; 32];
        state.db.create_user(&user_a, "free", "test").await.unwrap();

        // A provisions its own calendar (User path — ungated).
        let prov = ProvisionCalendarRequest {
            actor_id: user_a.to_vec(),
            calendar_id: cal_id.to_vec(),
            encrypted_metadata: b"sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&prov).unwrap().to_vec());
        provision_calendar_handler()(state.clone(), user_a, payload)
            .await
            .expect("user provisions own calendar");

        // While serving is on (default), the MDA may serve A.
        let q = make_query_events_payload(&user_a, &cal_id, None, None, 0);
        query_events_handler()(state.clone(), mda, q)
            .await
            .expect("MDA serves A while serving is on");

        // A opts OUT of serving on this nest.
        state
            .db
            .set_actor_mail_serving_enabled(&user_a, false)
            .await
            .unwrap();

        // MDA-serving path → rejected with the distinct serving-disabled code.
        let q = make_query_events_payload(&user_a, &cal_id, None, None, 0);
        let err = query_events_handler()(state.clone(), mda, q)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.mail_serving_disabled");

        // A's OWN client (User class, target == caller) is NEVER gated by the
        // serving flag — it still reads its own calendar.
        let q = make_query_events_payload(&user_a, &cal_id, None, None, 0);
        query_events_handler()(state.clone(), user_a, q)
            .await
            .expect("A's own client still reads its calendar after disabling MDA serving");
    }

    // ── S6.6 content-cutover tests ────────────────────────────────────
    //
    // The bar: the sealed body rests only in the actor's `__calendar` segment
    // store (the row carries no body), and every serve path returns the body
    // byte-identically, resolved through the row's stored `record_cid`
    // (`crate::segments::cal::load_event_body`).

    /// Post-cutover write: the body is durable in the segment, reachable
    /// through the row's stored `record_cid`.
    #[tokio::test]
    async fn put_event_stores_the_body_in_the_segment() {
        let state = fixture_state().await;
        let mda = [80u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [81u8; 32];
        let cal_id = [82u8; 32];
        let uid_hash = [83u8; 32];
        // event_id derives from the body bytes, so bind the seal once.
        let body = sealed(b"sealed-icalendar-body");
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body,
            &sealed(b"hint"),
            timestamp,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 10)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);

        let event_id = derive_caldav_event_id(&actor, timestamp, &body);
        // Post-cutover the row's stored cid is the ONLY handle on the record —
        // exactly what the serve path uses.
        let record_cid = page.events[0]
            .record_cid()
            .expect("row cid parses")
            .expect("a segment-served row must carry its record_cid");
        let (envelope, floor) =
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &record_cid)
                .await
                .unwrap()
                .expect("content record must be durable in the __calendar segment");
        assert_eq!(envelope.encrypted_body, body.to_vec());
        assert_eq!(floor.event_id, event_id);
        assert_eq!(floor.calendar_id, cal_id);
        assert_eq!(floor.internal_date, timestamp);
    }

    /// The open path (CalDAV REPORT) must be byte-identical across the cutover.
    #[tokio::test]
    async fn query_events_serves_the_body_from_the_segment() {
        use fauna_protocol::bridge_routing::QueryEventsReply;
        let state = fixture_state().await;
        let mda = [84u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [85u8; 32];
        let cal_id = [86u8; 32];
        let body = sealed(b"sealed-icalendar-body-for-report");
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &[87u8; 32],
            &body,
            &sealed(b"hint"),
            1_700_000_000,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        let q = make_query_events_payload(&actor, &cal_id, None, None, 10);
        let bytes = query_events_handler()(state.clone(), mda, q)
            .await
            .expect("query ok");
        match fauna_cbor::decode_strict::<QueryEventsReply>(&bytes).unwrap() {
            QueryEventsReply::Ok { events, .. } => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].encrypted_body, body.to_vec());
            }
            other => panic!("expected Ok; got {other:?}"),
        }
    }

    /// The incremental sync path (`sync_calendar_since`) is the second serve
    /// site and resolves through the same segment-first loader.
    #[tokio::test]
    async fn sync_calendar_since_serves_the_body_from_the_segment() {
        use fauna_protocol::bridge_routing::SyncCalendarSinceReply;
        let state = fixture_state().await;
        let mda = [88u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [89u8; 32];
        let cal_id = [90u8; 32];
        let body = sealed(b"sealed-icalendar-body-for-sync");
        provision_one(&state, &mda, &actor, &cal_id).await;

        let payload = make_put_payload(
            &actor,
            &cal_id,
            &[91u8; 32],
            &body,
            &sealed(b"hint"),
            1_700_000_000,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        let bytes = sync_calendar_since_handler()(
            state.clone(),
            mda,
            make_sync_payload(&actor, &cal_id, "1", 10),
        )
        .await
        .expect("sync ok");
        match fauna_cbor::decode_strict::<SyncCalendarSinceReply>(&bytes).unwrap() {
            SyncCalendarSinceReply::Ok { changed, .. } => {
                assert_eq!(changed.len(), 1);
                assert_eq!(changed[0].encrypted_body, body.to_vec());
            }
            other => panic!("expected Ok; got {other:?}"),
        }
    }

    /// The recovery half of the no-data-loss story: a crash **between** the
    /// segment append and the row insert must heal on the client's retry.
    ///
    /// Simulated by appending the content record and *not* inserting a row —
    /// exactly the state such a crash leaves — then replaying the PUT. The
    /// retry re-derives the same `event_id` from the same bytes, so
    /// `ensure_in_segment` finds the mirror row and skips (no duplicate record),
    /// and the DAO's empty-body guard is satisfied by that same mirror row, so
    /// the metadata row lands. Body preserved, exactly one content record.
    #[tokio::test]
    async fn a_crash_between_append_and_row_insert_heals_on_retry() {
        let state = fixture_state().await;
        let mda = [96u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [97u8; 32];
        let cal_id = [98u8; 32];
        let uid_hash = [99u8; 32];
        // S6.12: both the pre-seeded record and the PUT retry must carry genuine
        // seals, and — because `event_id` derives from the body bytes — the very
        // same sealed bytes. Bind once (seals are randomized) and reuse.
        let body = sealed(b"sealed-body-that-must-survive-a-crash");
        let hint = sealed(b"index-hint-that-must-survive-a-crash");
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &cal_id).await;

        // Pre-crash state: the content record is durable, no row exists.
        let event_id = derive_caldav_event_id(&actor, timestamp, &body);
        crate::segments::cal::ensure_in_segment(
            &state.cal_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::verify(body.clone())
                .expect("body verifies"),
            &fauna_mls::wrapped_blob::SealedRecordBytes::verify(hint.clone())
                .expect("hint verifies"),
            &CalFloorMetadata {
                calendar_id: cal_id,
                event_id,
                uid_hash: uid_hash.to_vec(),
                ciphertext_size: body.len() as u32,
                internal_date: timestamp,
                created_at: 1_700_000_050,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 10)
            .await
            .unwrap();
        assert!(
            page.events.is_empty(),
            "the crash left no row — that is the premise"
        );

        // The client retries the same PUT.
        let payload = make_put_payload(&actor, &cal_id, &uid_hash, &body, &hint, timestamp, None);
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("the retry must succeed, not wedge on the orphaned record");

        // The row now exists, and the body is still readable through it.
        let page = state
            .db
            .query_caldav_events(&actor, &cal_id, None, None, 10)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event_id, event_id);

        let record_cid = page.events[0]
            .record_cid()
            .expect("row cid parses")
            .expect("a segment-served row must carry its record_cid");
        let (envelope, _floor) =
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &record_cid)
                .await
                .unwrap()
                .expect("body survived the crash");
        assert_eq!(envelope.encrypted_body, body);
        // That the retry did not append a *second* content record is the
        // skip-if-present guard, pinned by
        // `segments::cal::tests::ensure_in_segment_is_idempotent`.
    }

    // ── fauna.calendar.changed push emission ──────────────────────
    //
    // The emit rule under test (transport.md § Push events, ratified
    // 2026-07-17): every durable calendar write — put Created/Updated,
    // delete Deleted, provision Created/metadata-Updated — fires ONE
    // `fauna.calendar.changed` at the calendar owner's own connections;
    // the no-DB-change outcomes (Idempotent / AlreadyExists / NotFound /
    // PreconditionFailed / CalendarMissing) fire NOTHING, mirroring the
    // placement-record no-emit rule.

    /// Receive exactly one `fauna.calendar.changed` push (bounded wait) and
    /// assert its payload names `owner` + `cal_id` in hex.
    async fn expect_calendar_changed(
        rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
        owner: &[u8; 32],
        cal_id: &[u8; 32],
    ) {
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for fauna.calendar.changed push")
            .expect("push channel closed");
        let frame = fauna_protocol::decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            fauna_protocol::Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = fauna_protocol::PushEvent::from_push(&push.kind, push.payload);
        match event {
            fauna_protocol::PushEvent::CalendarChanged(p) => {
                assert_eq!(p.actor_id, hex::encode(owner));
                assert_eq!(p.calendar_id, hex::encode(cal_id));
            }
            other => panic!("expected CalendarChanged push, got {}", other.kind()),
        }
    }

    #[tokio::test]
    async fn put_created_updated_emit_calendar_changed_idempotent_does_not() {
        let state = fixture_state().await;
        let mda = [210u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [211u8; 32];
        let cal_id = [212u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;
        // Subscribe AFTER provisioning so the provision push isn't in the queue.
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let uid_hash = [213u8; 32];
        let body_v1 = sealed(b"BEGIN:VEVENT v1");
        let hint = sealed(b"hint");

        // Created → one push.
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body_v1,
            &hint,
            1_700_000_000,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put created");
        expect_calendar_changed(&mut rx, &actor, &cal_id).await;

        // Updated (different body) → one push.
        let body_v2 = sealed(b"BEGIN:VEVENT v2");
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body_v2,
            &hint,
            1_700_000_100,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put updated");
        expect_calendar_changed(&mut rx, &actor, &cal_id).await;

        // Idempotent retry (same body bytes) → NO push.
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body_v2,
            &hint,
            1_700_000_100,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put idempotent");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn provision_created_emits_calendar_changed_already_exists_does_not() {
        use fauna_protocol::bridge_routing::ProvisionCalendarRequest;

        let state = fixture_state().await;
        let mda = [214u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [215u8; 32];
        let cal_id = [216u8; 32];
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let make_payload = || {
            let req = ProvisionCalendarRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                encrypted_metadata: b"sealed-meta".to_vec(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // Created → one push.
        provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .expect("provision created");
        expect_calendar_changed(&mut rx, &actor, &cal_id).await;

        // Identical retry → AlreadyExists → NO push.
        provision_calendar_handler()(state.clone(), mda, make_payload())
            .await
            .expect("provision already-exists");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn delete_deleted_emits_calendar_changed_not_found_does_not() {
        use fauna_protocol::bridge_routing::DeleteEventRequest;

        let state = fixture_state().await;
        let mda = [217u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [218u8; 32];
        let cal_id = [219u8; 32];
        provision_one(&state, &mda, &actor, &cal_id).await;

        let uid_hash = [220u8; 32];
        let body = sealed(b"BEGIN:VEVENT doomed");
        let hint = sealed(b"hint");
        let payload = make_put_payload(
            &actor,
            &cal_id,
            &uid_hash,
            &body,
            &hint,
            1_700_000_000,
            None,
        );
        put_event_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put created");

        // Subscribe after the put so only the delete's push is observed.
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let make_delete = || {
            let req = DeleteEventRequest {
                actor_id: actor.to_vec(),
                calendar_id: cal_id.to_vec(),
                uid_hash: uid_hash.to_vec(),
                if_match: None,
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // Deleted → one push.
        delete_event_handler()(state.clone(), mda, make_delete())
            .await
            .expect("delete deleted");
        expect_calendar_changed(&mut rx, &actor, &cal_id).await;

        // Repeat → NotFound → NO push.
        delete_event_handler()(state.clone(), mda, make_delete())
            .await
            .expect("delete not-found");
        expect_no_push(&mut rx).await;
    }
}
