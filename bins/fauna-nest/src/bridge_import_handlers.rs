//! WS-RPC handlers for the mailbox-migration import surface
//! (`docs/goal/behavior/mailbox-migration.md`).
//!
//! Every kind here is **User-class and caller-scoped**: the importer is the
//! user's own Fauna app pulling mail from a foreign IMAP server and filing
//! each message into its own mailbox. The write path is deliberately the same
//! one APPEND uses (`insert_appended_mail` → `place_or_get_existing_placement`
//! → placement journal + IDLE/NOTIFY push) — imports are "just APPENDs with a
//! source-tracking annotation" (§ Architectural rules) — but over *distinct*
//! kinds, because `fauna.bridges.append` is BridgeMda-only in the allowlist.
//!
//! **The seal happens HERE, not in the caller.** APPEND's at-rest seal lives
//! in its trusted BridgeMda caller (`internal/mda/imap/append.go`); import
//! replaces that caller with an untrusted user client, so `import_one` seals
//! the body and the nest-derived index hint to the recipient's registered
//! seal key through the D2 resolver (`get_recipient_seal_key`),
//! unconditionally in both storage modes, fail-closed on a missing key —
//! `encryption-at-rest.md` S1 uniform seal at ingest, the sixth seal site.
//!
//! Session lifecycle mirrors `mail-export.md` § RPC table:
//! `start` / `pause` / `resume` / `cancel` / `finalize` / `fail` +
//! `import_message` / `import_message_batch` / `list_import_sessions`.

use std::time::Duration;

use fauna_mail::segments::placement::MailPlacementRecord;
use fauna_protocol::bridge_routing::{
    BridgeImportCompletePush, BridgeImportErrorPush, BridgeImportProgressPush,
    FailImportSessionRequest, ImportMailboxCursor, ImportMessageBatchReply,
    ImportMessageBatchRequest, ImportMessageItem, ImportMessageOutcome, ImportMessageReply,
    ImportMessageRequest, ImportSessionActionReply, ImportSessionActionRequest, ImportSessionInfo,
    ListImportSessionsReply, ListImportSessionsRequest, MailboxStateEvent, StartImportSessionReply,
    StartImportSessionRequest,
};
use fauna_protocol::decode_strict as decode;

use crate::bridge_imap_handlers::{
    emit_bootstrap_create_records, emit_mailbox_state_event, enforce_imap_storage_quota,
    require_local_mail_serving, split_flags,
};
use crate::bridge_routing_handlers::{
    encode_reply, internal, malformed, not_found, require_class, seal_recipient_blob,
};
use crate::db::bridge_routing::{InboundMailFields, RecipientSealKey};
use crate::db::mail_import::{CreateImportSessionOutcome, ImportSessionRow};
use crate::email_handlers::invalid_params;
use crate::rpc_router::{RpcKindMeta, RpcRouterBuilder};
use fauna_mls::wrapped_blob::SealedRecordBytes;

// § Batching: the per-call ceilings the client must respect — owned by the
// crate that owns the wire contract, so this handler and the client's
// `BatchPacker` enforce one number rather than two that happen to agree.
use fauna_protocol::bridge_routing::{MAX_BATCH_BYTES, MAX_BATCH_MESSAGES};

fn row_to_info(row: ImportSessionRow) -> ImportSessionInfo {
    ImportSessionInfo {
        session_id: row.session_id,
        source_descriptor: row.source_descriptor,
        state: row.state,
        started_at: row.started_at,
        last_progress_at: row.last_progress_at,
        total_count: row.total_count,
        imported_count: row.imported_count,
        skipped_count: row.skipped_count,
        errored_count: row.errored_count,
        cursors: row
            .cursors
            .into_iter()
            .map(|(mailbox, (uid, uid_validity))| ImportMailboxCursor {
                mailbox,
                last_processed_source_uid: uid,
                source_uid_validity: uid_validity,
            })
            .collect(),
        error_reason: row.error_reason,
        scope: row.scope,
        date_from: row.date_from,
        // The pair ships TOGETHER. `source_hash` is the salt `source_sealed`
        // opens under, and it is derived from `source_descriptor` — exactly
        // the column the boot scrub blanks once the seal rests. A reply
        // carrying the seal alone renders every session correctly until the
        // first reboot and then not at all (`label_custody::render_set_name`
        // records the set-name plane shipping that hole twice).
        source_sealed: row.source_sealed.map(serde_bytes::ByteBuf::from),
        source_hash: row.source_hash.map(serde_bytes::ByteBuf::from),
    }
}

fn notify_import_progress(
    state: &crate::routes::AppState,
    actor: &[u8; 32],
    row: &ImportSessionRow,
) {
    state.ws.notify_push(
        actor,
        fauna_protocol::PushEvent::BridgeImportProgress(BridgeImportProgressPush {
            session_id: row.session_id.clone(),
            imported_count: row.imported_count,
            skipped_count: row.skipped_count,
            errored_count: row.errored_count,
        }),
    );
}

/// Validate one item's self-consistency against its **resolved** plaintext body
/// (`effective_len` — the inline body length, or the opened staged-envelope
/// length). Returns the per-message errored reason (client review-log fodder)
/// rather than failing the whole call — § Batching: one bad message doesn't take
/// down its siblings. Body presence + the exactly-one-of / skew-contract logic
/// live in [`import_one`], ahead of this call, because they gate *which* bytes
/// `effective_len` even measures.
fn validate_item(item: &ImportMessageItem, effective_len: usize) -> Result<(), String> {
    // `body_size` is the PLAINTEXT length in both the inline and the staged path
    // (`mailbox-migration.md`: checked after the nest opens the envelope).
    if item.body_size as usize != effective_len {
        return Err(format!(
            "body_size mismatch: metadata={} body_bytes={effective_len}",
            item.body_size,
        ));
    }
    if item.flags.iter().any(|f| f == "\\Recent") {
        return Err("\\Recent cannot be imported".into());
    }
    // The one definition of a present key, shared with the delivery and APPEND
    // doors (`mailbox-migration.md` § *There is no absent key*).
    fauna_mail::require_dedup_pair(&item.dedup_key, &item.envelope_key)?;
    Ok(())
}

