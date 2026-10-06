//! WS-RPC handlers for the CardDAV encrypted-mode surface.
//!
//! Mirror of the CalDAV surface (`bridge_caldav_handlers.rs`) for address
//! books / vCards. Allowed caller classes: `BridgeMda` and `BridgeMda | User`.
//!
//! Handlers:
//!   - `provision_addressbook`
//!   - `list_addressbooks`
//!   - `query_cards`
//!   - `put_card_ciphertext`
//!   - `delete_card`
//!   - `sync_addressbook_since`
//!   - `delete_addressbook`

use std::time::Duration;

use fauna_contacts::segments::CardFloorMetadata;
use fauna_contacts::segments::placement::CardPlacementRecord;
use fauna_protocol::{
    bridge_routing::{
        AddressbookEntry, CardEntry, DeleteAddressbookReply, DeleteAddressbookRequest,
        DeleteCardReply, DeleteCardRequest, ExpungedCardEntry, ListAddressbooksReply,
        ListAddressbooksRequest, ProvisionAddressbookReply, ProvisionAddressbookRequest,
        PutCardCiphertextReply, PutCardCiphertextRequest, QueryCardsReply, QueryCardsRequest,
        SyncAddressbookSinceReply, SyncAddressbookSinceRequest,
    },
    decode_strict as decode,
};

use crate::bridge_routing_handlers::{
    encode_reply, internal, malformed, placement_journal_diverged, require_class,
    require_dav_caller_scope,
};
use crate::db::bridge_carddav::{
    CardRow, DeleteCarddavAddressbookOutcome, DeleteCarddavCardOutcome, ExpungedCardRow,
    ProvisionOutcome, ReplaceCarddavCardOutcome, derive_carddav_card_id,
};
use crate::db::now_epoch_secs;
use crate::routes::AppState;
use crate::rpc_router::{RpcKindMeta, RpcRouterBuilder};

/// Map a DB-layer `CardRow` into the wire-shape `CardEntry`. Kept at
/// module scope for reuse in `sync_addressbook_since`.
fn row_to_card_entry(r: CardRow, encrypted_body: Vec<u8>) -> CardEntry {
    CardEntry {
        card_id: r.card_id.to_vec(),
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

/// Resolve each row's sealed body through the `__card` segment store and map the
/// rows onto the wire shape (S6.6 serve cutover) — twin of
/// `bridge_caldav_handlers::rows_to_event_entries`; see its docs.
async fn rows_to_card_entries(
    state: &AppState,
    actor: &[u8; 32],
    rows: Vec<CardRow>,
) -> anyhow::Result<Vec<CardEntry>> {
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let body =
            crate::segments::card::load_card_body(&state.card_segments, &state.db, actor, &row)
                .await?;
        let body = body.unwrap_or_else(|| {
            tracing::warn!(
                actor = %hex::encode(actor),
                card_id = %hex::encode(row.card_id),
                "card body unresolvable (missing record_cid or a segment miss); \
                 serving empty"
            );
            Vec::new()
        });
        entries.push(row_to_card_entry(row, body));
    }
    Ok(entries)
}

/// Map a DB-layer `ExpungedCardRow` into the wire-shape `ExpungedCardEntry`.
fn row_to_expunged_card_entry(r: ExpungedCardRow) -> ExpungedCardEntry {
    ExpungedCardEntry {
        card_id: r.card_id.to_vec(),
        uid_hash: r.uid_hash,
        modseq: r.modseq,
    }
}

// Entry gate for the `BridgeMda | User` CardDAV r/w RPCs: caller scope
// **and** the per-actor serving opt-out — `require_dav_caller_scope` (shared
// with the CalDAV twin) called below with `resource_kind = "card"`,
// `resource_noun = "address book"`.
//
// **Caller scope.** Only the MDA bridge may act on behalf of a *served* user
// (`target != caller`); it AUTH'd the MUA and is trusted to carry the AUTH'd
// actor's id. Every non-bridge caller — `User`, and (via the `Admin ⊇ User`
// promotion in `is_permitted`) `Admin` — may touch only their OWN actor's
// address books (`target == caller`). This is the load-bearing invariant that
// makes the `BridgeMda | User` allowlist safe for the direct Fauna-app path
// (carddav-server.md Decision B): a client seals locally and writes under its own
// `actor_id`, so nest never sees plaintext and no caller can reach another
// actor's address book (carddav-server.md § Threat model, architectural rule
// "MUA-AUTH never grants write access beyond the AUTH'd actor's address
// books").
//
// **Serving opt-out.** For the MDA-serving path only (`class == BridgeMda`),
// also refuse when `target` has turned IMAP/CardDAV serving OFF on this nest
// (per-actor, user-set; default ON). This is the CardDAV twin of the IMAP-side
// `require_local_mail_serving` gate — one user-set flag covers both protocols
// the MDA hosts. The user's OWN client path (`User`/`Admin`, `target ==
// caller`) is **never** gated by the serving flag: it governs where the *MDA*
// serves external CardDAV clients, not whether the user can read their own
// address book via their Fauna app (carddav-server.md Decision B). Spec:
// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
// § MUA reach.

/// Emit `fauna.addressbook.changed` at the address book owner's own connected
/// clients after a durable card or book write (put Created/Updated, delete
/// Deleted, provision Created/metadata-Updated — never Idempotent /
/// PreconditionFailed / AddressbookMissing / NotFound / AlreadyExists /
/// Conflict, which leave the DB unchanged; the same rule the placement-record
/// appends beside every call site follow, which is why each call sits inside
/// the arm that just appended).
///
/// The carddav twin of `bridge_caldav_handlers::notify_calendar_changed`:
/// best-effort, own-device fanout (`notify_push`'s single-actor axis), carrying
/// only plaintext the write path already holds. Consumers re-fetch/re-sync; the
/// index builder's attach-time reconcile walk and the pages' polls + reconnect
/// re-pull are the correctness backstop
/// (`docs/goal/architecture/transport.md` § Push events, ratified 2026-08-05).
pub(crate) fn notify_addressbook_changed(
    state: &std::sync::Arc<crate::routes::AppState>,
    owner: &[u8; 32],
    addressbook_id: &[u8; 32],
) {
    state.ws.notify_push(
        owner,
        fauna_protocol::PushEvent::AddressBookChanged(
            fauna_protocol::push_events::AddressBookChangedPayload {
                actor_id: hex::encode(owner),
                addressbook_id: hex::encode(addressbook_id),
                extra: std::collections::BTreeMap::new(),
            },
        ),
    );
}

// ── provision_addressbook ─────────────────────────────────────────

fn provision_addressbook_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.provision_addressbook").await?;
            let req: ProvisionAddressbookRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;

            // Caller-scoping (see `require_dav_caller_scope`): a non-bridge caller —
            // `User`, or `Admin` via the `Admin ⊇ User` promotion — may
            // provision only its OWN address book; only the MDA may provision
            // for a served user.
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
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
                let hms_before = state
                    .db
                    .carddav_addressbook_highestmodseq(&target, &book)
                    .await
                    .map_err(internal)?;
                let outcome = state
                    .db
                    .update_bridge_carddav_addressbook_metadata(
                        &target,
                        &book,
                        &req.encrypted_metadata,
                    )
                    .await
                    .map_err(internal)?;
                match outcome {
                    ProvisionOutcome::Updated => {
                        // Emit only when bytes actually changed. The DB layer
                        // signals byte-identical retries by leaving
                        // highestmodseq unchanged; the placement journal must
                        // mirror that no-op (twin of the CalDAV PROPPATCH
                        // gate in `bridge_caldav_handlers.rs`).
                        let hms_after = state
                            .db
                            .carddav_addressbook_highestmodseq(&target, &book)
                            .await
                            .map_err(internal)?;
                        if hms_before != hms_after
                            && let Some(new_hms) = hms_after
                        {
                            let record = CardPlacementRecord::UpdateAddressbookMetadata {
                                addressbook_id: book,
                                encrypted_metadata: req.encrypted_metadata.clone(),
                                modseq: new_hms as u64,
                            };
                            state
                                .card_placement
                                .append_event(&target, &record)
                                .await
                                .map_err(placement_journal_diverged)?;
                            notify_addressbook_changed(&state, &target, &book);
                        }
                        ProvisionAddressbookReply::Updated
                    }
                    ProvisionOutcome::NotFound => ProvisionAddressbookReply::NotFound,
                    other => unreachable!(
                        "update_bridge_carddav_addressbook_metadata cannot return {:?}",
                        other,
                    ),
                }
            } else {
                let now = now_epoch_secs();
                let outcome = state
                    .db
                    .insert_bridge_carddav_addressbook(&target, &book, &req.encrypted_metadata, now)
                    .await
                    .map_err(internal)?;
                match outcome {
                    ProvisionOutcome::Created => {
                        // Spec § D7 (ProvisionAddressbook record shape):
                        // `addressbook_id` + `encrypted_metadata`. The
                        // manifest-side apply seeds a fresh `AddressbookState`
                        // with `highestmodseq = 1` (see
                        // `apply_record_to_manifest` in
                        // `segments/card_placement.rs`), matching the DB
                        // layer's post-provision modseq = 1.
                        //
                        // Commit-then-append: the SQLite INSERT inside
                        // `insert_bridge_carddav_addressbook` has committed by
                        // the time the placement append below runs; the narrow
                        // crash window is closed by divergence detection at
                        // sync-collection / REPORT time (twin of the CalDAV
                        // MKCOL wiring). No emit on AlreadyExists / Conflict —
                        // the DB row is unchanged, so the journal mirrors the
                        // no-op.
                        let record = CardPlacementRecord::ProvisionAddressbook {
                            addressbook_id: book,
                            encrypted_metadata: req.encrypted_metadata.clone(),
                        };
                        state
                            .card_placement
                            .append_event(&target, &record)
                            .await
                            .map_err(placement_journal_diverged)?;
                        notify_addressbook_changed(&state, &target, &book);
                        ProvisionAddressbookReply::Created
                    }
                    ProvisionOutcome::AlreadyExists => ProvisionAddressbookReply::AlreadyExists,
                    ProvisionOutcome::Conflict => ProvisionAddressbookReply::Conflict,
                    other => {
                        unreachable!(
                            "insert_bridge_carddav_addressbook cannot return {:?}",
                            other,
                        )
                    }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── list_addressbooks ─────────────────────────────────────────────

fn list_addressbooks_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.list_addressbooks").await?;
            let req: ListAddressbooksRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book_rows = state
                .db
                .list_bridge_carddav_addressbooks(&target)
                .await
                .map_err(internal)?;

            let mut addressbooks = Vec::with_capacity(book_rows.len());
            for row in book_rows {
                let card_count = state
                    .db
                    .count_bridge_carddav_cards(&target, &row.addressbook_id)
                    .await
                    .map_err(internal)?;
                addressbooks.push(AddressbookEntry {
                    addressbook_id: row.addressbook_id.to_vec(),
                    encrypted_metadata: row.encrypted_metadata,
                    ctag: row.ctag,
                    highestmodseq: row.highestmodseq,
                    card_count,
                    created_at: row.created_at,
                });
            }
            encode_reply(&ListAddressbooksReply { addressbooks })
        })
    })
}

// ── query_cards ───────────────────────────────────────────────────

fn query_cards_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.query_cards").await?;
            let req: QueryCardsRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
                    .map_err(malformed)?;

            // Validate after_card_id length if present.
            if let Some(ref b) = req.after_card_id
                && b.len() != 32
            {
                return Err(malformed("after_card_id must be 32 bytes"));
            }

            // Check address book exists; capture highestmodseq without a second query.
            let hms = match state
                .db
                .carddav_addressbook_highestmodseq(&target, &book)
                .await
                .map_err(internal)?
            {
                None => return encode_reply(&QueryCardsReply::AddressbookNotFound),
                Some(h) => h,
            };

            // Pass wire_limit + 1 to DB so pagination detection works inside
            // query_carddav_cards (it trims and sets CardPage::more).
            let fetch_limit = if req.limit == 0 {
                0
            } else {
                req.limit.saturating_add(1)
            };

            let after_owned: Option<[u8; 32]> = req
                .after_card_id
                .as_ref()
                .map(|b| b.as_ref().try_into().unwrap()); // safe: validated above

            let page = state
                .db
                .query_carddav_cards(
                    &target,
                    &book,
                    req.since_modseq,
                    after_owned.as_ref(),
                    fetch_limit,
                )
                .await
                .map_err(internal)?;

            let cards = rows_to_card_entries(&state, &target, page.cards)
                .await
                .map_err(internal)?;

            encode_reply(&QueryCardsReply::Ok {
                cards,
                highestmodseq: hms,
                more: page.more,
            })
        })
    })
}

// ── put_card_ciphertext ───────────────────────────────────────────