/// Import a single validated-or-not item into the caller's mailbox. Returns
/// the per-message outcome; only infrastructure failures (DB down) surface as
/// `RpcError` and abort the call.
async fn import_one(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    seal_key: &RecipientSealKey,
    item: &ImportMessageItem,
    skip_dedup: bool,
    max_raw_message_bytes: u32,
) -> Result<ImportMessageOutcome, fauna_protocol::RpcError> {
    // ── Resolve the effective plaintext body: exactly-one-of {non-empty `body`,
    // `staged_body`} — the same guard `persist_inbound_mail_request` uses for the
    // inbound ingest leg (`smtp-server.md` § Message size limits, the
    // staged-envelope rule). An over-inline-ceiling import stages the ciphertext
    // on the byte plane and names it by `staged_body`; the nest resolves it from
    // its own blob store, opens the envelope, and then takes the *identical* path
    // an inline body takes below.
    let body: std::borrow::Cow<[u8]> = match &item.staged_body {
        None => {
            // ⚠ Skew contract: this
            // empty-body refusal MUST stay ahead of any staged-body resolution, so
            // an older nest that silently drops the unknown `staged_body` field
            // degrades to this exact typed per-message error — never an empty import.
            if item.body.is_empty() {
                return Ok(ImportMessageOutcome::Errored {
                    reason: "body must not be empty".into(),
                });
            }
            std::borrow::Cow::Borrowed(item.body.as_slice())
        }
        Some(staged) => {
            if !item.body.is_empty() {
                return Ok(ImportMessageOutcome::Errored {
                    reason: "body must be empty when staged_body is set".into(),
                });
            }
            // The admission ceiling, applied to the DECLARED sealed total before a
            // single chunk is read (`mail-message-size.md` § Message size limits,
            // the bounded-reference rule): the same typed refusal the resolved
            // body gets below, and the same ceiling the resolver then enforces.
            let sealed_ceiling = u64::from(max_raw_message_bytes)
                + fauna_mail::staged_envelope::STAGED_ENVELOPE_OVERHEAD_BYTES;
            if staged.total_bytes > sealed_ceiling {
                return Ok(ImportMessageOutcome::Errored {
                    reason: fauna_protocol::email::MESSAGE_TOO_LARGE_CODE.to_string(),
                });
            }
            let plaintext =
                match crate::mail_body_plane::resolve_staged_body(state, staged, sealed_ceiling)
                    .await
                {
                    Ok(pt) => pt,
                    // A reference the client got wrong (mis-named/reordered/dropped
                    // chunk, wrong key, lying total) is THIS message's fault — a
                    // per-message error so its siblings still import. A nest-wide infra
                    // failure (no byte plane) propagates and aborts the whole call.
                    Err(e) if e.code == crate::mail_body_plane::INVALID_BODY_REF_CODE => {
                        return Ok(ImportMessageOutcome::Errored {
                            reason: "staged body reference could not be resolved".into(),
                        });
                    }
                    Err(e) => return Err(e),
                };
            // A staged envelope that opens to empty plaintext is still an empty
            // message — refuse it exactly as the inline empty-body case above.
            if plaintext.is_empty() {
                return Ok(ImportMessageOutcome::Errored {
                    reason: "body must not be empty".into(),
                });
            }
            std::borrow::Cow::Owned(plaintext)
        }
    };

    // Admission ceiling — the same shared rule the SMTP `Data` paths read, so
    // import enforcement can never drift from what can actually rest
    // (`smtp-server.md` § Message size limits; the SMTP perimeter clamps the same
    // value Go-side). The client is untrusted, so this is authoritative regardless
    // of any client pre-check.
    if body.len() > max_raw_message_bytes as usize {
        // Typed so a client renders "too large" specifically rather than parsing
        // a free-form string. The reason IS the shared `MESSAGE_TOO_LARGE_CODE`
        // that `fauna.email.send` returns as an RpcError code — one identifier a
        // client matches on both first-party legs — following the
        // reason-as-discriminator convention `Skipped` uses ("dedup", …).
        return Ok(ImportMessageOutcome::Errored {
            reason: fauna_protocol::email::MESSAGE_TOO_LARGE_CODE.to_string(),
        });
    }
    if let Err(reason) = validate_item(item, body.len()) {
        return Ok(ImportMessageOutcome::Errored { reason });
    }

    // § Dedup scope: skip iff the key matches anything the actor already
    // holds, in any mailbox, AND the envelope keys agree — unless the
    // wizard's "Import duplicates anyway". The Message-ID is the sender's
    // choice: an earlier delivery reusing it with other content wrote the
    // row first, and it must not make this message skip (§ The envelope key
    // confirms a Message-ID hit).
    if !skip_dedup
        && state
            .db
            .dedup_hit(actor, &item.dedup_key, &item.envelope_key)
            .await
            .map_err(internal)?
    {
        return Ok(ImportMessageOutcome::Skipped {
            reason: "dedup".into(),
        });
    }

    // Every door that files bytes the nest did not compose removes the reserved
    // `X-Fauna-*` delivery stamps before it parses or seals them
    // (`smtp-server.md` § Architectural rules → *The `X-Fauna-*` namespace*):
    // a migrated copy — from a hostile source, or from another Fauna nest whose
    // stamps were that nest's policy — must not carry a spam tier or alias match
    // this nest never decided, because the MDA scorer and the apps trust the
    // stamp without asking which door filed the copy. The size ceiling and
    // `validate_item` above judged the bytes as sent; the dedup key is the
    // client's, over those same bytes, exactly as APPEND takes it. The
    // forward-loop trace `X-Fauna-Forwarded-By` survives, as at every door.
    let body: std::borrow::Cow<[u8]> =
        std::borrow::Cow::Owned(fauna_mail::received_header::strip_fauna_headers(&body));
    // A body made of nothing but reserved stamps strips to zero bytes. The
    // empty-body refusals above judged the bytes as sent, so re-check here:
    // an empty stored body fails every later export of the account
    // (`mail-export.md` § UX shape step 2), so it is this message's refusal —
    // its siblings still import. Mirrors the export's `EmptyAfterStrip`.
    if body.is_empty() {
        return Ok(ImportMessageOutcome::Errored {
            reason: "body is empty once its X-Fauna-* stamps are stripped".into(),
        });
    }

    // Seal at ingest, in both storage modes (`encryption-at-rest.md` S1) —
    // the caller is an untrusted user client, so unlike APPEND (whose trusted
    // BridgeMda caller seals before the RPC) the seal is the nest's job.
    // Index hint tokenized from the plaintext exactly as the MTA and
    // `seal_and_persist_local` do, so imported mail is searchable by the same
    // MDA SEARCH path; both blobs seal to the key resolved through the D2
    // seam by `run_import` (fail-closed there, once per call).
    let parsed = mail_parser::MessageParser::default().parse(body.as_ref());
    let subject = parsed.as_ref().and_then(|p| p.subject()).unwrap_or("");
    let body_text = parsed
        .as_ref()
        .and_then(|p| p.body_text(0))
        .unwrap_or_default();
    let index_hint =
        fauna_mail::tokenizer::tokenize(&format!("{subject} {body_text}")).canonical_bytes;
    // S6.12b: mint the typed halves from our own seal output (`verify` is a
    // cheap strict decode of bytes we just sealed — a failure is a seal-path
    // bug, surfaced as internal, never a per-item skip).
    let (sealed_body, sealed_hint) = {
        let pubkey = &seal_key.mls_pubkey;
        let mlkem_ek = Some(seal_key.mlkem_ek.as_slice());
        (
            SealedRecordBytes::verify(seal_recipient_blob(
                body.as_ref(),
                pubkey,
                mlkem_ek,
                "body",
            )?)
            .map_err(internal)?,
            SealedRecordBytes::verify(seal_recipient_blob(
                &index_hint,
                pubkey,
                mlkem_ek,
                "index-hint",
            )?)
            .map_err(internal)?,
        )
    };

    // § Quota composition: imports count toward the per-mailbox STORAGE +
    // MESSAGE quota; enforcement here is authoritative regardless of the
    // client's `fauna.quota.get` pre-check. Unlike APPEND (typed error →
    // `NO [OVERQUOTA]`), the per-message outcome carries the signal —
    // "the rest of the import is recorded as skipped with
    // reason=quota_exceeded" (§ Failure handling). Charged on the sealed
    // record size — the bytes that actually rest on disk.
    if enforce_imap_storage_quota(state, actor, sealed_body.len() as u64, 1)
        .await
        .is_err()
    {
        return Ok(ImportMessageOutcome::Skipped {
            reason: "quota_exceeded".into(),
        });
    }

    // Same storage path as APPEND: no envelope, no verdicts, never
    // spam-scored, own-submission.
    let fields = InboundMailFields {
        actor_id: *actor,
        timestamp: item.timestamp,
        ciphertext_size: sealed_body.len() as u32,
        encrypted_body: sealed_body,
        encrypted_index_hint: sealed_hint,
        sender_domain: item.sender_domain.clone(),
        spf: "none".into(),
        dkim: "none".into(),
        dmarc: "none".into(),
        dmarc_policy: "none".into(),
        arc: "none".into(),
        spam_score: 0,
        spam_disposition: "accept".into(),
        is_own_submission: true,
        scores: vec![],
        report_hash: vec![],
    };

    let insert_outcome = state
        .db
        .insert_appended_mail(&state.mail_segments, &fields)
        .await
        .map_err(internal)?;
    let message_id = insert_outcome.message_id;
    if let Some(closed_seg_id) = insert_outcome.finalized {
        crate::segments::notify_segments_changed(
            &state.ws,
            actor,
            "mail",
            closed_seg_id,
            fauna_protocol::push_events::SegmentChange::Finalized,
        );
    }

    let newly_seeded = state
        .db
        .ensure_bridge_imap_mailboxes(actor)
        .await
        .map_err(internal)?;
    emit_bootstrap_create_records(state, actor, newly_seeded).await?;

    let flags_string: String = {
        use std::collections::BTreeSet;
        let set: BTreeSet<&str> = item
            .flags
            .iter()
            .map(String::as_str)
            .filter(|&f| f != "\\Recent")
            .collect();
        set.into_iter().collect::<Vec<_>>().join(" ")
    };

    let (uid, placement_modseq) = state
        .db
        .place_or_get_existing_placement(
            actor,
            &message_id,
            &item.mailbox,
            item.timestamp,
            &flags_string,
            &item.sender_domain,
            insert_outcome.inserted,
        )
        .await
        .map_err(internal)?;

    let state_row = state
        .db
        .get_bridge_imap_mailbox_state(actor, &item.mailbox)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("mailbox state row missing after placement"))?;

    if let Some(modseq) = placement_modseq {
        let record = MailPlacementRecord::Append {
            mailbox: item.mailbox.clone(),
            uid,
            modseq: modseq as u64,
            flags: split_flags(&flags_string),
            content_record_id: message_id.to_vec(),
            internal_date: item.timestamp,
        };
        state
            .mail_placement
            .append_event(actor, &record)
            .await
            .map_err(internal)?;
        emit_mailbox_state_event(
            state,
            actor,
            &item.mailbox,
            MailboxStateEvent::Append {
                uid,
                flags: split_flags(&flags_string),
                modseq,
            },
        );
    }

    // § Dedup key persistence: every import populates the index with the
    // pair (first writer wins), including "Import duplicates anyway" imports
    // and a same-Message-ID message whose envelope key disagreed.
    state
        .db
        .insert_dedup_key(
            actor,
            &item.dedup_key,
            &item.envelope_key,
            &hex::encode(message_id),
        )
        .await
        .map_err(internal)?;

    Ok(ImportMessageOutcome::Imported {
        message_id: message_id.to_vec(),
        uid,
        uid_validity: state_row.uid_validity,
    })
}

/// Shared body of `import_message` / `import_message_batch`: gate the session,
/// run the items, fold progress into the session row per message, emit one
/// `BridgeImportProgress` push at the end.
async fn run_import(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    session_id: &str,
    items: &[ImportMessageItem],
    skip_dedup: bool,
    revised_total_count: Option<u64>,
) -> Result<(Vec<ImportMessageOutcome>, ImportSessionRow), fauna_protocol::RpcError> {
    let session = state
        .db
        .get_import_session(actor, session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found(format!("import session '{session_id}' not found")))?;
    if session.state != "running" {
        return Err(invalid_params(&format!(
            "import session '{session_id}' is {}, not running",
            session.state
        )));
    }

    // Phase-3 D2: a mailbox-migration import is genuine new-mail-ingest (the
    // sixth seal site, encryption-at-rest.md S1), so it resolves through the
    // content-sealing-epochs mail seam, once per call (it is per-actor, not
    // per-message). Fail-closed: a caller with no registered seal key cannot
    // store anything — same wording as `seal_and_persist_local`.
    let seal_key = state
        .db
        .get_recipient_mail_seal_key(actor, state.epoch_sealing_enabled())
        .await
        .map_err(internal)?
        .ok_or_else(|| invalid_params("recipient has no encryption key on file"))?;

    // The admission ceiling, resolved once per call (the spam policy is nest-wide,
    // not per-message): the admin's `max_message_bytes` knob clamped to what can
    // actually rest, via the one shared rule the SMTP `Data` paths read
    // (`smtp-server.md` § Message size limits). `0`/unset ⇒ the at-rest ceiling.
    let max_raw_message_bytes = fauna_mail::transport_limits::effective_max_raw_message_bytes(
        state
            .db
            .get_spam_policy()
            .await
            .map_err(internal)?
            .effective()
            .max_message_bytes,
    );

    let mut outcomes = Vec::with_capacity(items.len());
    let mut latest = session;
    let mut revised_total = revised_total_count;
    for item in items {
        let outcome = import_one(
            state,
            actor,
            &seal_key,
            item,
            skip_dedup,
            max_raw_message_bytes,
        )
        .await?;
        let (imported, skipped, errored) = match &outcome {
            ImportMessageOutcome::Imported { .. } => (1, 0, 0),
            ImportMessageOutcome::Skipped { .. } => (0, 1, 0),
            // The nest never builds `Unknown`; counted as errored if it ever did.
            ImportMessageOutcome::Errored { .. } | ImportMessageOutcome::Unknown => (0, 0, 1),
        };
        // Advance the resume cursor even for skipped/errored messages — the
        // client has *processed* this source UID; resume must not refetch it.
        latest = state
            .db
            .record_import_progress(
                actor,
                session_id,
                imported,
                skipped,
                errored,
                Some((
                    item.mailbox.as_str(),
                    item.source_uid,
                    item.source_uid_validity,
                )),
                revised_total.take(),
            )
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("import session row vanished mid-batch"))?;
        outcomes.push(outcome);
    }
    // One progress push per wire call (§ Per-message flow step 5 — "after
    // each batch"), to the importer's own connected clients.
    notify_import_progress(state, actor, &latest);
    Ok((outcomes, latest))
}

// ── Handlers ─────────────────────────────────────────────────────────

fn start_import_session_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.start_import_session").await?;
            let req: StartImportSessionRequest = decode(&payload).map_err(malformed)?;
            if req.source_descriptor.is_empty() {
                return Err(malformed("source_descriptor must not be empty"));
            }
            require_local_mail_serving(&state, &actor_id).await?;
            let session_id = uuid::Uuid::new_v4().to_string();
            match state
                .db
                .create_import_session(
                    &actor_id,
                    &session_id,
                    &req.source_descriptor,
                    req.total_count,
                    &req.scope,
                    // Recorded at the durable commit point beside `scope`:
                    // the range the user asked for lives nowhere else, so a
                    // session that did not record it can only resume by
                    // importing everything they excluded.
                    &req.date_from,
                    // Stored verbatim: the root is the owner's, so the nest
                    // neither mints nor opens this label (the `name_sealed` /
                    // `tags_sealed` store-and-serve posture).
                    req.source_sealed.as_ref().map(|b| &b[..]),
                )
                .await
                .map_err(internal)?
            {
                CreateImportSessionOutcome::Created => {
                    encode_reply(&StartImportSessionReply { session_id })
                }
                CreateImportSessionOutcome::SourceLocked => Err(import_source_locked()),
            }
        })
    })
}

fn import_message_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.import_message").await?;
            let req: ImportMessageRequest = decode(&payload).map_err(malformed)?;
            require_local_mail_serving(&state, &actor_id).await?;
            let (mut outcomes, row) = run_import(
                &state,
                &actor_id,
                &req.session_id,
                std::slice::from_ref(&req.message),
                req.skip_dedup,
                None,
            )
            .await?;
            encode_reply(&ImportMessageReply {
                outcome: outcomes.remove(0),
                imported_count: row.imported_count,
                skipped_count: row.skipped_count,
                errored_count: row.errored_count,
            })
        })
    })
}

fn import_message_batch_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.import_message_batch").await?;
            let req: ImportMessageBatchRequest = decode(&payload).map_err(malformed)?;
            if req.messages.is_empty() {
                return Err(malformed("messages must not be empty"));
            }
            if req.messages.len() > MAX_BATCH_MESSAGES {
                return Err(malformed(format!(
                    "batch of {} exceeds the {MAX_BATCH_MESSAGES}-message cap",
                    req.messages.len()
                )));
            }
            // Defense-in-depth on a batch's *referenced* byte total, never an
            // inline-frame promise (`mailbox-migration.md` § Batching): count the
            // staged ciphertext length for an over-ceiling item whose inline `body`
            // is empty, so a batch cannot smuggle unbounded staged bytes past this
            // bound. Per-message admission is still authoritative in `import_one`.
            let total_bytes: usize = req
                .messages
                .iter()
                .map(|m| {
                    m.body.len().saturating_add(
                        m.staged_body.as_ref().map_or(0, |s| s.total_bytes as usize),
                    )
                })
                .sum();
            if total_bytes > MAX_BATCH_BYTES {
                return Err(malformed(format!(
                    "batch of {total_bytes} body bytes exceeds the {MAX_BATCH_BYTES}-byte cap"
                )));
            }
            require_local_mail_serving(&state, &actor_id).await?;
            let (outcomes, row) = run_import(
                &state,
                &actor_id,
                &req.session_id,
                &req.messages,
                req.skip_dedup,
                req.revised_total_count,
            )
            .await?;
            encode_reply(&ImportMessageBatchReply {
                outcomes,
                imported_count: row.imported_count,
                skipped_count: row.skipped_count,
                errored_count: row.errored_count,
            })
        })
    })
}

fn list_import_sessions_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_import_sessions").await?;
            let _req: ListImportSessionsRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_import_sessions(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListImportSessionsReply {
                sessions: rows.into_iter().map(row_to_info).collect(),
            })
        })
    })
}