fn put_card_ciphertext_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.put_card_ciphertext").await?;
            let req: PutCardCiphertextRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
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
            // S6.12: structural seal gate — exact twin of
            // `put_event_ciphertext_handler` (see its comment). Proven at the
            // wire edge, before any state is touched.
            let sealed_body =
                fauna_mls::wrapped_blob::SealedRecordBytes::verify(req.encrypted_body.clone())
                    .map_err(|_| malformed("encrypted_body is not a sealed recipient envelope"))?;
            let sealed_hint = fauna_mls::wrapped_blob::SealedRecordBytes::verify(
                req.encrypted_index_hint.clone(),
            )
            .map_err(|_| malformed("encrypted_index_hint is not a sealed recipient envelope"))?;

            // The shared storage quota — the card twin of
            // `store_sealed_event`'s pre-check (`caldav-server.md` § QUOTA →
            // § Enforcement points), before the segment append so a refusal
            // writes nothing.
            let replaced = state
                .db
                .carddav_card_ciphertext_size(&target, &book, &req.uid_hash)
                .await
                .map_err(internal)?;
            crate::bridge_imap_handlers::enforce_dav_write_quota(
                &state,
                &target,
                req.ciphertext_size,
                replaced,
            )
            .await?;

            let now = now_epoch_secs();

            // S6.6 content cutover — exact twin of `put_event_ciphertext_handler`
            // (see its comment for why the segment append must precede the row,
            // and why `card_id` is derived here rather than inside the DAO).
            let card_id = derive_carddav_card_id(&target, req.timestamp, &req.encrypted_body);
            // `created_at: now` (→ the row's `received_at`) MUST stay
            // server-assigned — the orphan reaper's in-flight watermark keys
            // on it (see the calendar twin's comment); `req.timestamp` goes
            // to `internal_date` only.
            let floor = CardFloorMetadata {
                addressbook_id: book,
                card_id,
                uid_hash: req.uid_hash.clone(),
                ciphertext_size: req.ciphertext_size,
                internal_date: req.timestamp,
                created_at: now,
                ..Default::default()
            };
            let record_cid = crate::segments::card::ensure_in_segment(
                &state.card_segments,
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
                .replace_carddav_card_by_uid(
                    &target,
                    &book,
                    &req.uid_hash,
                    req.if_match.as_deref(),
                    &card_id,
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
                ReplaceCarddavCardOutcome::Created {
                    card_id,
                    etag,
                    modseq,
                    encrypted_fauna_ext,
                } => {
                    // Spec § D7 (PutCard record shape): `addressbook_id` +
                    // `uid_hash` + `etag` + `modseq` + `ciphertext_size`. The
                    // manifest apply dedups on (addressbook_id, uid_hash) and
                    // bumps the address book's highestmodseq (twin of the
                    // CalDAV PutEvent wiring; commit-then-append). No emit on
                    // Idempotent / PreconditionFailed / AddressbookMissing.
                    let record = CardPlacementRecord::PutCard {
                        addressbook_id: book,
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
                        card_id,
                        encrypted_fauna_ext: encrypted_fauna_ext.clone(),
                    };
                    state
                        .card_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    notify_addressbook_changed(&state, &target, &book);
                    PutCardCiphertextReply::Created {
                        card_id: card_id.to_vec(),
                        etag,
                        modseq,
                    }
                }
                ReplaceCarddavCardOutcome::Updated {
                    card_id,
                    etag,
                    modseq,
                    encrypted_fauna_ext,
                } => {
                    // See Created arm: same PutCard record shape + atomicity
                    // contract. Updated differs only in the DB row being
                    // replaced (a tombstone for the prior card_id recorded in
                    // the same SQLite tx) — neither alters the placement shape.
                    let record = CardPlacementRecord::PutCard {
                        addressbook_id: book,
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
                        card_id,
                        encrypted_fauna_ext: encrypted_fauna_ext.clone(),
                    };
                    state
                        .card_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    notify_addressbook_changed(&state, &target, &book);
                    PutCardCiphertextReply::Updated {
                        card_id: card_id.to_vec(),
                        etag,
                        modseq,
                    }
                }
                // Idempotent transport retry: the new body hashes to the same card_id as the
                // prior row, so no bump was done. The wire shape has no Idempotent variant;
                // reporting Updated is semantically coherent because the row state already
                // matches what the caller intended — the caller can use the returned etag
                // for subsequent conditional requests without distinguishing new-update from retry.
                ReplaceCarddavCardOutcome::Idempotent {
                    card_id,
                    etag,
                    modseq,
                    encrypted_fauna_ext: _,
                } => PutCardCiphertextReply::Updated {
                    card_id: card_id.to_vec(),
                    etag,
                    modseq,
                },
                ReplaceCarddavCardOutcome::PreconditionFailed { current_etag } => {
                    PutCardCiphertextReply::PreconditionFailed { current_etag }
                }
                ReplaceCarddavCardOutcome::AddressbookMissing => {
                    PutCardCiphertextReply::AddressbookNotFound
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── delete_card ───────────────────────────────────────────────────

fn delete_card_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.bridges.delete_card").await?;
            let req: DeleteCardRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
                    .map_err(malformed)?;

            if req.uid_hash.len() != 32 {
                return Err(malformed("uid_hash must be 32 bytes"));
            }

            let now = now_epoch_secs();
            let outcome = state
                .db
                .delete_carddav_card_by_uid(
                    &target,
                    &book,
                    &req.uid_hash,
                    req.if_match.as_deref(),
                    now,
                )
                .await
                .map_err(internal)?;

            let reply = match outcome {
                DeleteCarddavCardOutcome::Deleted { card_id, modseq } => {
                    // Spec § D7 (DeleteCard record shape): `addressbook_id` +
                    // `uid_hash` + `modseq`. The manifest apply drops the card
                    // placement and pushes a tombstone unconditionally (so
                    // WebDAV-Sync REPORT surfaces the deletion even against a
                    // drifted local manifest). Twin of the CalDAV DeleteEvent
                    // wiring; commit-then-append. No emit on NotFound /
                    // PreconditionFailed — neither mutates the DB.
                    let record = CardPlacementRecord::DeleteCard {
                        addressbook_id: book,
                        uid_hash: req
                            .uid_hash
                            .as_slice()
                            .try_into()
                            .expect("uid_hash validated to length 32 above"),
                        modseq: modseq as u64,
                        // v2 (S6.9): the deleted row's content-record id
                        // (restore rebuilds the `bridge_carddav_expunged` row
                        // from it) + the delete time that makes the S6.8d2
                        // retention prune possible.
                        card_id,
                        deleted_at: now,
                    };
                    state
                        .card_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    // S6.8d2 — age out tombstones below the same effective
                    // retention window the sync serve path enforces (twin of
                    // the CalDAV delete_event wiring; see it for why the
                    // prune is unobservable and rides DELETE).
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
                        .card_placement
                        .prune_tombstones(&target, now - retention_days * 86_400)
                        .await
                        .map_err(internal)?;
                    notify_addressbook_changed(&state, &target, &book);
                    DeleteCardReply::Deleted {
                        card_id: card_id.to_vec(),
                        modseq,
                    }
                }
                DeleteCarddavCardOutcome::NotFound => DeleteCardReply::NotFound,
                DeleteCarddavCardOutcome::PreconditionFailed { current_etag } => {
                    DeleteCardReply::PreconditionFailed { current_etag }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── delete_addressbook ────────────────────────────────────────────

fn delete_addressbook_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.delete_addressbook").await?;
            let req: DeleteAddressbookRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
                    .map_err(malformed)?;

            let outcome = state
                .db
                .delete_carddav_addressbook(&target, &book)
                .await
                .map_err(internal)?;

            let reply = match outcome {
                DeleteCarddavAddressbookOutcome::Deleted { cards_deleted } => {
                    // Card-only op (CalDAV has no delete-calendar RPC). Emit a
                    // DeleteAddressbook record; the manifest apply cascades the
                    // address book + its cards + its tombstones away (see
                    // `apply_record_to_manifest` in
                    // `segments/card_placement.rs`). Commit-then-append. No
                    // emit on NotFound — an idempotent re-delete leaves the DB
                    // unchanged, so the journal mirrors the no-op.
                    let record = CardPlacementRecord::DeleteAddressbook {
                        addressbook_id: book,
                    };
                    state
                        .card_placement
                        .append_event(&target, &record)
                        .await
                        .map_err(placement_journal_diverged)?;
                    notify_addressbook_changed(&state, &target, &book);
                    DeleteAddressbookReply::Deleted { cards_deleted }
                }
                DeleteCarddavAddressbookOutcome::NotFound => DeleteAddressbookReply::NotFound,
            };
            encode_reply(&reply)
        })
    })
}

// ── sync_addressbook_since ────────────────────────────────────────

fn sync_addressbook_since_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.sync_addressbook_since").await?;
            let req: SyncAddressbookSinceRequest = decode(&payload).map_err(malformed)?;

            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            require_dav_caller_scope(&state, class, &target, &actor_id, "card", "address book")
                .await?;

            let book: [u8; 32] =
                crate::rpc_errors::require_bytes32("addressbook_id", req.addressbook_id.as_slice())
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

            // Check address book exists; capture highestmodseq (becomes new_sync_token).
            let hms = match state
                .db
                .carddav_addressbook_highestmodseq(&target, &book)
                .await
                .map_err(internal)?
            {
                None => return encode_reply(&SyncAddressbookSinceReply::AddressbookNotFound),
                Some(h) => h,
            };

            // Restore-divergence detection. When the client's sync_token is
            // ahead of the address book's current highestmodseq, the most
            // plausible cause is a DR restore that landed the address book at
            // an earlier modseq than the MUA had observed. Log the divergence
            // + return Stale so the MUA falls through to full PROPFIND per
            // RFC 6578 §3.8.
            if since_modseq > hms {
                let now = fauna_core::data::Timestamp::now_secs_or_zero();
                let book_hex = hex::encode(book);
                {
                    let conn = state.db.conn().await;
                    let tx = conn.unchecked_transaction().map_err(internal)?;
                    crate::restore::divergence::write_divergence_row(
                        &tx,
                        &target,
                        "carddav",
                        &book_hex,
                        req.mua_id.as_deref(),
                        since_modseq,
                        hms,
                        now,
                    )
                    .map_err(internal)?;
                    tx.commit().map_err(internal)?;
                }
                return encode_reply(&SyncAddressbookSinceReply::Stale { server_modseq: hms });
            }

            // Stale-sync-token-past-retention detection (carddav-server.md
            // § Stale sync-token handling). The token is valid (<= hms) but
            // may predate the tombstone-retention window: if any tombstone
            // newer than since_modseq was expunged before the retention
            // cutoff, nest can no longer honestly enumerate the deletions
            // since the token, so it signals stale and the MUA full-resyncs
            // (PROPFIND + per-card GET). Distinct from the Stale (MUA-ahead)
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
                    .carddav_has_expunged_past_retention(&target, &book, since_modseq, cutoff_ts)
                    .await
                    .map_err(internal)?
                {
                    return encode_reply(&SyncAddressbookSinceReply::Ok {
                        changed: vec![],
                        expunged: vec![],
                        new_sync_token: hms.to_string(),
                        more: false,
                        stale: true,
                    });
                }
            }

            // Pass wire_limit + 1 to DB so pagination detection works inside
            // query_carddav_changes_since (it trims and sets CardPage::more).
            let fetch_limit = if req.limit == 0 {
                0
            } else {
                req.limit.saturating_add(1)
            };

            // query_carddav_changes_since orders by modseq ASC so the modseq
            // cursor is always valid for resuming the next page.
            let page = state
                .db
                .query_carddav_changes_since(&target, &book, since_modseq, fetch_limit)
                .await
                .map_err(internal)?;

            let expunged_rows = state
                .db
                .query_carddav_expunged_since(&target, &book, since_modseq)
                .await
                .map_err(internal)?;

            let changed = rows_to_card_entries(&state, &target, page.cards)
                .await
                .map_err(internal)?;
            let expunged: Vec<ExpungedCardEntry> = expunged_rows
                .into_iter()
                .map(row_to_expunged_card_entry)
                .collect();

            // When more == true: use the modseq of the last returned card as
            // new_sync_token so the next paged call resumes from exactly the
            // right place (modseq > last_returned_modseq).  Safety: `more ==
            // true` is only set when `cards.len() == wire_limit >= 1`, so
            // `changed.last()` is always Some here.
            // When more == false: use the address-book-wide highestmodseq so
            // that future incremental syncs pick up all changes made after
            // this call.
            let new_sync_token = if page.more {
                changed.last().unwrap().modseq.to_string()
            } else {
                hms.to_string()
            };
            encode_reply(&SyncAddressbookSinceReply::Ok {
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

// ── Registration entry point ──────────────────────────────────────

pub fn register_bridge_carddav_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.provision_addressbook",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: provision_addressbook_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_addressbooks",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_addressbooks_handler(),
        },
    );
    b.add(
        "fauna.bridges.query_cards",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: query_cards_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_card_ciphertext",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: put_card_ciphertext_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_card",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_card_handler(),
        },
    );
    b.add(
        "fauna.bridges.sync_addressbook_since",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: sync_addressbook_since_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_addressbook",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_addressbook_handler(),
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

    // ── provision_addressbook tests ───────────────────────────────

    #[tokio::test]
    async fn provision_addressbook_created_inserts_row() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [20u8; 32];
        let book_id = [30u8; 32];
        let meta = b"sealed-addressbook-metadata".to_vec();

        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: meta,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        // Row must exist.
        let exists = state
            .db
            .ensure_bridge_carddav_addressbook_exists(&actor, &book_id)
            .await
            .unwrap();
        assert!(exists, "address book row must be present after Created");
    }

    #[tokio::test]
    async fn provision_addressbook_identical_bytes_returns_already_exists() {
        let state = fixture_state().await;
        let mda = [11u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [21u8; 32];
        let book_id = [31u8; 32];
        let meta = b"same-metadata".to_vec();

        let make_payload = || {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: meta.clone(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created.
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        // Second call with identical bytes → AlreadyExists.
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::AlreadyExists);

        // Only one row.
        let rows = state
            .db
            .list_bridge_carddav_addressbooks(&actor)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "must be exactly one address book row after idempotent retry"
        );
    }

    #[tokio::test]
    async fn provision_addressbook_differing_bytes_returns_conflict_and_preserves_original() {
        let state = fixture_state().await;
        let mda = [12u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [22u8; 32];
        let book_id = [32u8; 32];
        let meta_orig = b"original-metadata".to_vec();
        let meta_new = b"different-metadata".to_vec();

        let make_payload = |meta: Vec<u8>| {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: meta,
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created.
        let bytes =
            provision_addressbook_handler()(state.clone(), mda, make_payload(meta_orig.clone()))
                .await
                .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        // Second call with different bytes → Conflict.
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_payload(meta_new))
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Conflict);

        // Original metadata bytes are unchanged.
        let rows = state
            .db
            .list_bridge_carddav_addressbooks(&actor)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].encrypted_metadata, meta_orig,
            "original metadata must be preserved on Conflict"
        );
    }

    #[tokio::test]
    async fn provision_addressbook_update_metadata_returns_updated() {
        let state = fixture_state().await;
        let mda = [25u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [26u8; 32];
        let book_id = [27u8; 32];

        // Provision first (MKCOL path).
        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"meta-v1".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        // PROPPATCH path: update_metadata=true with new bytes.
        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"meta-v2-resealed".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("update handler ok");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Updated);

        let rows = state
            .db
            .list_bridge_carddav_addressbooks(&actor)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].encrypted_metadata, b"meta-v2-resealed");
    }

    #[tokio::test]
    async fn provision_addressbook_update_metadata_returns_not_found_when_addressbook_missing() {
        let state = fixture_state().await;
        let mda = [28u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // No prior insert — addressbook_id [29u8; 32] does not exist.
        let req = ProvisionAddressbookRequest {
            actor_id: vec![29u8; 32],
            addressbook_id: vec![29u8; 32],
            encrypted_metadata: b"meta-resealed".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("update handler ok");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::NotFound);

        // Update must not create a row.
        let rows = state
            .db
            .list_bridge_carddav_addressbooks(&[29u8; 32])
            .await
            .unwrap();
        assert!(rows.is_empty(), "NotFound path must not insert rows");
    }

    #[tokio::test]
    async fn provision_addressbook_malformed_wrong_length_addressbook_id() {
        let state = fixture_state().await;
        let mda = [13u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionAddressbookRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![1u8; 16], // wrong length
            encrypted_metadata: b"some-metadata".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_addressbook_malformed_empty_encrypted_metadata() {
        let state = fixture_state().await;
        let mda = [14u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionAddressbookRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![1u8; 32],
            encrypted_metadata: vec![], // empty — invalid
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_addressbook_malformed_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [15u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ProvisionAddressbookRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![1u8; 32],
            encrypted_metadata: b"some-metadata".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn provision_addressbook_user_class_accepted() {
        let state = fixture_state().await;
        // No bridge service-user row, no admin row, but a `users` row → the
        // authority gate resolves it to User class.
        let user_actor = [50u8; 32];
        let book_id = [60u8; 32];
        state
            .db
            .create_user(&user_actor, "free", "test")
            .await
            .unwrap();

        let req = ProvisionAddressbookRequest {
            actor_id: user_actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"user-sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), user_actor, payload)
            .await
            .expect("User class must be accepted");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);
    }

    #[tokio::test]
    async fn provision_addressbook_bridge_mta_denied() {
        let state = fixture_state().await;
        let mta = [70u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = ProvisionAddressbookRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn provision_addressbook_admin_other_actor_denied() {
        // admin ⊇ user, but the kind is caller-scoped: an admin inherits the
        // User grant and may provision its OWN address book, never another
        // actor's. Targeting a different actor is denied (only the MDA bridge
        // may provision on behalf of a served user). The guard is what keeps
        // the `Admin ⊇ User` inheritance safe.
        let state = fixture_state().await;
        let admin_actor = [80u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

        let req = ProvisionAddressbookRequest {
            actor_id: vec![1u8; 32], // a DIFFERENT actor
            addressbook_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn provision_addressbook_admin_own_accepted() {
        // admin ⊇ user: an admin may provision its OWN address book (target ==
        // caller), inheriting the User grant.
        let state = fixture_state().await;
        let admin_actor = [80u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

        let req = ProvisionAddressbookRequest {
            actor_id: admin_actor.to_vec(), // SELF
            addressbook_id: [2u8; 32].to_vec(),
            encrypted_metadata: b"admin-own-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state, admin_actor, payload)
            .await
            .expect("an admin provisioning its own address book must be accepted");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);
    }

    #[tokio::test]
    async fn provision_addressbook_user_other_actor_denied() {
        // Caller-scoping guard: a plain User may provision only its OWN
        // address book — targeting another actor is denied. (Before the guard
        // this was an unguarded cross-actor write — a user could write any
        // actor's address book metadata.)
        let state = fixture_state().await;
        // No bridge service-user row, no admin row → User class.
        let user_actor = [50u8; 32];

        let req = ProvisionAddressbookRequest {
            actor_id: vec![99u8; 32], // a DIFFERENT actor
            addressbook_id: vec![2u8; 32],
            encrypted_metadata: b"meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_addressbook_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── list_addressbooks tests ───────────────────────────────────────

    fn make_list_payload(actor_id: &[u8; 32]) -> Bytes {
        use fauna_protocol::bridge_routing::ListAddressbooksRequest;
        let req = ListAddressbooksRequest {
            actor_id: actor_id.to_vec(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn list_addressbooks_empty_returns_empty_vec() {
        let state = fixture_state().await;
        let mda = [90u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [91u8; 32];

        let bytes = list_addressbooks_handler()(state, mda, make_list_payload(&actor))
            .await
            .expect("handler ok");
        let reply: fauna_protocol::bridge_routing::ListAddressbooksReply =
            fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            reply.addressbooks.is_empty(),
            "fresh actor must have no address books"
        );
    }

    #[tokio::test]
    async fn list_addressbooks_after_two_provisions_one_put_returns_two_with_correct_counts() {
        use fauna_protocol::bridge_routing::ListAddressbooksReply;

        let state = fixture_state().await;
        let mda = [92u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [93u8; 32];
        let book_1 = [101u8; 32];
        let book_2 = [102u8; 32];
        let meta_1 = b"sealed-meta-book-1".to_vec();
        let meta_2 = b"sealed-meta-book-2".to_vec();
        let now_ts = 1_700_000_000i64;

        // Provision two address books.
        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_1, &meta_1, now_ts)
            .await
            .unwrap();
        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_2, &meta_2, now_ts + 1)
            .await
            .unwrap();

        // PUT one card into book_1 only.
        state
            .db
            .place_carddav_card(
                &actor,
                &book_1,
                &[50u8; 32],
                b"encrypted-body",
                b"encrypted-hint",
                now_ts + 2,
                14,
                now_ts + 100,
            )
            .await
            .unwrap();

        let bytes = list_addressbooks_handler()(state, mda, make_list_payload(&actor))
            .await
            .expect("handler ok");
        let reply: ListAddressbooksReply = fauna_cbor::decode_strict(&bytes).unwrap();

        assert_eq!(reply.addressbooks.len(), 2, "two address books expected");

        // Order is created_at ASC — book_1 first.
        let e1 = &reply.addressbooks[0];
        let e2 = &reply.addressbooks[1];

        assert_eq!(e1.addressbook_id, book_1.to_vec());
        assert_eq!(e1.encrypted_metadata, meta_1);
        // After provision: ctag=0, highestmodseq=1.
        // After PUT: ctag and highestmodseq both bump to 2 (lockstep).
        assert_eq!(e1.ctag, 2, "book_1 got a PUT → ctag bumps in lockstep to 2");
        assert_eq!(e1.highestmodseq, 2, "book_1 got a PUT → highestmodseq==2");
        assert_eq!(e1.card_count, 1, "book_1 has one card");

        assert_eq!(e2.addressbook_id, book_2.to_vec());
        assert_eq!(e2.encrypted_metadata, meta_2);
        assert_eq!(e2.ctag, 0, "book_2 is provision-only → ctag still 0");
        assert_eq!(
            e2.highestmodseq, 1,
            "book_2 is provision-only → highestmodseq==1"
        );
        assert_eq!(e2.card_count, 0, "book_2 has no cards");
    }

    #[tokio::test]
    async fn list_addressbooks_bridge_mta_class_denied() {
        // The MTA role is neither BridgeMda nor User → still denied (only the
        // MDA + the actor's own Fauna app reach the CardDAV r/w RPCs).
        let state = fixture_state().await;
        let mta = [94u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [95u8; 32];

        let err = list_addressbooks_handler()(state, mta, make_list_payload(&actor))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_addressbooks_user_other_actor_denied() {
        // Decision B widened this RPC to `User`, but the caller-scope guard
        // still denies a user listing ANOTHER actor's address books.
        let state = fixture_state().await;
        // No bridge row, no admin row → User class.
        let user_actor = [96u8; 32];
        let other = [200u8; 32];

        let err = list_addressbooks_handler()(state, user_actor, make_list_payload(&other))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_addressbooks_admin_other_actor_denied() {
        // admin ⊇ user, but still caller-scoped: an admin may not list another
        // actor's address books.
        let state = fixture_state().await;
        let admin_actor = [97u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let err = list_addressbooks_handler()(state, admin_actor, make_list_payload(&other))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn list_addressbooks_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [98u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::ListAddressbooksRequest;
        let req = ListAddressbooksRequest {
            actor_id: vec![1u8; 16], // wrong length
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_addressbooks_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── query_cards tests ─────────────────────────────────────────────

    fn make_query_cards_payload(
        actor_id: &[u8; 32],
        addressbook_id: &[u8; 32],
        since_modseq: Option<i64>,
        after_card_id: Option<serde_bytes::ByteBuf>,
        limit: u32,
    ) -> Bytes {
        use fauna_protocol::bridge_routing::QueryCardsRequest;
        let req = QueryCardsRequest {
            actor_id: actor_id.to_vec(),
            addressbook_id: addressbook_id.to_vec(),
            since_modseq,
            after_card_id,
            limit,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// S6.10 gate 1, card twin — see the calendar test.
    #[tokio::test]
    async fn query_cards_refuses_on_pure_backup_destination() {
        let state = fixture_state().await;
        let mda = [163u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [164u8; 32];
        state
            .db
            .create_folder_with_options(
                "__card",
                &target,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let payload = make_query_cards_payload(&target, &[165u8; 32], None, None, 0);
        let err = query_cards_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.pure_backup_destination");
    }

    #[tokio::test]
    async fn query_cards_unknown_addressbook_returns_addressbook_not_found() {
        let state = fixture_state().await;
        let mda = [110u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [111u8; 32];
        let book_id = [112u8; 32];

        let payload = make_query_cards_payload(&actor, &book_id, None, None, 0);
        let bytes = query_cards_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: fauna_protocol::bridge_routing::QueryCardsReply =
            fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(
            reply,
            fauna_protocol::bridge_routing::QueryCardsReply::AddressbookNotFound
        );
    }

    #[tokio::test]
    async fn query_cards_returns_single_card_after_put() {
        use fauna_protocol::bridge_routing::QueryCardsReply;

        let state = fixture_state().await;
        let mda = [113u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [114u8; 32];
        let book_id = [115u8; 32];
        let uid_hash = [116u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision address book.
        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place one card. After provision modseq=1; after PUT modseq=2.
        let outcome = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
                &uid_hash,
                b"encrypted-card-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let (expected_card_id, expected_etag, expected_modseq) = match outcome {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created {
                card_id,
                etag,
                modseq,
            } => (card_id, etag, modseq),
            other => panic!("expected Created, got {other:?}"),
        };

        // See the calendar twin: `place_carddav_card` writes the ROW only, and
        // post-cutover the serve path resolves a body exclusively through the
        // row's stored `record_cid`. Append the record from the same body+hint
        // so the derived cid matches the row's.
        crate::segments::card::append_record(
            &state.card_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"encrypted-card-body".to_vec(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"encrypted-index-hint".to_vec(),
            ),
            &fauna_contacts::segments::CardFloorMetadata {
                addressbook_id: book_id,
                card_id: expected_card_id,
                uid_hash: uid_hash.to_vec(),
                ciphertext_size: 20,
                internal_date: now_ts + 1,
                created_at: now_ts + 100,
                ..Default::default()
            },
        )
        .await
        .expect("append the row's content record");

        let payload = make_query_cards_payload(&actor, &book_id, None, None, 0);
        let bytes = query_cards_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: QueryCardsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            QueryCardsReply::Ok {
                cards,
                highestmodseq,
                more,
            } => {
                assert_eq!(cards.len(), 1, "one card expected");
                assert_eq!(cards[0].card_id, expected_card_id.to_vec());
                assert_eq!(cards[0].encrypted_body, b"encrypted-card-body".to_vec());
                assert_eq!(cards[0].uid_hash, uid_hash.to_vec());
                assert_eq!(cards[0].etag, expected_etag);
                assert_eq!(cards[0].modseq, expected_modseq);
                assert_eq!(highestmodseq, 2, "highestmodseq must be 2 after one PUT");
                assert!(!more, "more must be false");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_cards_since_modseq_filters() {
        use fauna_protocol::bridge_routing::QueryCardsReply;

        let state = fixture_state().await;
        let mda = [120u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [121u8; 32];
        let book_id = [122u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision address book (modseq=1 after provisioning).
        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place two cards. Card 1 → modseq=2, Card 2 → modseq=3.
        state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
                &[51u8; 32],
                b"encrypted-body-2",
                b"encrypted-hint-2",
                now_ts + 2,
                16,
                now_ts + 200,
            )
            .await
            .unwrap();
        let expected_card2_id = match outcome2 {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("expected Created, got {other:?}"),
        };

        // Query with since_modseq=2 → only card 2 (modseq=3).
        let payload = make_query_cards_payload(&actor, &book_id, Some(2), None, 0);
        let bytes = query_cards_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: QueryCardsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            QueryCardsReply::Ok { cards, .. } => {
                assert_eq!(cards.len(), 1, "only card with modseq > 2 expected");
                assert_eq!(cards[0].card_id, expected_card2_id.to_vec());
                assert_eq!(cards[0].modseq, 3);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_cards_pagination_with_after_card_id() {
        use fauna_protocol::bridge_routing::QueryCardsReply;

        let state = fixture_state().await;
        let mda = [130u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [131u8; 32];
        let book_id = [132u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision address book.
        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_id, b"sealed-meta", now_ts)
            .await
            .unwrap();

        // Place 3 cards with distinct timestamps and bodies so card_ids differ.
        let mut card_ids = Vec::new();
        for i in 0u8..3 {
            let outcome = state
                .db
                .place_carddav_card(
                    &actor,
                    &book_id,
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
                crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => {
                    card_ids.push(card_id);
                }
                other => panic!("expected Created, got {other:?}"),
            }
        }
        // Sort card_ids ascending (DB returns in card_id ASC order).
        card_ids.sort();

        // First page: limit=2 → expect 2 cards, more=true.
        let payload = make_query_cards_payload(&actor, &book_id, None, None, 2);
        let bytes = query_cards_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok page 1");
        let reply1: QueryCardsReply = fauna_cbor::decode_strict(&bytes).unwrap();

        let last_card_id = match reply1 {
            QueryCardsReply::Ok {
                ref cards, more, ..
            } => {
                assert_eq!(cards.len(), 2, "page 1 must have 2 cards");
                assert!(more, "more must be true after page 1");
                cards.last().unwrap().card_id.clone()
            }
            other => panic!("expected Ok, got {other:?}"),
        };

        // Second page: after_card_id = last_card_id, limit=2 → expect 1 card, more=false.
        let payload2 = make_query_cards_payload(
            &actor,
            &book_id,
            None,
            Some(serde_bytes::ByteBuf::from(last_card_id)),
            2,
        );
        let bytes2 = query_cards_handler()(state, mda, payload2)
            .await
            .expect("handler ok page 2");
        let reply2: QueryCardsReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            QueryCardsReply::Ok { cards, more, .. } => {
                assert_eq!(cards.len(), 1, "page 2 must have 1 card");
                assert!(!more, "more must be false on last page");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn query_cards_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [140u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [141u8; 32];
        let book_id = [142u8; 32];

        let payload = make_query_cards_payload(&actor, &book_id, None, None, 0);
        let err = query_cards_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_cards_user_other_actor_denied() {
        // Decision B: `User` may query its OWN address book's cards, but the
        // caller-scope guard denies querying ANOTHER actor's cards.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [143u8; 32];
        let other = [200u8; 32];
        let book_id = [144u8; 32];

        let payload = make_query_cards_payload(&other, &book_id, None, None, 0);
        let err = query_cards_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_cards_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [145u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];
        let book_id = [146u8; 32];

        let payload = make_query_cards_payload(&other, &book_id, None, None, 0);
        let err = query_cards_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn query_cards_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mda = [150u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryCardsRequest;
        let req = QueryCardsRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![2u8; 32],
            since_modseq: None,
            after_card_id: None,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_cards_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn query_cards_rejects_malformed_addressbook_id() {
        let state = fixture_state().await;
        let mda = [151u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryCardsRequest;
        let req = QueryCardsRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 16], // wrong length
            since_modseq: None,
            after_card_id: None,
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_cards_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn query_cards_rejects_malformed_after_card_id() {
        let state = fixture_state().await;
        let mda = [152u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        use fauna_protocol::bridge_routing::QueryCardsRequest;
        let req = QueryCardsRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            since_modseq: None,
            after_card_id: Some(serde_bytes::ByteBuf::from(vec![1u8; 16])), // wrong length
            limit: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = query_cards_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── put_card_ciphertext tests ─────────────────────────────────────

    /// Provision an address book; helper reused across multiple tests.
    async fn provision_one(
        state: &Arc<AppState>,
        mda: &[u8; 32],
        actor: &[u8; 32],
        book_id: &[u8; 32],
    ) {
        use fauna_protocol::bridge_routing::ProvisionAddressbookRequest;
        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        provision_addressbook_handler()(state.clone(), *mda, payload)
            .await
            .expect("provision_one: handler ok");
    }

    /// Seal `plaintext` as a genuine recipient envelope — the shape every
    /// production caller produces (Go MDA `EncryptToRecipientHybrid`; no
    /// Rust-client card writer exists yet, and when one lands it seals the
    /// same way calendar's `seal_event_body` does). S6.12 makes the PUT
    /// handler refuse anything else, so test bodies and hints must be real
    /// seals, not byte literals. NOT deterministic (HPKE encapsulation is
    /// randomized) — seal once and reuse the returned bytes when a test needs
    /// the same body twice (idempotency/retry paths).
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
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
        encrypted_body: &[u8],
        encrypted_index_hint: &[u8],
        timestamp: i64,
        if_match: Option<String>,
    ) -> Bytes {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;
        let req = PutCardCiphertextRequest {
            actor_id: actor_id.to_vec(),
            addressbook_id: addressbook_id.to_vec(),
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

    #[tokio::test]
    async fn put_card_ciphertext_unknown_addressbook_returns_addressbook_not_found() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [160u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [161u8; 32];
        let book_id = [162u8; 32];

        let payload = make_put_payload(
            &actor,
            &book_id,
            &[170u8; 32],
            &sealed(b"encrypted-body"),
            &sealed(b"encrypted-hint"),
            1_700_000_000,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, PutCardCiphertextReply::AddressbookNotFound);
    }

    /// The card twin of `bridge_caldav_handlers`'
    /// `an_events_bytes_count_toward_the_shared_storage_quota`
    /// (`caldav-server.md` § QUOTA — shared with IMAP): a contact's bytes draw
    /// on the one storage number, never on MESSAGE.
    #[tokio::test]
    async fn a_cards_bytes_count_toward_the_shared_storage_quota() {
        let state = fixture_state().await;
        let mda = [244u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [245u8; 32];
        let book_id = [246u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;
        let before = crate::bridge_imap_handlers::imap_quota_usage(&state, &actor)
            .await
            .expect("quota usage");
        assert_eq!(before, (0, 0), "a fresh address book holds nothing");

        let payload = make_put_payload(
            &actor,
            &book_id,
            &[247u8; 32],
            &sealed(&[b'c'; 500]),
            &sealed(b"hint"),
            1_700_000_000,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put ok");

        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        let cid = page.cards[0]
            .record_cid()
            .unwrap()
            .expect("row names its record");
        let sr = state
            .db
            .segment_records_lookup_record(&actor, crate::segments::card::KIND, &cid)
            .await
            .unwrap()
            .expect("mirror row");
        let size =
            crate::segments::record_sizes(&state.card_segments, &actor, &[(sr.segment_id, cid)])
                .await
                .unwrap()[0]
                .expect("real segment size");
        assert!(
            size > 500,
            "the record carries at least its sealed body: {size}"
        );
        assert_eq!(
            crate::bridge_imap_handlers::imap_quota_usage(&state, &actor)
                .await
                .expect("quota usage"),
            (size, 0),
            "the card's record counts toward STORAGE, never toward MESSAGE"
        );
    }

    /// The card twin of `bridge_caldav_handlers`'
    /// `an_event_put_past_the_storage_ceiling_is_refused_and_appends_nothing` and
    /// `a_shrinking_event_update_passes_even_over_the_ceiling`
    /// (`caldav-server.md` § QUOTA → § Enforcement points; `carddav-server.md`
    /// delegates card quota there): a new card past the ceiling is refused and
    /// writes nothing, while a shrinking update always passes.
    #[tokio::test]
    async fn a_card_put_past_the_storage_ceiling_is_refused_but_a_shrink_passes() {
        let state = fixture_state().await;
        let mda = [248u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [249u8; 32];
        let book_id = [250u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;
        let put = |uid: u8, body: Vec<u8>| {
            let payload = make_put_payload(
                &actor,
                &book_id,
                &[uid; 32],
                &body,
                &sealed(b"hint"),
                1_700_000_000,
                None,
            );
            put_card_ciphertext_handler()(state.clone(), mda, payload)
        };
        put(1, sealed(&[b'a'; 1200])).await.expect("the card fits");
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        let err = put(2, sealed(&[b'b'; 100]))
            .await
            .expect_err("a new card past the ceiling must be refused");
        assert_eq!(err.code, "fauna.bridges.over_quota");
        assert_eq!(
            state
                .db
                .segment_records_count_live(&actor, crate::segments::card::KIND)
                .await
                .unwrap(),
            1,
            "the refused body must never reach the segment"
        );
        put(1, sealed(&[b'c'; 400]))
            .await
            .expect("a shrinking update is never refused");
    }

    #[tokio::test]
    async fn put_card_ciphertext_new_card_returns_created() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [163u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [164u8; 32];
        let book_id = [165u8; 32];
        let uid_hash = [166u8; 32];
        // card_id derives from the body bytes, so bind the seal once.
        let encrypted_body = sealed(b"encrypted-vcard-v1");
        let encrypted_hint = sealed(b"encrypted-hint-v1");
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Expected card_id, via the same derivation the DB layer itself uses.
        let expected_card_id: Vec<u8> =
            derive_carddav_card_id(&actor, timestamp, &encrypted_body).to_vec();

        match reply {
            PutCardCiphertextReply::Created {
                card_id,
                etag,
                modseq,
            } => {
                assert_eq!(
                    card_id, expected_card_id,
                    "card_id must match blake3 derivation"
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
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1, "one card row must exist after Created");
        assert_eq!(page.cards[0].card_id.to_vec(), expected_card_id);
    }

    #[tokio::test]
    async fn put_card_ciphertext_update_same_uid_hash_returns_updated_with_tombstone() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [167u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [168u8; 32];
        let book_id = [169u8; 32];
        let uid_hash = [171u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // PUT first body.
        let payload1 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_card_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .expect("handler ok 1");
        let reply1: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let old_card_id = match reply1 {
            PutCardCiphertextReply::Created { card_id, .. } => card_id,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second different body to same uid_hash.
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            None,
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .expect("handler ok 2");
        let reply2: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutCardCiphertextReply::Updated {
                card_id,
                etag,
                modseq,
            } => {
                assert_ne!(
                    card_id, old_card_id,
                    "new card_id must differ (different body)"
                );
                assert_eq!(
                    modseq, 3,
                    "modseq must be 3 after provision(1)+PUT(2)+UPDATE(3)"
                );
                assert_eq!(etag, format!("{:016x}", 3));
            }
            other => panic!("expected Updated, got {other:?}"),
        }

        // Tombstone for old_card_id must exist.
        let expunged = state
            .db
            .query_carddav_expunged_since(&actor, &book_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 1, "one tombstone must exist after update");
        assert_eq!(
            expunged[0].card_id.to_vec(),
            old_card_id,
            "tombstone must be for the old card_id"
        );
    }

    #[tokio::test]
    async fn put_card_ciphertext_if_match_matching_returns_updated() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [172u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [173u8; 32];
        let book_id = [174u8; 32];
        let uid_hash = [175u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // PUT first card, capture etag.
        let payload1 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_card_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let first_etag = match reply1 {
            PutCardCiphertextReply::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second card with matching if_match.
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            Some(first_etag),
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        assert!(
            matches!(reply2, PutCardCiphertextReply::Updated { .. }),
            "matching if_match must return Updated, got {reply2:?}"
        );
    }

    #[tokio::test]
    async fn put_card_ciphertext_if_match_mismatch_returns_precondition_failed() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [176u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [177u8; 32];
        let book_id = [178u8; 32];
        let uid_hash = [179u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // PUT first card, capture its etag.
        let payload1 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v1"),
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes1 = put_card_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let first_etag = match reply1 {
            PutCardCiphertextReply::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // PUT second card with wrong if_match.
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-v2"),
            &sealed(b"encrypted-hint-v2"),
            timestamp + 1,
            Some("0000000000000000".to_string()), // wrong etag
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutCardCiphertextReply::PreconditionFailed { current_etag } => {
                assert_eq!(
                    current_etag, first_etag,
                    "current_etag must be the first card's etag"
                );
            }
            other => panic!("expected PreconditionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_card_ciphertext_transport_retry_same_body_returns_updated_no_bump() {
        use fauna_protocol::bridge_routing::PutCardCiphertextReply;

        let state = fixture_state().await;
        let mda = [180u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [181u8; 32];
        let book_id = [182u8; 32];
        let uid_hash = [183u8; 32];
        // Idempotent retry: same sealed bytes across both PUTs (bind once).
        let encrypted_body = sealed(b"encrypted-body-retry");
        let encrypted_hint = sealed(b"encrypted-hint-retry");
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // First PUT.
        let payload1 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes1 = put_card_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let (card_id_a, modseq_a) = match reply1 {
            PutCardCiphertextReply::Created {
                card_id, modseq, ..
            } => (card_id, modseq),
            other => panic!("expected Created, got {other:?}"),
        };

        // Retry with identical bytes — same uid_hash, same body, same timestamp.
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        let reply2: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();

        match reply2 {
            PutCardCiphertextReply::Updated {
                card_id,
                modseq,
                etag,
            } => {
                assert_eq!(card_id, card_id_a, "retry must return the same card_id");
                assert_eq!(modseq, modseq_a, "modseq must not bump on idempotent retry");
                assert_eq!(etag, format!("{:016x}", modseq_a));
            }
            other => panic!("expected Updated (idempotent retry), got {other:?}"),
        }

        // Exactly one card row.
        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.cards.len(),
            1,
            "must have exactly one card row after retry"
        );

        // No tombstones.
        let expunged = state
            .db
            .query_carddav_expunged_since(&actor, &book_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 0, "no tombstone on idempotent retry");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_size_mismatch() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [184u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"ten-bytes!".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 999, // wrong
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_empty_body() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [185u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: vec![],
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 0,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_empty_index_hint() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [186u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: vec![],
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_an_unsealed_body() {
        // S6.12 twin of the calendar gate: the RPC is `BridgeMda | User`, so
        // a User-class client can hand the nest arbitrary bytes — and since
        // S6.6 those bytes would go straight into the backup-eligible `__card`
        // segment store. A body that is not a sealed recipient envelope is
        // refused at the wire edge, BEFORE the segment append.
        let state = fixture_state().await;
        let mda = [200u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [201u8; 32];
        let book_id = [202u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;

        let raw_body = b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:u1\r\nFN:X\r\nEND:VCARD\r\n";
        let payload = make_put_payload(
            &actor,
            &book_id,
            &[203u8; 32],
            raw_body,
            &sealed(b"hint-tokens"),
            1_700_000_000,
            None,
        );
        let err = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Twin of the calendar assertion: post-cutover a record's id IS the
        // hash of its envelope bytes, so a record that was never built has no
        // id to look up — assert the actor's card mirror is empty instead.
        let records: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM segment_records WHERE scope_id = ?1 AND kind = ?2",
                rusqlite::params![actor.as_slice(), crate::segments::card::KIND],
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
    async fn put_card_ciphertext_rejects_an_unsealed_index_hint() {
        // Twin of the body gate: the hint is content-derived token bytes and
        // every production caller seals it — raw tokens at rest would leak
        // the card's word set. Same wire-edge refusal.
        let state = fixture_state().await;
        let mda = [204u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [205u8; 32];
        let book_id = [206u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_put_payload(
            &actor,
            &book_id,
            &[207u8; 32],
            &sealed(b"BEGIN:VCARD..."),
            b"raw-token-set-bytes",
            1_700_000_000,
            None,
        );
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_wrong_length_uid_hash() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [187u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 16], // wrong length
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_wrong_length_actor_id() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [188u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_rejects_wrong_length_addressbook_id() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [189u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = PutCardCiphertextRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 16], // wrong length
            uid_hash: vec![3u8; 32],
            encrypted_body: b"some-body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 9,
            if_match: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = put_card_ciphertext_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_card_ciphertext_bridge_mta_class_denied() {
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
        let err = put_card_ciphertext_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn put_card_ciphertext_user_other_actor_denied() {
        // Decision B: `User` may write a card to its OWN address book (the
        // client seals locally), but the caller-scope guard denies writing
        // under ANOTHER actor's id — without it a user could forge cards into
        // a foreign address book.
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
        let err = put_card_ciphertext_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── delete_card tests ─────────────────────────────────────────────

    fn make_delete_payload(
        actor_id: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<String>,
    ) -> Bytes {
        let req = DeleteCardRequest {
            actor_id: actor_id.to_vec(),
            addressbook_id: addressbook_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            if_match,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn delete_card_missing_addressbook_returns_not_found() {
        let state = fixture_state().await;
        let mda = [200u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [201u8; 32];
        let book_id = [202u8; 32];
        let uid_hash = [203u8; 32];

        // No address book provisioned — expect NotFound.
        let payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        let bytes = delete_card_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteCardReply::NotFound);
    }

    #[tokio::test]
    async fn delete_card_missing_card_returns_not_found() {
        let state = fixture_state().await;
        let mda = [204u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [205u8; 32];
        let book_id = [206u8; 32];
        let uid_hash = [207u8; 32];

        // Provision the address book but place no cards.
        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        let bytes = delete_card_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteCardReply::NotFound);
    }

    #[tokio::test]
    async fn delete_card_success_returns_deleted_with_tombstone() {
        let state = fixture_state().await;
        let mda = [208u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [209u8; 32];
        let book_id = [210u8; 32];
        let uid_hash = [211u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq baseline = 1).
        provision_one(&state, &mda, &actor, &book_id).await;

        // Place one card (modseq = 2 after placement).
        let place_outcome = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
                &uid_hash,
                b"encrypted-card-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let (placed_card_id, placed_etag) = match place_outcome {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created {
                card_id,
                etag,
                modseq,
            } => {
                assert_eq!(modseq, 2, "modseq after place must be 2");
                (card_id, etag)
            }
            other => panic!("expected Created, got {other:?}"),
        };
        let _ = placed_etag; // not needed for this test

        // Delete the card (modseq = 3 after delete).
        let payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        let bytes = delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            DeleteCardReply::Deleted { card_id, modseq } => {
                assert_eq!(
                    card_id,
                    placed_card_id.to_vec(),
                    "card_id must match placed row"
                );
                assert_eq!(
                    modseq, 3,
                    "modseq must be 3 after provision(1)+place(2)+delete(3)"
                );
            }
            other => panic!("expected Deleted, got {other:?}"),
        }

        // Card row must be gone.
        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        assert!(
            page.cards.is_empty(),
            "card row must be removed after delete"
        );

        // Tombstone must exist with correct fields.
        let expunged = state
            .db
            .query_carddav_expunged_since(&actor, &book_id, 0)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 1, "one tombstone expected");
        assert_eq!(
            expunged[0].card_id.to_vec(),
            placed_card_id.to_vec(),
            "tombstone card_id must match deleted row"
        );
        assert_eq!(
            expunged[0].uid_hash,
            uid_hash.to_vec(),
            "tombstone uid_hash must match"
        );
        assert_eq!(expunged[0].modseq, 3, "tombstone modseq must be 3");
    }

    #[tokio::test]
    async fn delete_card_if_match_mismatch_returns_precondition_failed() {
        let state = fixture_state().await;
        let mda = [212u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [213u8; 32];
        let book_id = [214u8; 32];
        let uid_hash = [215u8; 32];
        let now_ts = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // Place card; etag = format!("{:016x}", 2) = "0000000000000002".
        let place_outcome = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
                &uid_hash,
                b"encrypted-card-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let placed_etag = match place_outcome {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };
        assert_eq!(placed_etag, "0000000000000002");

        // Delete with wrong if_match — should return PreconditionFailed.
        let payload = make_delete_payload(
            &actor,
            &book_id,
            &uid_hash,
            Some("0000000000000000".to_string()),
        );
        let bytes = delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            DeleteCardReply::PreconditionFailed { current_etag } => {
                assert_eq!(
                    current_etag, placed_etag,
                    "current_etag must be the placed card's etag"
                );
            }
            other => panic!("expected PreconditionFailed, got {other:?}"),
        }

        // Card row must still exist.
        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.cards.len(),
            1,
            "card row must survive if_match mismatch"
        );
    }

    #[tokio::test]
    async fn delete_card_if_match_matching_returns_deleted() {
        let state = fixture_state().await;
        let mda = [216u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [217u8; 32];
        let book_id = [218u8; 32];
        let uid_hash = [219u8; 32];
        let now_ts = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        let place_outcome = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
                &uid_hash,
                b"encrypted-card-body",
                b"encrypted-index-hint",
                now_ts + 1,
                20,
                now_ts + 100,
            )
            .await
            .unwrap();
        let placed_etag = match place_outcome {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created, got {other:?}"),
        };

        // Delete with matching if_match — should return Deleted.
        let payload = make_delete_payload(&actor, &book_id, &uid_hash, Some(placed_etag));
        let bytes = delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();

        assert!(
            matches!(reply, DeleteCardReply::Deleted { .. }),
            "matching if_match must return Deleted, got {reply:?}"
        );

        // Card row must be gone.
        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 0)
            .await
            .unwrap();
        assert!(
            page.cards.is_empty(),
            "card row must be removed after delete with matching if_match"
        );
    }

    #[tokio::test]
    async fn delete_card_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [220u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_delete_payload(&[1u8; 32], &[2u8; 32], &[3u8; 32], None);
        let err = delete_card_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_card_user_other_actor_denied() {
        // Decision B: `User` may delete cards from its OWN address book, but the
        // caller-scope guard denies deleting from ANOTHER actor's address book.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [221u8; 32];
        let other = [200u8; 32];

        let payload = make_delete_payload(&other, &[2u8; 32], &[3u8; 32], None);
        let err = delete_card_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_card_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [222u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let payload = make_delete_payload(&other, &[2u8; 32], &[3u8; 32], None);
        let err = delete_card_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_card_rejects_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [223u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteCardRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_card_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn delete_card_rejects_wrong_length_addressbook_id() {
        let state = fixture_state().await;
        let mda = [224u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteCardRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 16], // wrong length
            uid_hash: vec![3u8; 32],
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_card_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn delete_card_rejects_wrong_length_uid_hash() {
        let state = fixture_state().await;
        let mda = [225u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteCardRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 32],
            uid_hash: vec![3u8; 16], // wrong length
            if_match: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_card_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── delete_addressbook tests ──────────────────────────────────────

    fn make_delete_addressbook_payload(actor_id: &[u8; 32], addressbook_id: &[u8; 32]) -> Bytes {
        let req = DeleteAddressbookRequest {
            actor_id: actor_id.to_vec(),
            addressbook_id: addressbook_id.to_vec(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn delete_addressbook_missing_returns_not_found() {
        let state = fixture_state().await;
        let mda = [230u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [231u8; 32];
        let book_id = [232u8; 32];

        // No address book provisioned — idempotent NotFound.
        let payload = make_delete_addressbook_payload(&actor, &book_id);
        let bytes = delete_addressbook_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteAddressbookReply::NotFound);
    }

    #[tokio::test]
    async fn delete_addressbook_success_cascades_and_returns_count() {
        let state = fixture_state().await;
        let mda = [233u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [234u8; 32];
        let book_id = [235u8; 32];
        let now_ts = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;
        // Place two cards.
        for (i, body) in [b"card-body-1", b"card-body-2"].iter().enumerate() {
            state
                .db
                .place_carddav_card(
                    &actor,
                    &book_id,
                    &[240 + i as u8; 32],
                    *body,
                    b"hint",
                    now_ts + 1 + i as i64,
                    body.len() as u32,
                    now_ts + 100,
                )
                .await
                .unwrap();
        }

        let payload = make_delete_addressbook_payload(&actor, &book_id);
        let bytes = delete_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteAddressbookReply::Deleted { cards_deleted: 2 });

        // A second delete is idempotent NotFound (book row gone).
        let payload = make_delete_addressbook_payload(&actor, &book_id);
        let bytes = delete_addressbook_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteAddressbookReply::NotFound);
    }

    #[tokio::test]
    async fn delete_addressbook_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [236u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_delete_addressbook_payload(&[1u8; 32], &[2u8; 32]);
        let err = delete_addressbook_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_addressbook_user_other_actor_denied() {
        // `User` may delete its OWN book, but the caller-scope guard denies
        // deleting ANOTHER actor's address book.
        let state = fixture_state().await;
        let user_actor = [237u8; 32];
        let other = [200u8; 32];

        let payload = make_delete_addressbook_payload(&other, &[2u8; 32]);
        let err = delete_addressbook_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn delete_addressbook_rejects_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [238u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteAddressbookRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![2u8; 32],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_addressbook_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn delete_addressbook_rejects_wrong_length_addressbook_id() {
        let state = fixture_state().await;
        let mda = [239u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = DeleteAddressbookRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 16], // wrong length
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_addressbook_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── sync_addressbook_since tests ──────────────────────────────────

    fn make_sync_payload(
        actor_id: &[u8; 32],
        addressbook_id: &[u8; 32],
        sync_token: &str,
        limit: u32,
    ) -> Bytes {
        let req = SyncAddressbookSinceRequest {
            actor_id: actor_id.to_vec(),
            addressbook_id: addressbook_id.to_vec(),
            sync_token: sync_token.to_string(),
            limit,
            ..Default::default()
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    #[tokio::test]
    async fn sync_addressbook_since_unknown_addressbook_returns_addressbook_not_found() {
        let state = fixture_state().await;
        let mda = [230u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [231u8; 32];
        let book_id = [232u8; 32];

        // No address book provisioned — expect AddressbookNotFound.
        let payload = make_sync_payload(&actor, &book_id, "0", 0);
        let bytes = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, SyncAddressbookSinceReply::AddressbookNotFound);
    }

    #[tokio::test]
    async fn sync_addressbook_since_fresh_addressbook_returns_all_changed_no_expunged() {
        let state = fixture_state().await;
        let mda = [233u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [234u8; 32];
        let book_id = [235u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision address book (modseq baseline = 1).
        provision_one(&state, &mda, &actor, &book_id).await;

        // Place 3 cards (modseqs 2, 3, 4; highestmodseq = 4).
        let uid_a = [240u8; 32];
        let uid_b = [241u8; 32];
        let uid_c = [242u8; 32];
        let place_a = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
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
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };
        let id_b = match place_b {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };
        let id_c = match place_c {
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };

        // Full sync from "0", unbounded.
        let payload = make_sync_payload(&actor, &book_id, "0", 0);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncAddressbookSinceReply::Ok {
                changed,
                expunged,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(changed.len(), 3, "full sync must return all 3 cards");
                assert!(expunged.is_empty(), "no tombstones on a fresh address book");
                assert_eq!(
                    new_sync_token, "4",
                    "provision(1)+3 places = highestmodseq 4"
                );
                assert!(!more, "unbounded limit: more must be false");
                // Verify card_ids match.
                let returned_ids: Vec<Vec<u8>> =
                    changed.iter().map(|e| e.card_id.clone()).collect();
                assert!(
                    returned_ids.contains(&id_a.to_vec()),
                    "card A must be present"
                );
                assert!(
                    returned_ids.contains(&id_b.to_vec()),
                    "card B must be present"
                );
                assert!(
                    returned_ids.contains(&id_c.to_vec()),
                    "card C must be present"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_addressbook_since_after_put_and_delete_returns_both_signals() {
        let state = fixture_state().await;
        let mda = [243u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [244u8; 32];
        let book_id = [245u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq 1).
        provision_one(&state, &mda, &actor, &book_id).await;

        // Place 2 cards (modseqs 2 + 3).
        let uid_keep = [250u8; 32];
        let uid_del = [251u8; 32];
        let place_keep = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
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
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };
        let _ = place_keep;

        // Capture the sync_token after the two placements (highestmodseq = 3).
        let payload = make_sync_payload(&actor, &book_id, "0", 0);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let first_reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let baseline_token = match first_reply {
            SyncAddressbookSinceReply::Ok { new_sync_token, .. } => new_sync_token,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(
            baseline_token, "3",
            "after provision(1)+2 places: highestmodseq=3"
        );

        // Place 1 new card (modseq 4).
        let uid_new = [252u8; 32];
        let place_new = state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };

        // Delete one existing card (modseq 5). Use a wall-clock-recent
        // expunged_at so the tombstone stays within the retention window the
        // handler checks against `now` — the fixed `now_ts` (2023) base would
        // otherwise read as past-retention and trip the Ok { stale: true }
        // signal, masking this test's both-signals intent.
        let del_now = now_epoch_secs();
        state
            .db
            .delete_carddav_card_by_uid(&actor, &book_id, &uid_del, None, del_now)
            .await
            .unwrap();

        // Incremental sync from baseline_token (= "3").
        let payload = make_sync_payload(&actor, &book_id, &baseline_token, 0);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncAddressbookSinceReply::Ok {
                changed,
                expunged,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(
                    changed.len(),
                    1,
                    "only the new card (modseq 4) should appear"
                );
                assert_eq!(expunged.len(), 1, "deleted card should appear as tombstone");
                assert_eq!(
                    new_sync_token, "5",
                    "provision(1)+2 places+1 new+1 delete = highestmodseq 5"
                );
                assert!(!more, "unbounded limit: more must be false");

                assert_eq!(
                    changed[0].card_id,
                    id_new.to_vec(),
                    "changed entry must be the newly placed card"
                );
                assert_eq!(
                    expunged[0].card_id,
                    id_del.to_vec(),
                    "expunged entry card_id must match deleted card"
                );
                assert_eq!(
                    expunged[0].uid_hash,
                    uid_del.to_vec(),
                    "expunged entry uid_hash must match deleted card's uid"
                );
                assert_eq!(expunged[0].modseq, 5, "tombstone modseq must be 5");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_addressbook_since_rejects_malformed_sync_token_not_numeric() {
        let state = fixture_state().await;
        let mda = [253u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "abc", 0);
        let err = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_addressbook_since_rejects_malformed_sync_token_negative() {
        let state = fixture_state().await;
        let mda = [254u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "-1", 0);
        let err = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_addressbook_since_rejects_malformed_sync_token_empty() {
        let state = fixture_state().await;
        // Use a new byte value — 255 is u8::MAX, valid.
        let mda = [255u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "", 0);
        let err = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_addressbook_since_bridge_mta_class_denied() {
        let state = fixture_state().await;
        let mta = [60u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let payload = make_sync_payload(&[1u8; 32], &[2u8; 32], "0", 0);
        let err = sync_addressbook_since_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_addressbook_since_user_other_actor_denied() {
        // Decision B: `User` may sync its OWN address book, but the
        // caller-scope guard denies syncing ANOTHER actor's address book.
        let state = fixture_state().await;
        // No bridge row, no admin → User class.
        let user_actor = [61u8; 32];
        let other = [200u8; 32];

        let payload = make_sync_payload(&other, &[2u8; 32], "0", 0);
        let err = sync_addressbook_since_handler()(state, user_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_addressbook_since_admin_other_actor_denied() {
        let state = fixture_state().await;
        let admin_actor = [62u8; 32];
        state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
        let other = [201u8; 32];

        let payload = make_sync_payload(&other, &[2u8; 32], "0", 0);
        let err = sync_addressbook_since_handler()(state, admin_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn sync_addressbook_since_rejects_wrong_length_actor_id() {
        let state = fixture_state().await;
        let mda = [63u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SyncAddressbookSinceRequest {
            actor_id: vec![1u8; 16], // wrong length
            addressbook_id: vec![2u8; 32],
            sync_token: "0".to_string(),
            limit: 0,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_addressbook_since_rejects_wrong_length_addressbook_id() {
        let state = fixture_state().await;
        let mda = [64u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = SyncAddressbookSinceRequest {
            actor_id: vec![1u8; 32],
            addressbook_id: vec![2u8; 16], // wrong length
            sync_token: "0".to_string(),
            limit: 0,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = sync_addressbook_since_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn sync_addressbook_since_paginates_correctly_when_more_true_resumes_from_last_returned_modseq()
     {
        // Guards against the partial-page bug: with the pre-fix code,
        // new_sync_token = hms (address-book-wide highestmodseq) regardless of
        // `more`. A second paged call passing that token would use
        // `modseq > hms` as the filter, which skips every card in the
        // [last_returned_modseq+1..hms] window — i.e. card #3 gets silently
        // dropped. This test confirms the second call correctly returns it.
        //
        // Also confirms cards are returned in modseq ASC order (not
        // card_id ASC), so the correct card is deferred to page 2.
        let state = fixture_state().await;
        let mda = [65u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [66u8; 32];
        let book_id = [67u8; 32];
        let now_ts = 1_700_000_000i64;

        // Provision (modseq 1), then place 3 cards: modseqs 2, 3, 4.
        provision_one(&state, &mda, &actor, &book_id).await;

        let uid_a = [70u8; 32];
        let uid_b = [71u8; 32];
        let uid_c = [72u8; 32];
        state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
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
            .place_carddav_card(
                &actor,
                &book_id,
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
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("unexpected {other:?}"),
        };

        // highestmodseq is now 4.

        // ── Page 1: sync from "0" with limit=2 ──────────────────────────────
        let payload = make_sync_payload(&actor, &book_id, "0", 2);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok - page 1");
        let reply1: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        let (changed1, token1, more1) = match reply1 {
            SyncAddressbookSinceReply::Ok {
                changed,
                new_sync_token,
                more,
                ..
            } => (changed, new_sync_token, more),
            other => panic!("expected Ok on page 1, got {other:?}"),
        };

        assert_eq!(changed1.len(), 2, "page 1 must return exactly 2 cards");
        assert!(more1, "page 1 must signal more == true");
        // new_sync_token must be the modseq of the LAST returned card (3),
        // NOT the address-book-wide highestmodseq (4). With the pre-fix code
        // this would be "4" and the second call would return empty.
        assert_eq!(
            token1, "3",
            "when more==true, new_sync_token must be modseq of last returned card (3), not hms (4)"
        );

        // ── Page 2: resume from the returned token ───────────────────────────
        let payload = make_sync_payload(&actor, &book_id, &token1, 2);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok - page 2");
        let reply2: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply2 {
            SyncAddressbookSinceReply::Ok {
                changed,
                new_sync_token,
                more,
                ..
            } => {
                assert_eq!(
                    changed.len(),
                    1,
                    "page 2 must return the remaining 1 card (modseq 4)"
                );
                assert!(!more, "page 2 must signal more == false");
                assert_eq!(
                    new_sync_token, "4",
                    "when more==false, new_sync_token must be address-book-wide hms (4)"
                );
                assert_eq!(
                    changed[0].card_id,
                    id_c.to_vec(),
                    "the deferred card must be card C (modseq 4, the largest modseq)"
                );
            }
            other => panic!("expected Ok on page 2, got {other:?}"),
        }
    }

    // ── restore-divergence tests ────────────────────────────────────────

    /// When the client's sync_token is strictly ahead of the address book's
    /// highestmodseq (the post-DR-restore "MUA ahead" case), the handler must:
    ///   1. Write one `bridge_restore_divergence` row keyed to the most recent
    ///      `restore_history` row for the actor.
    ///   2. Return `SyncAddressbookSinceReply::Stale { server_modseq }` rather
    ///      than the silent zero-rows Ok reply.
    #[tokio::test]
    async fn sync_addressbook_since_returns_stale_when_token_ahead() {
        let state = fixture_state().await;
        let mda = [0x79u8; 32];
        let actor = [0x78u8; 32];
        let book_id = [0x7au8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Provision the address book (highestmodseq starts at 1).
        provision_one(&state, &mda, &actor, &book_id).await;

        // Verify hms is 1 so we know what "ahead" means.
        let hms = state
            .db
            .carddav_addressbook_highestmodseq(&actor, &book_id)
            .await
            .unwrap()
            .expect("address book exists");
        assert_eq!(hms, 1, "freshly provisioned address book hms must be 1");

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
            .insert_restore_history(&actor, snap_id, "addressbook", None)
            .await
            .expect("insert_restore_history");

        // Call the handler with sync_token="5" — ahead of hms=1, so Stale.
        let req = SyncAddressbookSinceRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            sync_token: "5".to_string(),
            limit: 0,
            mua_id: Some("Apple Contacts/14.0".into()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        // Must be Stale with server_modseq == hms (1).
        match reply {
            SyncAddressbookSinceReply::Stale { server_modseq } => {
                assert_eq!(
                    server_modseq, 1,
                    "Stale server_modseq must equal address book hms"
                );
            }
            other => panic!("expected SyncAddressbookSinceReply::Stale, got {other:?}"),
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
            Some("Apple Contacts/14.0"),
            "mua_id must be forwarded from the request"
        );
    }

    #[tokio::test]
    async fn sync_addressbook_since_returns_stale_ok_when_token_past_retention() {
        // A valid (token <= hms) but past-retention sync-token: a tombstone
        // newer than the token was expunged before the retention cutoff, so
        // nest signals Ok { stale: true } (carddav-server.md § Stale sync-token
        // handling) and writes NO divergence row (retention expiry is normal,
        // not a restore anomaly — that distinguishes it from the Stale arm).
        let state = fixture_state().await;
        let mda = [0x7bu8; 32];
        let actor = [0x7cu8; 32];
        let book_id = [0x7du8; 32];

        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        provision_one(&state, &mda, &actor, &book_id).await; // modseq 1

        // Place (modseq 2) then expunge (modseq 3) one card, backdating the
        // tombstone's expunged_at to 31 days ago — past the 30-day retention
        // window the handler reads from the effective ImapPolicy (no
        // put_imap_policy override set in this test → catalog default 30).
        let uid = [0x7eu8; 32];
        state
            .db
            .place_carddav_card(
                &actor,
                &book_id,
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
            .delete_carddav_card_by_uid(&actor, &book_id, &uid, None, aged_expunged_at)
            .await
            .expect("delete");
        let tombstone_modseq = match del {
            crate::db::bridge_carddav::DeleteCarddavCardOutcome::Deleted { modseq, .. } => modseq,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let hms = state
            .db
            .carddav_addressbook_highestmodseq(&actor, &book_id)
            .await
            .unwrap()
            .expect("address book exists");
        assert!(
            tombstone_modseq <= hms,
            "tombstone modseq {tombstone_modseq} must be <= hms {hms}"
        );

        // sync_token "1": behind the tombstone (3) and <= hms, so not the
        // MUA-ahead Stale branch — the retention branch must fire.
        let payload = make_sync_payload(&actor, &book_id, "1", 0);
        let bytes = sync_addressbook_since_handler()(state.clone(), mda, payload)
            .await
            .expect("handler must not error");
        let reply: SyncAddressbookSinceReply = fauna_cbor::decode_strict(&bytes).unwrap();

        match reply {
            SyncAddressbookSinceReply::Ok {
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
    async fn carddav_user_own_actor_full_roundtrip() {
        // Decision B end-to-end (carddav-server.md § Persistence): a Fauna app —
        // User class (no bridge row, no admin row) — provisions its OWN
        // address book, writes a card (sealed client-side; nest sees only
        // ciphertext), then reads it back via list_addressbooks + query_cards.
        // This is the direct client path. The caller-scope guard lets the user
        // reach exactly its own data and nothing else.
        let state = fixture_state().await;
        let user_actor = [42u8; 32];
        let book_id = [43u8; 32];
        state
            .db
            .create_user(&user_actor, "free", "test")
            .await
            .unwrap();

        // 1. provision_addressbook (BridgeMda | User, own actor → Created).
        let prov = ProvisionAddressbookRequest {
            actor_id: user_actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"sealed-personal-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&prov).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), user_actor, payload)
            .await
            .expect("a user must be able to provision its own address book");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        // 2. put_card_ciphertext (own actor → Created).
        let uid_hash = [7u8; 32];
        let body = sealed(b"sealed-vcard-bytes");
        let put = make_put_payload(
            &user_actor,
            &book_id,
            &uid_hash,
            &body,
            &sealed(b"sealed-hint"),
            1_700_000_000,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), user_actor, put)
            .await
            .expect("a user must be able to write a card to its own address book");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutCardCiphertextReply::Created { .. }),
            "first write is Created, got {reply:?}"
        );

        // 3. list_addressbooks (own actor) shows the book with card_count == 1.
        let bytes =
            list_addressbooks_handler()(state.clone(), user_actor, make_list_payload(&user_actor))
                .await
                .expect("a user must be able to list its own address books");
        let reply: ListAddressbooksReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.addressbooks.len(), 1);
        assert_eq!(reply.addressbooks[0].addressbook_id, book_id.to_vec());
        assert_eq!(reply.addressbooks[0].card_count, 1);

        // 4. query_cards (own actor) returns the sealed body verbatim.
        let q = make_query_cards_payload(&user_actor, &book_id, None, None, 0);
        let bytes = query_cards_handler()(state.clone(), user_actor, q)
            .await
            .expect("a user must be able to query its own address book's cards");
        let reply: QueryCardsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        match reply {
            QueryCardsReply::Ok { cards, .. } => {
                assert_eq!(cards.len(), 1, "the one card we wrote");
                assert_eq!(
                    cards[0].encrypted_body,
                    body.to_vec(),
                    "nest returns the sealed body opaquely"
                );
                assert_eq!(cards[0].uid_hash, uid_hash.to_vec());
            }
            other => panic!("expected Ok with one card, got {other:?}"),
        }
    }

    /// Slice 2 (deployment-home-with-public-relay.md § MUA reach): the per-actor
    /// serving opt-out gates ONLY the MDA-serving path. After actor A disables
    /// serving, the MDA is refused (`mail_serving_disabled`), but A's OWN Fauna
    /// app (User class, `target == caller`) still reads its address book — the
    /// flag governs where the MDA serves external CardDAV clients, not whether the
    /// user can read their own data (carddav-server.md Decision B).
    #[tokio::test]
    async fn carddav_serving_optout_gates_mda_path_only() {
        let state = fixture_state().await;
        let mda = [12u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let user_a = [42u8; 32];
        let book_id = [43u8; 32];
        state.db.create_user(&user_a, "free", "test").await.unwrap();

        // A provisions its own address book (User path — ungated).
        let prov = ProvisionAddressbookRequest {
            actor_id: user_a.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"sealed-meta".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&prov).unwrap().to_vec());
        provision_addressbook_handler()(state.clone(), user_a, payload)
            .await
            .expect("user provisions own address book");

        // While serving is on (default), the MDA may serve A.
        let q = make_query_cards_payload(&user_a, &book_id, None, None, 0);
        query_cards_handler()(state.clone(), mda, q)
            .await
            .expect("MDA serves A while serving is on");

        // A opts OUT of serving on this nest.
        state
            .db
            .set_actor_mail_serving_enabled(&user_a, false)
            .await
            .unwrap();

        // MDA-serving path → rejected with the distinct serving-disabled code.
        let q = make_query_cards_payload(&user_a, &book_id, None, None, 0);
        let err = query_cards_handler()(state.clone(), mda, q)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.mail_serving_disabled");

        // A's OWN client (User class, target == caller) is NEVER gated by the
        // serving flag — it still reads its own address book.
        let q = make_query_cards_payload(&user_a, &book_id, None, None, 0);
        query_cards_handler()(state.clone(), user_a, q)
            .await
            .expect("A's own client still reads its address book after disabling MDA serving");
    }

    // ── S6.3 placement-journal wiring tests (twin of caldav T10) ───────────
    //
    // The four CardDAV state-changing handlers — `provision_addressbook`,
    // `put_card_ciphertext`, `delete_card`, `delete_addressbook` — must
    // append the matching `CardPlacementRecord` to `state.card_placement`
    // *after* their SQLite mutation commits. These are the exact twins of the
    // CalDAV `provision_calendar` / `put_event_ciphertext` / `delete_event`
    // wiring tests in `bridge_caldav_handlers.rs`, plus the card-only
    // `delete_addressbook` (CalDAV has no delete-calendar RPC). Emission gates
    // on the mutating outcome variants only (Created/Updated/Deleted) — the DB
    // layer signals byte-identical retries as no-ops, and the journal mirrors
    // that. Actor IDs `[70u8..79u8; 32]` to stay clear of every previously
    // claimed range. Spec: design § D7; `carddav-server.md` § Storage model →
    // Durability & disaster recovery.

    #[tokio::test]
    async fn provision_addressbook_created_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [70u8; 32];
        let book_id = [101u8; 32];
        let meta = b"sealed-addressbook-metadata-v1".to_vec();

        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: meta.clone(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.addressbooks.len(),
            1,
            "ProvisionAddressbook record must land in the manifest's addressbooks Vec",
        );
        assert_eq!(manifest.addressbooks[0].addressbook_id, book_id);
        assert_eq!(manifest.addressbooks[0].encrypted_metadata, meta);
    }

    #[tokio::test]
    async fn provision_addressbook_already_exists_appends_nothing() {
        // Idempotent re-provision (identical bytes) must NOT emit a second
        // ProvisionAddressbook record. The DB layer absorbs the duplicate via
        // its identical-bytes check; the journal mirrors that no-op.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [71u8; 32];
        let book_id = [102u8; 32];
        let meta = b"identical-metadata".to_vec();

        let make_payload = || {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: meta.clone(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First call → Created (emits one record).
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Created);

        let manifest_after_first = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest after first");
        assert_eq!(manifest_after_first.addressbooks.len(), 1);

        // Second call with identical bytes → AlreadyExists (no record emitted).
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::AlreadyExists);

        let manifest_after_second = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest after second");

        // Full manifest equality — `kind_manifest.next_seg_id` would advance
        // if a duplicate ProvisionAddressbook record had been appended.
        assert_eq!(
            manifest_after_second, manifest_after_first,
            "idempotent re-provision must not emit a duplicate ProvisionAddressbook record",
        );
    }

    #[tokio::test]
    async fn provision_addressbook_update_metadata_appends_placement_record() {
        // PROPPATCH path: update_metadata=true with new bytes must emit one
        // CardPlacementRecord::UpdateAddressbookMetadata record.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [72u8; 32];
        let book_id = [103u8; 32];

        // Provision (MKCOL) — provision_one seals b"sealed-meta".
        provision_one(&state, &mda, &actor, &book_id).await;
        let manifest_after_provision = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(manifest_after_provision.addressbooks.len(), 1);
        let hms_after_provision = manifest_after_provision.addressbooks[0].highestmodseq;

        // PROPPATCH (update_metadata=true) with different bytes.
        let req = ProvisionAddressbookRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            encrypted_metadata: b"resealed-meta-v2".to_vec(),
            update_metadata: true,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = provision_addressbook_handler()(state.clone(), mda, payload)
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Updated);

        let manifest_after_update = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            manifest_after_update.addressbooks.len(),
            1,
            "manifest carries exactly one AddressbookState for this address book",
        );
        assert_eq!(
            manifest_after_update.addressbooks[0].encrypted_metadata, b"resealed-meta-v2",
            "manifest must carry the new metadata after UpdateAddressbookMetadata apply",
        );
        assert!(
            manifest_after_update.addressbooks[0].highestmodseq > hms_after_provision,
            "manifest's highestmodseq must bump (was {}, now {})",
            hms_after_provision,
            manifest_after_update.addressbooks[0].highestmodseq,
        );
    }

    #[tokio::test]
    async fn provision_addressbook_update_metadata_byte_identical_appends_nothing() {
        // Byte-identical PROPPATCH retry: the DB layer returns Updated without
        // bumping modseq; the journal mirrors the no-op (no duplicate
        // UpdateAddressbookMetadata record).
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [73u8; 32];
        let book_id = [104u8; 32];

        provision_one(&state, &mda, &actor, &book_id).await;

        let make_update_payload = || {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: b"identical-resealed-meta".to_vec(),
                update_metadata: true,
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // First PROPPATCH → Updated (emits one record).
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_update_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Updated);

        let manifest_after_first = state.card_placement.current_manifest(&actor).await.unwrap();

        // Second PROPPATCH with identical bytes → Updated reply, no journal change.
        let bytes = provision_addressbook_handler()(state.clone(), mda, make_update_payload())
            .await
            .unwrap();
        let reply: ProvisionAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, ProvisionAddressbookReply::Updated);

        let manifest_after_second = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            manifest_after_second, manifest_after_first,
            "byte-identical PROPPATCH retry must not emit a duplicate UpdateAddressbookMetadata record",
        );
    }

    #[tokio::test]
    async fn put_card_ciphertext_created_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [74u8; 32];
        let book_id = [105u8; 32];
        let uid_hash = [106u8; 32];
        let timestamp = 1_700_000_000i64;
        let encrypted_body = sealed(b"encrypted-vcard-body-v1");

        // Provision the address book first (emits a ProvisionAddressbook record).
        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &sealed(b"encrypted-hint-v1"),
            timestamp,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let (expected_etag, expected_modseq) = match reply {
            PutCardCiphertextReply::Created { etag, modseq, .. } => (etag, modseq as u64),
            other => panic!("expected Created, got {other:?}"),
        };

        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.cards.len(),
            1,
            "PutCard record must land in the manifest's cards Vec",
        );
        let placement = &manifest.cards[0];
        assert_eq!(placement.addressbook_id, book_id);
        assert_eq!(placement.uid_hash, uid_hash);
        assert_eq!(placement.etag, expected_etag);
        assert_eq!(placement.modseq, expected_modseq);
        assert_eq!(placement.ciphertext_size, encrypted_body.len() as u32);
    }

    /// S6.9 v2 journal, card twin: the PutCard record carries the
    /// content-record id and the row's EFFECTIVE sidecar (the MUA-preserve
    /// path — see the calendar twin for the full argument).
    #[tokio::test]
    async fn put_card_journals_the_content_record_id_and_effective_sidecar() {
        use fauna_protocol::bridge_routing::PutCardCiphertextRequest;

        let state = fixture_state().await;
        let mda = [160u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [169u8; 32];
        let book_id = [213u8; 32];
        let uid_hash = [214u8; 32];
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &book_id).await;

        let body1 = sealed(b"vcard-v1");
        // Struct-update fixture on a growing wire type: keep
        // `..Default::default()` even while it's a no-op, so concurrent
        // field-adds merge cleanly instead of colliding on the grown axis.
        #[allow(clippy::needless_update)]
        let req = PutCardCiphertextRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body: body1.clone(),
            encrypted_index_hint: sealed(b"hint-v1"),
            timestamp,
            ciphertext_size: body1.len() as u32,
            if_match: None,
            encrypted_fauna_ext: Some(b"fauna-sidecar".to_vec()),
            ..Default::default()
        };
        let bytes = put_card_ciphertext_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("fauna put ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let created_card_id = match reply {
            PutCardCiphertextReply::Created { card_id, .. } => card_id,
            other => panic!("expected Created, got {other:?}"),
        };

        let m = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(
            m.cards[0].card_id.to_vec(),
            created_card_id.clone(),
            "the journal carries the content-record id"
        );
        assert_eq!(
            m.cards[0].encrypted_fauna_ext,
            Some(b"fauna-sidecar".to_vec())
        );

        // MUA re-PUT: new body, NO sidecar on the wire.
        let body2 = sealed(b"vcard-v2-mua-edit");
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &body2,
            &sealed(b"hint-v2"),
            timestamp + 10,
            None,
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .expect("mua put ok");
        let reply2: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes2).unwrap();
        let updated_card_id = match reply2 {
            PutCardCiphertextReply::Updated { card_id, .. } => card_id,
            other => panic!("expected Updated, got {other:?}"),
        };
        assert_ne!(updated_card_id, created_card_id, "new body, new id");

        let m = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(m.cards.len(), 1, "supersede, not duplicate");
        assert_eq!(
            m.cards[0].card_id.to_vec(),
            updated_card_id,
            "the journal follows the superseding record id"
        );
        assert_eq!(
            m.cards[0].encrypted_fauna_ext,
            Some(b"fauna-sidecar".to_vec()),
            "the EFFECTIVE (preserved) sidecar, not the request's None"
        );
    }

    /// S6.9 v2 journal + S6.8d2 prune, card twin — see the calendar twin.
    #[tokio::test]
    async fn delete_card_journals_deleted_at_and_prunes_expired_tombstones() {
        use fauna_contacts::segments::placement::CardTombstoneRef;
        use fauna_protocol::bridge_routing::{DeleteCardReply, DeleteCardRequest};

        let state = fixture_state().await;
        let mda = [160u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [170u8; 32];
        let book_id = [215u8; 32];
        let uid_hash = [216u8; 32];
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &book_id).await;

        state
            .card_placement
            .update_manifest(&actor, |m| {
                m.tombstones.push(CardTombstoneRef {
                    addressbook_id: book_id,
                    uid_hash: [0x0Fu8; 32],
                    modseq: 1,
                    card_id: [0x0Fu8; 32],
                    deleted_at: 1_000,
                });
                true
            })
            .await
            .unwrap();

        let body = sealed(b"vcard-to-delete");
        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &body,
            &sealed(b"hint"),
            timestamp,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put ok");

        let del_req = DeleteCardRequest {
            actor_id: actor.to_vec(),
            addressbook_id: book_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            if_match: None,
        };
        let bytes = delete_card_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&del_req).unwrap().to_vec()),
        )
        .await
        .expect("delete ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let deleted_card_id = match reply {
            DeleteCardReply::Deleted { card_id, .. } => card_id,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let m = state.card_placement.current_manifest(&actor).await.unwrap();
        let ts = m
            .tombstones
            .iter()
            .find(|t| t.uid_hash == uid_hash)
            .expect("fresh tombstone present");
        assert_eq!(
            ts.card_id.to_vec(),
            deleted_card_id,
            "the tombstone carries the deleted row's record id"
        );
        assert!(ts.deleted_at > 0, "the tombstone carries its time");
        assert!(
            !m.tombstones.iter().any(|t| t.uid_hash == [0x0F; 32]),
            "the expired tombstone was pruned by the DELETE (S6.8d2)"
        );
    }

    #[tokio::test]
    async fn put_card_ciphertext_idempotent_appends_nothing() {
        // A transport retry (identical body to an already-stored card) returns
        // ReplaceCarddavCardOutcome::Idempotent — no bump on the SQLite side.
        // The journal mirrors that no-op.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [75u8; 32];
        let book_id = [107u8; 32];
        let uid_hash = [108u8; 32];
        let timestamp = 1_700_000_000i64;
        // Idempotent retry: same sealed bytes across both PUTs (bind once).
        let encrypted_body = sealed(b"encrypted-body-retry");
        let encrypted_hint = sealed(b"encrypted-hint-retry");

        provision_one(&state, &mda, &actor, &book_id).await;

        // First PUT → Created (one PutCard record).
        let payload1 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes1 = put_card_ciphertext_handler()(state.clone(), mda, payload1)
            .await
            .unwrap();
        let reply1: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes1).unwrap();
        let modseq_before: u64 = match reply1 {
            PutCardCiphertextReply::Created { modseq, .. } => modseq as u64,
            other => panic!("expected Created, got {other:?}"),
        };

        // Identical retry → Idempotent (no record emitted, no modseq bump).
        let payload2 = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &encrypted_body,
            &encrypted_hint,
            timestamp,
            None,
        );
        let bytes2 = put_card_ciphertext_handler()(state.clone(), mda, payload2)
            .await
            .unwrap();
        // The handler maps Idempotent to PutCardCiphertextReply::Updated by
        // design (no Idempotent wire variant); the manifest contents are the
        // load-bearing assertion.
        let _ = bytes2;

        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.cards.len(),
            1,
            "Idempotent retry must NOT emit a duplicate PutCard record",
        );
        assert_eq!(
            manifest.cards[0].modseq, modseq_before,
            "Idempotent retry must NOT bump the placement modseq",
        );
        assert_eq!(
            manifest.addressbooks[0].highestmodseq, modseq_before,
            "address book highestmodseq must not advance on Idempotent retry",
        );
    }

    #[tokio::test]
    async fn delete_card_appends_placement_record() {
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [76u8; 32];
        let book_id = [109u8; 32];
        let uid_hash = [110u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;

        // PUT first so there's a card row to delete.
        let put_payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-for-delete"),
            &sealed(b"encrypted-hint-for-delete"),
            timestamp,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, put_payload)
            .await
            .expect("seed put");

        // Verify the manifest seed: one card placement, no tombstones.
        let mid = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest mid");
        assert_eq!(
            mid.cards.len(),
            1,
            "PUT wiring must seed one card before DELETE test",
        );
        assert!(mid.tombstones.is_empty(), "no tombstones before DELETE");

        // DELETE → emits a DeleteCard record.
        let del_payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        let bytes = delete_card_handler()(state.clone(), mda, del_payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        let expected_modseq: u64 = match reply {
            DeleteCardReply::Deleted { modseq, .. } => modseq as u64,
            other => panic!("expected Deleted, got {other:?}"),
        };

        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert!(
            manifest
                .cards
                .iter()
                .all(|c| !(c.addressbook_id == book_id && c.uid_hash == uid_hash)),
            "DeleteCard record must drop the card from the manifest's cards Vec",
        );
        assert_eq!(
            manifest.tombstones.len(),
            1,
            "DeleteCard record must push exactly one tombstone",
        );
        let tomb = &manifest.tombstones[0];
        assert_eq!(tomb.addressbook_id, book_id);
        assert_eq!(tomb.uid_hash, uid_hash);
        assert_eq!(tomb.modseq, expected_modseq);
    }

    #[tokio::test]
    async fn delete_card_not_found_appends_nothing() {
        // DELETE on a never-existed card returns NotFound — idempotent, nothing
        // to delete, no journal entry.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [77u8; 32];
        let book_id = [111u8; 32];
        let uid_hash = [112u8; 32];

        provision_one(&state, &mda, &actor, &book_id).await;

        // DELETE without ever PUTting → NotFound, no DeleteCard record.
        let del_payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        let bytes = delete_card_handler()(state.clone(), mda, del_payload)
            .await
            .expect("handler ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteCardReply::NotFound);

        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");
        assert!(
            manifest.cards.is_empty(),
            "no card placement was ever emitted",
        );
        assert!(
            manifest.tombstones.is_empty(),
            "DeleteCard NotFound must NOT emit a tombstone",
        );
    }

    #[tokio::test]
    async fn delete_addressbook_appends_placement_record() {
        // Card-only op (CalDAV has no delete-calendar RPC): a successful
        // delete_addressbook emits a DeleteAddressbook record, whose manifest
        // apply cascades the address book + its cards + its tombstones away.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [78u8; 32];
        let book_id = [113u8; 32];
        let uid_hash = [114u8; 32];
        let timestamp = 1_700_000_000i64;

        provision_one(&state, &mda, &actor, &book_id).await;
        // Seed a card so the cascade has something to drop.
        let put_payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"encrypted-body-cascade"),
            &sealed(b"encrypted-hint-cascade"),
            timestamp,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, put_payload)
            .await
            .expect("seed put");

        let mid = state.card_placement.current_manifest(&actor).await.unwrap();
        assert_eq!(mid.addressbooks.len(), 1, "seed: one address book");
        assert_eq!(mid.cards.len(), 1, "seed: one card");

        // DELETE the whole address book.
        let payload = make_delete_addressbook_payload(&actor, &book_id);
        let bytes = delete_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, DeleteAddressbookReply::Deleted { .. }),
            "expected Deleted, got {reply:?}",
        );

        let manifest = state.card_placement.current_manifest(&actor).await.unwrap();
        assert!(
            manifest
                .addressbooks
                .iter()
                .all(|a| a.addressbook_id != book_id),
            "DeleteAddressbook must drop the address book from the manifest",
        );
        assert!(
            manifest.cards.iter().all(|c| c.addressbook_id != book_id),
            "DeleteAddressbook must cascade the address book's cards away",
        );
        assert!(
            manifest
                .tombstones
                .iter()
                .all(|t| t.addressbook_id != book_id),
            "DeleteAddressbook must cascade the address book's tombstones away",
        );
    }

    #[tokio::test]
    async fn delete_addressbook_not_found_appends_nothing() {
        // Idempotent re-delete of a never-existed address book → NotFound, no
        // DeleteAddressbook record.
        let state = fixture_state().await;
        let mda = [10u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [79u8; 32];
        let book_id = [115u8; 32];

        let payload = make_delete_addressbook_payload(&actor, &book_id);
        let bytes = delete_addressbook_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");
        let reply: DeleteAddressbookReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, DeleteAddressbookReply::NotFound);

        let manifest = state.card_placement.current_manifest(&actor).await.unwrap();
        assert!(
            manifest.addressbooks.is_empty(),
            "NotFound delete_addressbook must NOT emit a record",
        );
    }

    // ── End-to-end placement journal round-trip (twin of caldav T12) ──
    //
    // Drive a representative CardDAV client workflow through the WS-RPC
    // handler entry points and assert the resulting compacted placement
    // manifest, both in-memory via `current_manifest` and on disk via
    // `CardPlacementManifest::load(path)`.
    #[tokio::test]
    async fn placement_journal_round_trip_card_full_workflow() {
        use fauna_contacts::segments::placement::{
            CardPlacementManifest, card_placement_manifest_path,
        };

        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [0x61u8; 32];
        let work_book = [0x11u8; 32];
        let personal_book = [0x22u8; 32];
        let card_a1 = [0xa1u8; 32]; // work_book card 1 (survives)
        let card_a2 = [0xa2u8; 32]; // work_book card 2 (deleted)
        let card_b1 = [0xb1u8; 32]; // personal_book card

        // 1 + 2. Provision two address books.
        provision_one(&state, &mda, &actor, &work_book).await;
        provision_one(&state, &mda, &actor, &personal_book).await;

        // 3. PUT card_a1 into work_book.
        let payload = make_put_payload(
            &actor,
            &work_book,
            &card_a1,
            &sealed(&[10u8]),
            &sealed(b"e1"),
            1_700_000_001,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put a1 ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutCardCiphertextReply::Created { .. }),
            "a1 Created; got {reply:?}"
        );

        // 4. PUT card_a2 into work_book.
        let payload = make_put_payload(
            &actor,
            &work_book,
            &card_a2,
            &sealed(&[20u8]),
            &sealed(b"e2"),
            1_700_000_002,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put a2 ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutCardCiphertextReply::Created { .. }),
            "a2 Created; got {reply:?}"
        );

        // 5. PUT card_b1 into personal_book.
        let payload = make_put_payload(
            &actor,
            &personal_book,
            &card_b1,
            &sealed(&[30u8]),
            &sealed(b"e3"),
            1_700_000_003,
            None,
        );
        let bytes = put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put b1 ok");
        let reply: PutCardCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, PutCardCiphertextReply::Created { .. }),
            "b1 Created; got {reply:?}"
        );

        // 6. DELETE card_a2 from work_book.
        let payload = make_delete_payload(&actor, &work_book, &card_a2, None);
        let bytes = delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("delete a2 ok");
        let reply: DeleteCardReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply, DeleteCardReply::Deleted { .. }),
            "a2 Deleted; got {reply:?}"
        );

        // ── Final-state assertions on the in-memory manifest ──
        let manifest = state
            .card_placement
            .current_manifest(&actor)
            .await
            .expect("placement manifest");

        assert_eq!(
            manifest.addressbooks.len(),
            2,
            "two address books provisioned"
        );
        assert!(
            manifest
                .addressbooks
                .iter()
                .any(|a| a.addressbook_id == work_book)
        );
        assert!(
            manifest
                .addressbooks
                .iter()
                .any(|a| a.addressbook_id == personal_book)
        );

        // Two surviving cards: work_book:a1 + personal_book:b1 (a2 was deleted).
        assert_eq!(manifest.cards.len(), 2, "two surviving cards");
        assert!(
            manifest
                .cards
                .iter()
                .any(|c| c.addressbook_id == work_book && c.uid_hash == card_a1),
            "work_book:a1 placement must survive",
        );
        assert!(
            manifest
                .cards
                .iter()
                .any(|c| c.addressbook_id == personal_book && c.uid_hash == card_b1),
            "personal_book:b1 placement must survive",
        );

        // One tombstone — work_book:a2 from the DELETE.
        assert_eq!(
            manifest.tombstones.len(),
            1,
            "exactly one tombstone (work_book:a2)"
        );
        let tomb = &manifest.tombstones[0];
        assert_eq!(tomb.addressbook_id, work_book);
        assert_eq!(tomb.uid_hash, card_a2);

        // ── On-disk manifest equals the in-memory snapshot ──
        let manifest_path = card_placement_manifest_path(state.card_placement.data_dir(), &actor);
        let on_disk = CardPlacementManifest::load(&manifest_path)
            .expect("load manifest from disk")
            .expect("manifest file exists after all writes");
        assert_eq!(
            on_disk, manifest,
            "on-disk manifest must equal the in-memory snapshot after every save_atomic",
        );
    }

    // ── S6.6 content-cutover tests (twins of the calendar arm) ────────

    /// Post-cutover write: the vCard body is durable in the `__card` segment,
    /// reachable through the row's stored `record_cid`.
    #[tokio::test]
    async fn put_card_stores_the_body_in_the_segment() {
        let state = fixture_state().await;
        let mda = [80u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [81u8; 32];
        let book_id = [82u8; 32];
        // card_id derives from the body bytes, so bind the seal once.
        let body = sealed(b"sealed-vcard-body");
        let timestamp = 1_700_000_000i64;
        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_put_payload(
            &actor,
            &book_id,
            &[83u8; 32],
            &body,
            &sealed(b"hint"),
            timestamp,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        let page = state
            .db
            .query_carddav_cards(&actor, &book_id, None, None, 10)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1);

        let card_id = derive_carddav_card_id(&actor, timestamp, &body);
        // Post-cutover the row's stored cid is the ONLY handle on the record.
        let record_cid = page.cards[0]
            .record_cid()
            .expect("row cid parses")
            .expect("a segment-served row must carry its record_cid");
        let (envelope, floor) = crate::segments::card::read_record(
            &state.card_segments,
            &state.db,
            &actor,
            &record_cid,
        )
        .await
        .unwrap()
        .expect("content record must be durable in the __card segment");
        assert_eq!(envelope.encrypted_body, body.to_vec());
        assert_eq!(floor.card_id, card_id);
        assert_eq!(floor.addressbook_id, book_id);
    }

    /// The open path (CardDAV multiget) must be byte-identical across the cutover.
    #[tokio::test]
    async fn query_cards_serves_the_body_from_the_segment() {
        use fauna_protocol::bridge_routing::QueryCardsReply;
        let state = fixture_state().await;
        let mda = [84u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [85u8; 32];
        let book_id = [86u8; 32];
        let body = sealed(b"sealed-vcard-body-for-multiget");
        provision_one(&state, &mda, &actor, &book_id).await;

        let payload = make_put_payload(
            &actor,
            &book_id,
            &[87u8; 32],
            &body,
            &sealed(b"hint"),
            1_700_000_000,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        let q = make_query_cards_payload(&actor, &book_id, None, None, 10);
        let bytes = query_cards_handler()(state.clone(), mda, q)
            .await
            .expect("query ok");
        match fauna_cbor::decode_strict::<QueryCardsReply>(&bytes).unwrap() {
            QueryCardsReply::Ok { cards, .. } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].encrypted_body, body.to_vec());
            }
            other => panic!("expected Ok; got {other:?}"),
        }
    }

    // ── fauna.addressbook.changed push emission ───────────────────
    //
    // The emit rule under test (transport.md § Push events, ratified
    // 2026-08-05): every durable card or book write — put Created/Updated,
    // delete Deleted, provision Created/metadata-Updated — fires ONE
    // `fauna.addressbook.changed` at the address book owner's own
    // connections; the no-DB-change outcomes (Idempotent / AlreadyExists /
    // NotFound / PreconditionFailed / AddressbookMissing / Conflict) fire
    // NOTHING, mirroring the placement-record no-emit rule. Exact twin of the
    // `fauna.calendar.changed` test block in `bridge_caldav_handlers.rs`.

    /// Receive exactly one `fauna.addressbook.changed` push (bounded wait) and
    /// assert its payload names `owner` + `book_id` in hex.
    async fn expect_addressbook_changed(
        rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
        owner: &[u8; 32],
        book_id: &[u8; 32],
    ) {
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for fauna.addressbook.changed push")
            .expect("push channel closed");
        let frame = fauna_protocol::decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            fauna_protocol::Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = fauna_protocol::PushEvent::from_push(&push.kind, push.payload);
        match event {
            fauna_protocol::PushEvent::AddressBookChanged(p) => {
                assert_eq!(p.actor_id, hex::encode(owner));
                assert_eq!(p.addressbook_id, hex::encode(book_id));
            }
            other => panic!("expected AddressBookChanged push, got {}", other.kind()),
        }
    }

    #[tokio::test]
    async fn put_created_updated_emit_addressbook_changed_idempotent_does_not() {
        let state = fixture_state().await;
        let mda = [230u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [231u8; 32];
        let book_id = [232u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;
        // Subscribe AFTER provisioning so the provision push isn't in the queue.
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let uid_hash = [233u8; 32];
        let body_v1 = sealed(b"BEGIN:VCARD v1");
        let hint = sealed(b"hint");

        // Created → one push.
        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &body_v1,
            &hint,
            1_700_000_000,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put created");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Updated (different body) → one push.
        let body_v2 = sealed(b"BEGIN:VCARD v2");
        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &body_v2,
            &hint,
            1_700_000_100,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put updated");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Idempotent retry (same body bytes) → NO push. This is the arm the
        // carddav DB layer already makes free: `replace_carddav_card_by_uid`
        // leaves ctag/modseq untouched when the new body hashes to the prior
        // row's card_id, so there is nothing to suppress here — only an arm
        // never to add.
        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &body_v2,
            &hint,
            1_700_000_100,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put idempotent");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn provision_created_emits_addressbook_changed_already_exists_does_not() {
        use fauna_protocol::bridge_routing::ProvisionAddressbookRequest;

        let state = fixture_state().await;
        let mda = [234u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [235u8; 32];
        let book_id = [236u8; 32];
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let make_payload = || {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: b"sealed-meta".to_vec(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // Created → one push.
        provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .expect("provision created");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Identical retry → AlreadyExists → NO push.
        provision_addressbook_handler()(state.clone(), mda, make_payload())
            .await
            .expect("provision already-exists");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn provision_metadata_update_emits_only_when_the_bytes_change() {
        use fauna_protocol::bridge_routing::ProvisionAddressbookRequest;

        let state = fixture_state().await;
        let mda = [237u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [238u8; 32];
        let book_id = [239u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;
        let (_conn, mut rx) = state.ws.subscribe(actor);

        let update = |meta: &[u8]| {
            let req = ProvisionAddressbookRequest {
                actor_id: actor.to_vec(),
                addressbook_id: book_id.to_vec(),
                encrypted_metadata: meta.to_vec(),
                update_metadata: true,
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // PROPPATCH with new bytes → highestmodseq moves → one push.
        provision_addressbook_handler()(state.clone(), mda, update(b"sealed-meta-v2"))
            .await
            .expect("metadata updated");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Byte-identical PROPPATCH → highestmodseq unchanged → NO push. The
        // guard under test is the `hms_before != hms_after` gate, the same one
        // that decides whether a placement record is appended.
        provision_addressbook_handler()(state.clone(), mda, update(b"sealed-meta-v2"))
            .await
            .expect("metadata no-op");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn delete_deleted_emits_addressbook_changed_not_found_does_not() {
        let state = fixture_state().await;
        let mda = [240u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [241u8; 32];
        let book_id = [242u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;

        let uid_hash = [243u8; 32];
        let payload = make_put_payload(
            &actor,
            &book_id,
            &uid_hash,
            &sealed(b"BEGIN:VCARD"),
            &sealed(b"hint"),
            1_700_000_000,
            None,
        );
        put_card_ciphertext_handler()(state.clone(), mda, payload)
            .await
            .expect("put created");

        // Subscribe AFTER the put so only the deletes are in the queue.
        let (_conn, mut rx) = state.ws.subscribe(actor);

        // Deleted → one push.
        let payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("delete deleted");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Re-delete → NotFound → NO push.
        let payload = make_delete_payload(&actor, &book_id, &uid_hash, None);
        delete_card_handler()(state.clone(), mda, payload)
            .await
            .expect("delete not-found");
        expect_no_push(&mut rx).await;
    }

    #[tokio::test]
    async fn delete_addressbook_emits_addressbook_changed_not_found_does_not() {
        let state = fixture_state().await;
        let mda = [244u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let actor = [245u8; 32];
        let book_id = [246u8; 32];
        provision_one(&state, &mda, &actor, &book_id).await;
        let (_conn, mut rx) = state.ws.subscribe(actor);

        // Deleted → one push. The book is gone, but the nudge still names it:
        // consumers re-list and observe the disappearance, exactly as the
        // calendar twin's consumers re-list after a write.
        delete_addressbook_handler()(
            state.clone(),
            mda,
            make_delete_addressbook_payload(&actor, &book_id),
        )
        .await
        .expect("delete deleted");
        expect_addressbook_changed(&mut rx, &actor, &book_id).await;

        // Re-delete → NotFound → NO push.
        delete_addressbook_handler()(
            state.clone(),
            mda,
            make_delete_addressbook_payload(&actor, &book_id),
        )
        .await
        .expect("delete not-found");
        expect_no_push(&mut rx).await;
    }
}