/// One handler body per state-mutating kind; `transition` names the target
/// state + allowed sources (`mail-export.md:132` state machine, mirrored).
fn session_transition_handler(
    kind: &'static str,
    new_state: &'static str,
    allowed_from: &'static [&'static str],
) -> crate::rpc_router::RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, kind).await?;
            let req: ImportSessionActionRequest = decode(&payload).map_err(malformed)?;
            let row = transition_or_typed_err(
                &state,
                &actor_id,
                &req.session_id,
                new_state,
                allowed_from,
                None,
            )
            .await?;
            if new_state == "completed" {
                state.ws.notify_push(
                    &actor_id,
                    fauna_protocol::PushEvent::BridgeImportComplete(BridgeImportCompletePush {
                        session_id: row.session_id.clone(),
                        imported_count: row.imported_count,
                        skipped_count: row.skipped_count,
                        errored_count: row.errored_count,
                    }),
                );
            }
            encode_reply(&ImportSessionActionReply {
                session: row_to_info(row),
            })
        })
    })
}

fn fail_import_session_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fail_import_session").await?;
            let req: FailImportSessionRequest = decode(&payload).map_err(malformed)?;
            if req.reason.is_empty() {
                return Err(malformed("reason must not be empty"));
            }
            let row = transition_or_typed_err(
                &state,
                &actor_id,
                &req.session_id,
                "errored",
                &["running", "paused"],
                Some(&req.reason),
            )
            .await?;
            state.ws.notify_push(
                &actor_id,
                fauna_protocol::PushEvent::BridgeImportError(BridgeImportErrorPush {
                    session_id: row.session_id.clone(),
                    reason: req.reason.clone(),
                }),
            );
            encode_reply(&ImportSessionActionReply {
                session: row_to_info(row),
            })
        })
    })
}

/// Run the conditional transition, mapping the `None` (no-op) result to the
/// precise typed error: `not_found` when the session doesn't exist for this
/// caller, `invalid_params` when it exists but the transition doesn't apply.
async fn transition_or_typed_err(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    session_id: &str,
    new_state: &str,
    allowed_from: &[&str],
    error_reason: Option<&str>,
) -> Result<ImportSessionRow, fauna_protocol::RpcError> {
    if let Some(row) = state
        .db
        .transition_import_session(actor, session_id, new_state, allowed_from, error_reason)
        .await
        .map_err(internal)?
    {
        return Ok(row);
    }
    match state
        .db
        .get_import_session(actor, session_id)
        .await
        .map_err(internal)?
    {
        None => Err(not_found(format!(
            "import session '{session_id}' not found"
        ))),
        // The zero-row `UPDATE` above is ambiguous between "impossible
        // transition" and "already converged": `allowed_from` only matches
        // the SOURCE state, so a call landing after the session already
        // reached `new_state` matches nothing either. `forbid_replay: false`
        // on all five of these kinds (`transport.md` § Idempotency and
        // reconnect-with-resume) asserts the handler is naturally idempotent
        // — state converges, and the reply must say so instead of reporting
        // a wrong-state error for a call that already got what it asked for.
        Some(row) if row.state == new_state => Ok(row),
        Some(row) => Err(invalid_params(&format!(
            "import session '{session_id}' is {}; cannot move to {new_state}",
            row.state
        ))),
    }
}

/// The wizard tried to start a second concurrent import of the same source
/// (`mailbox-migration.md` § Architectural rules — "this source is already
/// being imported on another device").
/// The caller already knows the source descriptor they submitted (it is
/// their own request field), but the error text drops it anyway — never a
/// plaintext or hashed form — rather than carry per-source detail in an
/// error string at all (`docs/goal/behavior/file-sync.md` § Sealed names &
/// paths, S7 the log + error-string scrub, design record § 3 "import lock +
/// list" row).
fn import_source_locked() -> fauna_protocol::RpcError {
    let mut e = fauna_protocol::RpcError::new(
        "fauna.bridges.import_source_locked",
        "error.bridges.import_source_locked",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(
        "an import of this source is already running or paused; resume or cancel it first"
            .to_string(),
    )));
    e
}

pub fn register_bridge_import_handlers(b: &mut RpcRouterBuilder) {
    let fetch = Duration::from_secs(5);
    // 60 s: batches carry up to 16 MiB of bodies (same rationale as APPEND).
    let routing = Duration::from_secs(60);
    // `forbid_replay: true` (81st pass) — this REVERSES the 2026-08-01 audit,
    // which read the same code, saw "at-most-once by the per-(actor, source)
    // lock", and filed it as the `writer_grant.revoke` wrong-answer-
    // never-double-apply shape. The at-most-once half is correct; the
    // classification is not, on two grounds that audit did not reach.
    //
    // (1) **The caller cannot key the recovery.** In the 76th pass's
    // consume-shaped class the caller supplies the key, so a lookup restores
    // the lost answer. Here the id is minted server-side (`Uuid::new_v4`), so a
    // replay mints a *second* id, trips the lock its own first call took, and
    // returns `import_source_locked` — leaving the caller with no id at all.
    // Reconciling means scanning `list_import_sessions` and *guessing* its own
    // row by source descriptor, which is a protocol the error does not state.
    //
    // (2) **The answer is not merely diverging, it is false.** The error says
    // an import "is already running or paused" — `mailbox-migration.md` §
    // Architectural rules renders it "on another device". After a replay the
    // competing device is the caller itself, so the nest confidently reports a
    // device that does not exist. That is strictly worse than the misleading-
    // answer class, which at least says nothing untrue.
    //
    // The lookup remedy is UNAVAILABLE: `SourceLocked` is also the genuine
    // second-device case, and once the id is server-minted nothing on the wire
    // separates a replay from a second device — answering with the existing
    // session's id would defeat exactly what the lock is for. So the only fix
    // that converges the answer is to stop the blind retry: the caller then
    // sees `RpcDisconnected { was_in_flight }` — the honest "don't know" that
    // *is* the trigger for the documented `list_import_sessions` resume path.
    //
    // Consistency check: `import_message{,_batch}` — steps 2..n of this same
    // flow — are already `true`. A flow whose first step permits a blind retry
    // while every later step forbids it was internally inconsistent.
    //
    // Hazard pin: `a_replayed_create_trips_its_own_lock_and_loses_the_session_id`.
    b.add(
        "fauna.bridges.start_import_session",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: fetch,
            handler: start_import_session_handler(),
        },
    );
    // NOT idempotent, two ways (audited 2026-08-01): (1) `record_import_progress`
    // is an accumulator (`imported_count = imported_count + ?` …), so even a
    // deduped replay recounts the message — the wizard's persisted progress
    // drifts from reality; (2) the guard is NARROWER THAN THE KIND (the
    // `invite_codes.create` rule): with `skip_dedup: true` there is no dedup
    // lookup at all, and a replayed call stores the same message AGAIN — real
    // user-visible mail duplication. The flag is per-kind, so the weakest
    // branch decides it. Interrupted imports are exactly what the session's
    // resume cursors exist for — the caller re-drives from
    // `list_import_sessions`, never from a blind wire retry.
    b.add(
        "fauna.bridges.import_message",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: routing,
            handler: import_message_handler(),
        },
    );
    // Same two grounds as `import_message` (the batch is a loop over the same
    // `import_one` + accumulator).
    b.add(
        "fauna.bridges.import_message_batch",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: routing,
            handler: import_message_batch_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_import_sessions",
        RpcKindMeta {
            // Pure read.
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_import_sessions_handler(),
        },
    );
    // The five state transitions below (pause/resume/cancel/finalize/fail) are
    // at-most-once by construction (audited 2026-08-01): each is one keyed
    // `UPDATE … WHERE state IN (allowed_from)`, and no transition's target
    // state is in its own allowed_from — so a replay that lands after the
    // first application matches zero rows and becomes a typed wrong-state
    // error, never a second application.
    b.add(
        "fauna.bridges.pause_import_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.pause_import_session",
                "paused",
                &["running"],
            ),
        },
    );
    b.add(
        "fauna.bridges.resume_import_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.resume_import_session",
                "running",
                &["paused"],
            ),
        },
    );
    b.add(
        "fauna.bridges.cancel_import_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.cancel_import_session",
                "cancelled",
                &["running", "paused"],
            ),
        },
    );
    b.add(
        "fauna.bridges.finalize_import_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.finalize_import_session",
                "completed",
                &["running"],
            ),
        },
    );
    b.add(
        "fauna.bridges.fail_import_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fail_import_session_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use fauna_protocol::encode_canonical;

    const USER: [u8; 32] = [7u8; 32];

    /// Fixture with a registered recipient seal key — the normal case. The
    /// import path fails closed without one (`import_fails_closed_without_a_
    /// registered_seal_key` uses `bare_fixture_state`).
    async fn fixture_state() -> Arc<AppState> {
        let state = bare_fixture_state().await;
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &USER,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
    }

    async fn bare_fixture_state() -> Arc<AppState> {
        let state = crate::test_support::fixture_state();
        // The dispatch gate (`caller_class_for_actor`) denies any actor without a
        // `users` row; every test here dispatches handlers as the plain `USER`, so
        // it needs a users row to be classed `CallerClass::User`. (`put_actor_mls_
        // pubkey` in `fixture_state` seeds only the seal key, not a users row.)
        state.db.create_user(&USER, "free", "test").await.unwrap();
        state
    }

    async fn start_session(state: &Arc<AppState>, source: &str) -> String {
        let req = StartImportSessionRequest {
            source_sealed: None,
            source_descriptor: source.into(),
            total_count: 10,
            scope: vec!["INBOX".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = start_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect("start ok");
        let reply: StartImportSessionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        reply.session_id
    }

    fn item(mailbox: &str, body: &[u8], source_uid: u32, dedup_key: &str) -> ImportMessageItem {
        ImportMessageItem {
            mailbox: mailbox.into(),
            flags: vec!["\\Seen".into()],
            body: body.to_vec(),
            timestamp: 1_700_000_000,
            body_size: body.len() as u32,
            sender_domain: "example.com".into(),
            source_uid,
            source_uid_validity: 9,
            dedup_key: dedup_key.into(),
            envelope_key: "env:v1:fixture".into(),
            ..Default::default()
        }
    }

    async fn import_single(
        state: &Arc<AppState>,
        session_id: &str,
        it: ImportMessageItem,
        skip_dedup: bool,
    ) -> Result<ImportMessageReply, fauna_protocol::RpcError> {
        let req = ImportMessageRequest {
            session_id: session_id.into(),
            message: it,
            skip_dedup,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = import_message_handler()(state.clone(), USER, payload).await?;
        Ok(fauna_cbor::decode_strict(&bytes).unwrap())
    }

    #[tokio::test]
    async fn start_then_import_lands_message_and_advances_session() {
        let state = fixture_state().await;
        let sid = start_session(&state, "gmail:imap.gmail.com:alice").await;

        let reply = import_single(
            &state,
            &sid,
            item("INBOX", b"Subject: hi\r\n\r\nbody", 44, "k1"),
            false,
        )
        .await
        .unwrap();
        let ImportMessageOutcome::Imported {
            uid,
            uid_validity,
            message_id,
        } = reply.outcome
        else {
            panic!("expected Imported, got {:?}", reply.outcome);
        };
        assert!(uid >= 1);
        assert!(uid_validity >= 1);
        assert_eq!(message_id.len(), 32);
        assert_eq!(reply.imported_count, 1);

        // The session row folded the progress + cursor.
        let row = state
            .db
            .get_import_session(&USER, &sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.imported_count, 1);
        assert_eq!(row.cursors.get("INBOX"), Some(&(44, 9)));

        // The message is a real placement — visible to the IMAP layer.
        let mb = state
            .db
            .get_bridge_imap_mailbox_state(&USER, "INBOX")
            .await
            .unwrap()
            .unwrap();
        assert!(mb.uid_next >= 2, "uid_next bumped by the placement");
    }

    /// Hazard pin for `import_message`'s `forbid_replay: true` (2026-08-01),
    /// asserting the *harm* rather than the flag so it stays meaningful if the
    /// handler is ever made idempotent: with `skip_dedup: true` there is no
    /// dedup lookup, so a same-message repeat — exactly what a blind wire
    /// retry would send — stores the message AGAIN and the session counts two
    /// imports. If this test ever fails because the repeat is refused or
    /// deduped, the handler has become idempotent on this branch — re-audit
    /// the flag with the accumulator ground in mind before flipping it back.
    #[tokio::test]
    async fn a_skip_dedup_repeat_stores_the_same_message_twice() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        let it = item("INBOX", b"Subject: dup\r\n\r\nbody", 7, "same-key");
        let first = import_single(&state, &sid, it.clone(), true).await.unwrap();
        let second = import_single(&state, &sid, it, true).await.unwrap();

        assert!(
            matches!(first.outcome, ImportMessageOutcome::Imported { .. }),
            "first import lands"
        );
        assert!(
            matches!(second.outcome, ImportMessageOutcome::Imported { .. }),
            "the repeat is NOT deduped — this double-store is why the kind is forbid_replay"
        );
        assert_eq!(second.imported_count, 2, "both copies were counted");
    }

    /// The accumulator half of the same audit: even with dedup ON, a repeated
    /// call is *recounted* — one message yields `imported + skipped = 2`
    /// processed in the persisted session row, so the wizard's progress
    /// arithmetic drifts under replay. (The dedup does protect the mail store
    /// itself: the repeat is `Skipped`.)
    #[tokio::test]
    async fn a_deduped_repeat_is_still_recounted() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        let it = item("INBOX", b"Subject: once\r\n\r\nbody", 8, "one-key");
        let first = import_single(&state, &sid, it.clone(), false)
            .await
            .unwrap();
        let second = import_single(&state, &sid, it, false).await.unwrap();

        assert!(matches!(
            first.outcome,
            ImportMessageOutcome::Imported { .. }
        ));
        assert!(
            matches!(second.outcome, ImportMessageOutcome::Skipped { .. }),
            "dedup catches the message itself"
        );
        let row = state
            .db
            .get_import_session(&USER, &sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.imported_count, row.skipped_count),
            (1, 1),
            "one message, two processed counts — the accumulator recounts a replay"
        );
    }

    /// Hazard pin for `start_import_session`'s `forbid_replay: true` (81st
    /// pass), asserting the *harm* rather than the flag, like its two siblings
    /// above. The session id is minted server-side (`Uuid::new_v4`), so a
    /// replayed create cannot land on the first call's id: it mints a second
    /// one, then trips the `(actor, source_descriptor)` lock its OWN first call
    /// took, and answers `import_source_locked` — whose text tells the user an
    /// import "is already running or paused" (the goal doc: "on another
    /// device"). A caller whose reply was lost to the reconnect therefore holds
    /// no session id and is told a falsehood about a device that does not
    /// exist.
    ///
    /// The lookup remedy the 76th pass prescribes for the consume-shaped class
    /// is UNAVAILABLE here: `SourceLocked` is also the genuine second-device
    /// case, and nothing on the wire distinguishes a replay from a second
    /// device once the id is server-minted — so answering with the existing
    /// session's id would defeat exactly what the lock is for. Hence the flip.
    ///
    /// If this test ever fails because the second create returns the first
    /// session's id, a caller-minted key has been introduced and the flag
    /// should be re-audited before flipping it back.
    #[tokio::test]
    async fn a_replayed_create_trips_its_own_lock_and_loses_the_session_id() {
        let state = fixture_state().await;
        let first = start_session(&state, "imap://mail.example.com/INBOX").await;

        // The blind auto-retry a `forbid_replay: false` kind would issue.
        let req = StartImportSessionRequest {
            source_sealed: None,
            source_descriptor: "imap://mail.example.com/INBOX".into(),
            total_count: 10,
            scope: vec!["INBOX".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = start_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect_err("the replay is refused by the lock the first call took");

        assert_eq!(
            err.code, "fauna.bridges.import_source_locked",
            "the caller's own in-flight session is reported as a competing import"
        );
        // The harm: the reply carries no way back to `first`.
        assert!(
            !format!("{:?}", err.details).contains(&first),
            "the lock error does not hand back the session id the caller lost"
        );
        let sessions = state.db.list_import_sessions(&USER).await.unwrap();
        assert_eq!(
            sessions.len(),
            1,
            "state converges — one session — so the defect is the answer, not a double-apply"
        );
    }

    /// The assertion that would have caught the defect:
    /// the record must rest as a sealed `MailRecordEnvelope`, never as the raw
    /// RFC 5322 bytes the untrusted caller sent — body AND index hint.
    #[tokio::test]
    async fn import_seals_body_and_index_hint_at_rest() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        let raw: &[u8] =
            b"Message-ID: <seal@example.com>\r\nSubject: secret words\r\n\r\nplaintext body";
        let reply = import_single(&state, &sid, item("INBOX", raw, 1, "k1"), false)
            .await
            .unwrap();
        let ImportMessageOutcome::Imported { message_id, .. } = reply.outcome else {
            panic!("expected Imported, got {:?}", reply.outcome);
        };

        let (env, _floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &USER,
            &message_id,
        )
        .await
        .unwrap()
        .expect("imported record must be readable at rest");
        assert!(
            fauna_mls::wrapped_blob::is_sealed_mail_record(&env.encrypted_body),
            "imported body must rest sealed in both storage modes \
             (encryption-at-rest.md S1 uniform seal at ingest)"
        );
        assert!(
            fauna_mls::wrapped_blob::is_sealed_mail_record(&env.encrypted_index_hint),
            "imported index hint must rest sealed (the PQ-6 word-set surface)"
        );
        let needle: &[u8] = b"plaintext body";
        assert!(
            !env.encrypted_body
                .windows(needle.len())
                .any(|w| w == needle),
            "sealed body must not contain the plaintext"
        );
    }

    /// A caller with no registered recipient seal key cannot store anything —
    /// fail-closed, same contract as `seal_and_persist_local`, never a silent
    /// unsealed write.
    #[tokio::test]
    async fn import_fails_closed_without_a_registered_seal_key() {
        let state = bare_fixture_state().await;
        let sid = start_session(&state, "src").await;
        let err = import_single(&state, &sid, item("INBOX", b"m", 1, "k"), false)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.email.invalid_params");
    }

    #[tokio::test]
    async fn second_import_of_same_key_dedup_skips_and_opt_out_overrides() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        let first = import_single(&state, &sid, item("INBOX", b"m1", 1, "k1"), false)
            .await
            .unwrap();
        assert!(matches!(
            first.outcome,
            ImportMessageOutcome::Imported { .. }
        ));

        // Same dedup key, different mailbox: skipped (§ Dedup scope — any
        // of the actor's mailboxes).
        let second = import_single(&state, &sid, item("Archive", b"m1", 2, "k1"), false)
            .await
            .unwrap();
        assert_eq!(
            second.outcome,
            ImportMessageOutcome::Skipped {
                reason: "dedup".into()
            }
        );
        assert_eq!(second.skipped_count, 1);

        // "Import duplicates anyway" bypasses the check (§ Opt-out).
        let third = import_single(&state, &sid, item("Archive", b"m1x", 3, "k1"), true)
            .await
            .unwrap();
        assert!(matches!(
            third.outcome,
            ImportMessageOutcome::Imported { .. }
        ));
    }

    // ── The envelope key confirms a Message-ID hit
    //    (`mailbox-migration.md` § The envelope key confirms a Message-ID hit).
    //    The row is seeded exactly as `ingest_inbound_mail` writes it — the MTA
    //    records the sender-chosen Message-ID first, whatever the body. ──

    const STRANGER_MSGID: &str = "msgid:v1:m@x.test";

    fn keyed_item(envelope_key: &str) -> ImportMessageItem {
        ImportMessageItem {
            envelope_key: envelope_key.to_string(),
            ..item(
                "INBOX",
                b"Message-ID: <m@x.test>\r\n\r\nreal",
                1,
                STRANGER_MSGID,
            )
        }
    }

    async fn seed_ingest_row(state: &Arc<AppState>, envelope_key: &str) {
        state
            .db
            .insert_dedup_key(&USER, STRANGER_MSGID, envelope_key, "mail://delivered")
            .await
            .unwrap();
    }

    /// The finding itself: a stranger who reuses the Message-ID of a message
    /// in the mailbox the user has not imported yet must not make the real
    /// message skip.
    #[tokio::test]
    async fn an_ingest_written_message_id_does_not_pre_empt_an_import_with_a_different_body() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;
        seed_ingest_row(&state, "env:v1:forged").await;

        let reply = import_single(&state, &sid, keyed_item("env:v1:real"), false)
            .await
            .unwrap();
        assert!(
            matches!(reply.outcome, ImportMessageOutcome::Imported { .. }),
            "a same-Message-ID message with a different envelope is a different \
             message and must be stored, got {:?}",
            reply.outcome
        );
        // First writer wins: the index still points at the delivered copy.
        assert_eq!(
            state
                .db
                .dedup_envelope_key(&USER, STRANGER_MSGID)
                .await
                .unwrap(),
            Some("env:v1:forged".to_string())
        );
    }

    /// Identical bytes still dedup: the ordinary resend/re-import case.
    #[tokio::test]
    async fn an_agreeing_envelope_key_still_dedup_skips() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;
        seed_ingest_row(&state, "env:v1:real").await;

        let reply = import_single(&state, &sid, keyed_item("env:v1:real"), false)
            .await
            .unwrap();
        assert_eq!(
            reply.outcome,
            ImportMessageOutcome::Skipped {
                reason: "dedup".into()
            }
        );
    }

    /// Every import records its envelope key beside the dedup key.
    #[tokio::test]
    async fn an_import_records_its_envelope_key() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;
        import_single(&state, &sid, keyed_item("env:v1:real"), false)
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .dedup_envelope_key(&USER, STRANGER_MSGID)
                .await
                .unwrap(),
            Some("env:v1:real".to_string())
        );
    }

    #[tokio::test]
    async fn batch_isolates_bad_messages_and_reports_outcomes_in_order() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        let mut bad = item("INBOX", b"m2", 2, "k2");
        bad.body_size = 999; // size mismatch → per-message Errored, not a call failure
        let req = ImportMessageBatchRequest {
            session_id: sid.clone(),
            messages: vec![
                item("INBOX", b"m1", 1, "k1"),
                bad,
                item("INBOX", b"m3", 3, "k3"),
            ],
            skip_dedup: false,
            revised_total_count: Some(500),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = import_message_batch_handler()(state.clone(), USER, payload)
            .await
            .expect("batch call ok");
        let reply: ImportMessageBatchReply = fauna_cbor::decode_strict(&bytes).unwrap();

        assert_eq!(reply.outcomes.len(), 3);
        assert!(matches!(
            reply.outcomes[0],
            ImportMessageOutcome::Imported { .. }
        ));
        assert!(matches!(
            reply.outcomes[1],
            ImportMessageOutcome::Errored { .. }
        ));
        assert!(matches!(
            reply.outcomes[2],
            ImportMessageOutcome::Imported { .. }
        ));
        assert_eq!(reply.imported_count, 2);
        assert_eq!(reply.errored_count, 1);

        let row = state
            .db
            .get_import_session(&USER, &sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.total_count, 500, "revised_total_count applied");
        // Cursor advanced past the errored message too (processed ≠ stored).
        assert_eq!(row.cursors.get("INBOX"), Some(&(3, 9)));
    }

    #[tokio::test]
    async fn batch_caps_enforced() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;
        let req = ImportMessageBatchRequest {
            session_id: sid,
            messages: (0..33).map(|i| item("INBOX", b"m", i, "k")).collect(),
            skip_dedup: false,
            revised_total_count: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = import_message_batch_handler()(state.clone(), USER, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn import_requires_running_session() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        // Pause, then import → invalid_params.
        let pause = ImportSessionActionRequest {
            session_id: sid.clone(),
        };
        let payload = Bytes::from(encode_canonical(&pause).unwrap().to_vec());
        session_transition_handler("fauna.bridges.pause_import_session", "paused", &["running"])(
            state.clone(),
            USER,
            payload,
        )
        .await
        .expect("pause ok");

        let err = import_single(&state, &sid, item("INBOX", b"m", 1, "k"), false)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.email.invalid_params");

        // Unknown session → not_found.
        let err = import_single(&state, "nope", item("INBOX", b"m", 1, "k"), false)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn source_lock_rejects_second_start_until_cancel() {
        let state = fixture_state().await;
        let _sid = start_session(&state, "gmail:imap.gmail.com:alice").await;

        let req = StartImportSessionRequest {
            source_sealed: None,
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 1,
            scope: vec!["INBOX".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = start_import_session_handler()(state.clone(), USER, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.import_source_locked");
        // S7 (the log + error-string scrub, `file-sync.md` § Sealed names &
        // paths): the lock-conflict error must never carry the source
        // descriptor, plaintext or otherwise (design record § 3 "import
        // lock + list" row: "drops the descriptor from its message").
        let details = match err.details.as_deref() {
            Some(fauna_protocol::Value::String(s)) => s.clone(),
            other => panic!("expected a string details value, got {other:?}"),
        };
        assert!(
            !details.contains("gmail"),
            "leaked source descriptor in lock error: {details}"
        );
        assert!(
            !details.contains("alice"),
            "leaked source descriptor in lock error: {details}"
        );
    }

    #[tokio::test]
    async fn lifecycle_finalize_and_fail_paths() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;

        // finalize: running → completed.
        let req = ImportSessionActionRequest {
            session_id: sid.clone(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = session_transition_handler(
            "fauna.bridges.finalize_import_session",
            "completed",
            &["running"],
        )(state.clone(), USER, payload)
        .await
        .expect("finalize ok");
        let reply: ImportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.session.state, "completed");

        // fail on a completed session → invalid_params (terminal state).
        let freq = FailImportSessionRequest {
            session_id: sid.clone(),
            reason: "auth_failed".into(),
        };
        let payload = Bytes::from(encode_canonical(&freq).unwrap().to_vec());
        let err = fail_import_session_handler()(state.clone(), USER, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.email.invalid_params");

        // fail on a fresh running session records the reason.
        let sid2 = start_session(&state, "src2").await;
        let freq = FailImportSessionRequest {
            session_id: sid2.clone(),
            reason: "auth_failed".into(),
        };
        let payload = Bytes::from(encode_canonical(&freq).unwrap().to_vec());
        let bytes = fail_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect("fail ok");
        let reply: ImportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.session.state, "errored");
        assert_eq!(reply.session.error_reason, "auth_failed");
    }

    /// The 76th pass's misleading-answer
    /// class, applied here. All five state-transition kinds declare
    /// `forbid_replay: false`, which `transport.md` § Idempotency and
    /// reconnect-with-resume defines as "an assertion that the handler
    /// itself is naturally idempotent" — but a second call landing after the
    /// session already reached its target state used to answer
    /// `invalid_params` (the keyed `UPDATE … WHERE state IN (allowed_from)`
    /// matches zero rows), even though the state the caller wanted had
    /// already converged. A transition into the state the session already
    /// holds must reply success with the current row; a transition that is
    /// genuinely impossible from the current state must still error.
    #[tokio::test]
    async fn a_transition_into_the_already_held_state_replies_success() {
        let state = fixture_state().await;

        async fn transition(
            state: &Arc<AppState>,
            kind: &'static str,
            new_state: &'static str,
            allowed_from: &'static [&'static str],
            sid: &str,
        ) -> Result<String, fauna_protocol::RpcError> {
            let req = ImportSessionActionRequest {
                session_id: sid.to_string(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let bytes = session_transition_handler(kind, new_state, allowed_from)(
                state.clone(),
                USER,
                payload,
            )
            .await?;
            let reply: ImportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
            Ok(reply.session.state)
        }

        let sid = start_session(&state, "src").await;

        // First pause: running → paused, a genuine transition.
        let after_first = transition(
            &state,
            "fauna.bridges.pause_import_session",
            "paused",
            &["running"],
            &sid,
        )
        .await
        .expect("first pause applies");
        assert_eq!(after_first, "paused");

        // A REPLAYED pause: `allowed_from = ["running"]` no longer matches
        // (the row is already "paused"), but "paused" is exactly the state
        // the caller asked for. Must succeed, not `invalid_params`.
        let after_replay = transition(
            &state,
            "fauna.bridges.pause_import_session",
            "paused",
            &["running"],
            &sid,
        )
        .await
        .expect("a replayed pause into the already-held state must succeed");
        assert_eq!(after_replay, "paused");

        // A genuinely impossible transition still errors: the session is
        // "cancelled", and "completed" is neither its current state nor
        // reachable from `allowed_from = ["running"]`.
        let sid2 = start_session(&state, "src2").await;
        transition(
            &state,
            "fauna.bridges.cancel_import_session",
            "cancelled",
            &["running", "paused"],
            &sid2,
        )
        .await
        .expect("cancel applies");
        let err = transition(
            &state,
            "fauna.bridges.finalize_import_session",
            "completed",
            &["running"],
            &sid2,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.email.invalid_params");

        // `fail_import_session` carries an extra `reason` field: a replay
        // must not overwrite the FIRST reason with whatever the replay
        // happens to carry — the row already converged on the original.
        let sid3 = start_session(&state, "src3").await;
        let freq = FailImportSessionRequest {
            session_id: sid3.clone(),
            reason: "auth_failed".into(),
        };
        let payload = Bytes::from(encode_canonical(&freq).unwrap().to_vec());
        let bytes = fail_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect("first fail applies");
        let reply: ImportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.session.state, "errored");
        assert_eq!(reply.session.error_reason, "auth_failed");

        let freq_replay = FailImportSessionRequest {
            session_id: sid3.clone(),
            reason: "a_different_reason".into(),
        };
        let payload = Bytes::from(encode_canonical(&freq_replay).unwrap().to_vec());
        let bytes = fail_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect("a replayed fail into the already-held state must succeed");
        let reply: ImportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.session.state, "errored");
        assert_eq!(
            reply.session.error_reason, "auth_failed",
            "a replay must not overwrite the original error_reason"
        );
    }

    #[tokio::test]
    async fn list_returns_caller_sessions_only() {
        let state = fixture_state().await;
        let _sid = start_session(&state, "src").await;

        let req = ListImportSessionsRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_import_sessions_handler()(state.clone(), USER, payload)
            .await
            .unwrap();
        let reply: ListImportSessionsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.sessions.len(), 1);

        // A different caller sees nothing.
        let other = [8u8; 32];
        state.db.create_user(&other, "free", "test").await.unwrap();
        let req = ListImportSessionsRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = list_import_sessions_handler()(state.clone(), other, payload)
            .await
            .unwrap();
        let reply: ListImportSessionsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(reply.sessions.is_empty());
    }

    #[tokio::test]
    async fn start_sessions_scope_reaches_the_list_reply() {
        // mailbox-migration.md § Resume protocol: `list_import_sessions` must
        // return the scope a resumed client re-`EXAMINE`s, end-to-end from
        // the `start_import_session` request through the DB row to the
        // `list_import_sessions` reply — not just at the DB layer.
        let state = fixture_state().await;
        let req = StartImportSessionRequest {
            source_sealed: None,
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 3,
            scope: vec!["INBOX".into(), "Sent".into(), "Archive".into()],
            date_from: "2023-11-14".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        start_import_session_handler()(state.clone(), USER, payload)
            .await
            .expect("start ok");

        let list_req = ListImportSessionsRequest {};
        let payload = Bytes::from(encode_canonical(&list_req).unwrap().to_vec());
        let bytes = list_import_sessions_handler()(state.clone(), USER, payload)
            .await
            .unwrap();
        let reply: ListImportSessionsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply.sessions.len(), 1);
        assert_eq!(
            reply.sessions[0].scope,
            vec![
                "INBOX".to_string(),
                "Sent".to_string(),
                "Archive".to_string()
            ]
        );
        // § Wizard steps step 3: the since date travels the same end-to-end
        // path, and for a sharper reason — a resume that reads it back as
        // empty imports every message the user excluded.
        assert_eq!(reply.sessions[0].date_from, "2023-11-14");
    }

    #[tokio::test]
    async fn bridge_callers_are_denied() {
        use crate::db::bridge_service_users::BridgeRole;
        let state = fixture_state().await;
        let mda = [9u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mda, BridgeRole::Mda, "b1")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&mda, &[9u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mda, None)
            .await
            .unwrap();

        let req = StartImportSessionRequest {
            source_sealed: None,
            source_descriptor: "src".into(),
            total_count: 1,
            scope: vec!["INBOX".into()],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = start_import_session_handler()(state.clone(), mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── S9.4: the staged-envelope import leg ────────────────────────────
    // (`smtp-server.md` § Message size limits, the staged-envelope rule;
    // `mailbox-migration.md` § RPC surface + the transport-ceiling block.)

    use fauna_protocol::bridge_routing::StagedBodyRef;

    /// `fixture_state` plus a real byte plane, so `resolve_staged_body` has a blob
    /// store to read staged ciphertext back out of.
    async fn fixture_state_with_blob_plane() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&USER, "free", "test").await.unwrap();
        crate::test_support::seed_recipient_seal_key(
            &db,
            &USER,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().to_path_buf();
        std::mem::forget(blob_dir); // outlive the call; never deleted under test
        let backup_svc = Arc::new(
            crate::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
                .unwrap(),
        );
        Arc::new(AppState {
            backup_service: Some(backup_svc),
            ..AppState::for_test(db)
        })
    }

    fn staged_item(staged: StagedBodyRef, body_size: u32, dedup_key: &str) -> ImportMessageItem {
        ImportMessageItem {
            mailbox: "INBOX".into(),
            flags: vec!["\\Seen".into()],
            body: Vec::new(), // empty: the bytes rode the byte plane
            timestamp: 1_700_000_000,
            body_size,
            sender_domain: "example.com".into(),
            source_uid: 1,
            source_uid_validity: 9,
            dedup_key: dedup_key.into(),
            envelope_key: "env:v1:fixture".into(),
            staged_body: Some(staged),
        }
    }

    /// The happy path: a body staged as a one-shot envelope resolves to the
    /// original plaintext and rests **sealed**, exactly as an inline import would.
    #[tokio::test]
    async fn a_staged_body_import_resolves_and_seals_the_plaintext_at_rest() {
        let state = fixture_state_with_blob_plane().await;
        let sid = start_session(&state, "src").await;

        let plaintext: Vec<u8> =
            b"Subject: staged\r\n\r\nthe secret staged plaintext body".to_vec();
        let sref = crate::mail_body_plane::stage_staged_body(&state, &plaintext)
            .await
            .expect("stage the ciphertext on the byte plane");

        let reply = import_single(
            &state,
            &sid,
            staged_item(sref, plaintext.len() as u32, "k-staged"),
            false,
        )
        .await
        .unwrap();
        let ImportMessageOutcome::Imported { message_id, .. } = reply.outcome else {
            panic!("expected Imported, got {:?}", reply.outcome);
        };

        // Rests sealed, and never carries the resolved plaintext in the clear.
        let (env, _floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &USER,
            &message_id,
        )
        .await
        .unwrap()
        .expect("imported record must be readable at rest");
        assert!(
            fauna_mls::wrapped_blob::is_sealed_mail_record(&env.encrypted_body),
            "a staged import must rest sealed, same as inline"
        );
        let needle: &[u8] = b"secret staged plaintext";
        assert!(
            !env.encrypted_body
                .windows(needle.len())
                .any(|w| w == needle),
            "sealed body must not contain the resolved plaintext"
        );
    }

    /// **The repeat walk** — a staged reference declaring a sealed total over the
    /// admission ceiling is refused as the typed `message_too_large` before a
    /// chunk is read. The reference names chunks that were never staged, so a
    /// resolver that read first would answer "could not be resolved" instead.
    #[tokio::test]
    async fn a_staged_body_declaring_over_the_ceiling_is_too_large_unread() {
        let state = fixture_state_with_blob_plane().await;
        let sid = start_session(&state, "src").await;
        let max_raw = fauna_mail::transport_limits::effective_max_raw_message_bytes(
            state
                .db
                .get_spam_policy()
                .await
                .unwrap()
                .effective()
                .max_message_bytes,
        );
        let sref = StagedBodyRef {
            chunk_hashes: vec![serde_bytes::ByteBuf::from(vec![0xabu8; 32]); 60_000],
            total_bytes: u64::from(max_raw)
                + fauna_mail::staged_envelope::STAGED_ENVELOPE_OVERHEAD_BYTES
                + 1,
            key: fauna_core::secret::SecretByteBuf::from(vec![0u8; 32]),
        };
        let reply = import_single(&state, &sid, staged_item(sref, 1, "k-big"), false)
            .await
            .unwrap();
        let ImportMessageOutcome::Errored { reason } = reply.outcome else {
            panic!("expected Errored, got {:?}", reply.outcome);
        };
        assert_eq!(reason, fauna_protocol::email::MESSAGE_TOO_LARGE_CODE);
    }

    /// Exactly-one-of: a `staged_body` alongside a non-empty inline `body` is a
    /// self-contradictory item — refused per-message, siblings untouched.
    #[tokio::test]
    async fn a_staged_body_with_a_non_empty_inline_body_is_rejected() {
        let state = fixture_state_with_blob_plane().await;
        let sid = start_session(&state, "src").await;
        let sref = crate::mail_body_plane::stage_staged_body(&state, b"Subject: x\r\n\r\nbody")
            .await
            .unwrap();
        let mut it = staged_item(sref, 18, "k");
        it.body = b"also inline".to_vec(); // both set → contradiction
        let reply = import_single(&state, &sid, it, false).await.unwrap();
        let ImportMessageOutcome::Errored { reason } = reply.outcome else {
            panic!("expected Errored, got {:?}", reply.outcome);
        };
        assert!(
            reason.contains("body must be empty when staged_body is set"),
            "{reason}"
        );
    }

    /// Skew contract: an empty inline body with NO `staged_body`
    /// — exactly what an older nest sees after dropping the unknown field — must
    /// refuse loudly per-message, never import an empty message.
    #[tokio::test]
    async fn an_empty_body_with_no_staged_ref_is_refused_loudly() {
        let state = fixture_state().await;
        let sid = start_session(&state, "src").await;
        let reply = import_single(&state, &sid, item("INBOX", b"", 1, "k"), false)
            .await
            .unwrap();
        let ImportMessageOutcome::Errored { reason } = reply.outcome else {
            panic!("expected Errored, got {:?}", reply.outcome);
        };
        assert!(reason.contains("body must not be empty"), "{reason}");
    }

    /// `body_size` is checked against the RESOLVED plaintext length, not the
    /// (empty) inline body — so a lying `body_size` is caught after the open.
    #[tokio::test]
    async fn a_staged_body_size_must_equal_the_resolved_plaintext_length() {
        let state = fixture_state_with_blob_plane().await;
        let sid = start_session(&state, "src").await;
        let plaintext = b"Subject: s\r\n\r\nreal length".to_vec();
        let sref = crate::mail_body_plane::stage_staged_body(&state, &plaintext)
            .await
            .unwrap();
        // Claim a wrong plaintext length.
        let reply = import_single(
            &state,
            &sid,
            staged_item(sref, plaintext.len() as u32 + 999, "k"),
            false,
        )
        .await
        .unwrap();
        let ImportMessageOutcome::Errored { reason } = reply.outcome else {
            panic!("expected Errored, got {:?}", reply.outcome);
        };
        assert!(reason.contains("body_size mismatch"), "{reason}");
    }

    /// Admission: the shared `effective_max_raw_message_bytes` rule refuses a body
    /// over the ceiling (here a small admin `max_message_bytes` override), so
    /// import enforcement tracks the same limit the SMTP `Data` paths clamp.
    #[tokio::test]
    async fn an_over_ceiling_body_is_refused_by_the_shared_admission_rule() {
        let state = fixture_state().await;
        state
            .db
            .put_spam_policy(crate::db::mail_policy::SpamPolicyOverrides {
                max_message_bytes: Some(64),
                ..Default::default()
            })
            .await
            .unwrap();
        let sid = start_session(&state, "src").await;
        let big = vec![b'x'; 200]; // 200 > the 64-byte ceiling
        let reply = import_single(&state, &sid, item("INBOX", &big, 1, "k"), false)
            .await
            .unwrap();
        let ImportMessageOutcome::Errored { reason } = reply.outcome else {
            panic!("expected Errored, got {:?}", reply.outcome);
        };
        assert_eq!(
            reason,
            fauna_protocol::email::MESSAGE_TOO_LARGE_CODE,
            "an over-ceiling import must carry the shared typed too-large code, \
             the same identifier `fauna.email.send` returns"
        );
    }

    /// A bad staged reference is THIS message's fault — a per-message error that
    /// lets its inline sibling import (§ Batching: one bad message doesn't strand
    /// the others). Distinct from an infra failure, which would abort the call.
    #[tokio::test]
    async fn a_bad_staged_reference_errors_that_message_but_imports_its_siblings() {
        let state = fixture_state_with_blob_plane().await;
        let sid = start_session(&state, "src").await;

        // A reference to a chunk that was never staged.
        let bogus = StagedBodyRef {
            chunk_hashes: vec![serde_bytes::ByteBuf::from(vec![0xEEu8; 32])],
            total_bytes: 100,
            key: fauna_core::secret::SecretByteBuf::from(vec![0u8; 32]),
        };
        let req = ImportMessageBatchRequest {
            session_id: sid.clone(),
            messages: vec![
                staged_item(bogus, 100, "k-bad"),
                item("INBOX", b"Subject: ok\r\n\r\ninline sibling", 2, "k-ok"),
            ],
            skip_dedup: false,
            revised_total_count: None,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = import_message_batch_handler()(state.clone(), USER, payload)
            .await
            .expect("the call itself succeeds — the bad message is per-message data");
        let reply: ImportMessageBatchReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(reply.outcomes[0], ImportMessageOutcome::Errored { .. }),
            "the bogus staged reference errors that message, got {:?}",
            reply.outcomes[0]
        );
        assert!(
            matches!(reply.outcomes[1], ImportMessageOutcome::Imported { .. }),
            "its inline sibling still imports, got {:?}",
            reply.outcomes[1]
        );
    }
}
