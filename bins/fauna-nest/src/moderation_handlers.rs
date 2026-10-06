//! WS-RPC handlers for the user-facing moderation surface —
//! `fauna.moderation.{stats,actions,appeal,train}` and the report/signal kinds.
//! A faithful transport migration of the `/api/v1/moderation/*` HTTP routes;
//! the migratable HTTP twins were **DELETED** in the WS-RPC-everywhere rip
//! (`moderation_routes.rs` gone). No moderation route stays HTTP.
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (`User | Admin` arms — moderation is end-user-facing; bridge actors have
//! no role). Confidence scores ride as per-mille `u16` (the dag-cbor wire
//! forbids floats; the spam-preferences precedent).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_core::carried::CarriedValue;
use fauna_protocol::moderation::MAX_APPEAL_REASON_BYTES;
use fauna_protocol::moderation::{
    AbuseReportMineEntry, AbuseReportMineReply, AbuseReportMineRequest, AbuseReportOutcome,
    AbuseReportQueueEntry, AbuseReportQueueReply, AbuseReportQueueRequest, AbuseReportReason,
    AbuseReportResolveReply, AbuseReportResolveRequest, AbuseReportStatus, AbuseReportSubject,
    AbuseReportSubmitReply, AbuseReportSubmitRequest, AbuseReportWithdrawReply,
    AbuseReportWithdrawRequest, MAX_ABUSE_REPORT_EXCERPT_BYTES, MAX_ABUSE_REPORT_NOTE_BYTES,
};
use fauna_protocol::moderation::{
    LabelStat, ModerationActionsReply, ModerationActionsRequest, ModerationAppealReply,
    ModerationAppealRequest, ModerationLegalTakedownReply, ModerationLegalTakedownRequest,
    ModerationReportShareSetReply, ModerationReportShareSetRequest,
    ModerationReportShareStatusReply, ModerationReportShareStatusRequest,
    ModerationSignalContributeReply, ModerationSignalContributeRequest,
    ModerationSignalShareSetReply, ModerationSignalShareSetRequest,
    ModerationSignalShareStatusReply, ModerationSignalShareStatusRequest, ModerationStatsReply,
    ModerationStatsRequest, ModerationTrainReply, ModerationTrainRequest, ObligationAction,
    ReportShareEntry,
};
use fauna_protocol::spam::probability_to_per_mille;
use fauna_protocol::{LocalizedText, RpcError, Value, decode_strict as decode};

use crate::db::moderation::{
    AbuseReportInsert, AbuseReportResolve, AbuseReportRow, AbuseReportWithdraw, AppealRecord,
    AppealSubject, NewAbuseReport,
};
use crate::db::notifications::NotificationText;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── helpers ────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("moderation", reason)
}

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("moderation", reason)
}

fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited_ns("moderation")
}

fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("moderation", reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.moderation.stats ─────────────────────────────────────────────

fn stats_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.stats").await?;
            let _req: ModerationStatsRequest = decode(&payload).map_err(malformed)?;
            let stats = state.db.get_label_stats().await.map_err(|e| {
                tracing::error!("get_label_stats error: {e}");
                internal("storage error")
            })?;
            let labels = stats
                .into_iter()
                .map(|(category, count, avg_confidence)| LabelStat {
                    category,
                    count,
                    avg_confidence_per_mille: probability_to_per_mille(avg_confidence),
                    extra: BTreeMap::new(),
                })
                .collect();
            encode_reply(&ModerationStatsReply {
                labels,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.actions ───────────────────────────────────────────

fn actions_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.actions").await?;
            // The connection actor is the subject (the HTTP twin's `?actor=`
            // any-actor query is dropped).
            let _req: ModerationActionsRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);
            let rows = state
                .db
                .get_obligation_actions_for_author(&actor_hex)
                .await
                .map_err(|e| {
                    tracing::error!("get_obligation_actions_for_author error: {e}");
                    internal("storage error")
                })?;
            let actions = rows
                .into_iter()
                .map(|a| ObligationAction {
                    id: a.id,
                    content_type: a.content_type,
                    content_id: a.content_id,
                    category: a.category,
                    confidence_per_mille: probability_to_per_mille(a.confidence),
                    action: a.action_taken,
                    timestamp: a.timestamp,
                    extra: BTreeMap::new(),
                })
                .collect();
            encode_reply(&ModerationActionsReply {
                actions,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.appeal ────────────────────────────────────────────

fn appeal_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.appeal").await?;
            let req: ModerationAppealRequest = decode(&payload).map_err(malformed)?;
            if req.content_id.is_empty() {
                return Err(invalid_params("content_id required"));
            }
            if req.reason.is_empty() {
                return Err(invalid_params("reason required"));
            }
            // The appeal lands on the permanent, un-prunable audit chain, so its
            // free text is bounded by the constant the shared client guard
            // renders too — refused before any read or write.
            if req.reason.len() > MAX_APPEAL_REASON_BYTES {
                return Err(invalid_params("reason too long"));
            }
            // One spelling per record: a 32-byte hex id is keyed lowercase, so a
            // case-varied repeat is the same (appellant, content_id) pair to the
            // pending bound below, and matches the takedown's own obligation
            // row. Any other id matches only an obligation row, verbatim.
            let content_id = match fauna_core::hex32::decode(&req.content_id) {
                Ok(id) => hex::encode(id),
                Err(_) => req.content_id.clone(),
            };
            // An appeal is the second leg of the transparency triple
            // (`moderation.md` § Legal takedown) — the handle owed to the author
            // of content an enforcement action was taken against. Without this
            // gate the handler audit-logs any string a caller sends, which is
            // not an appeal trail but an unauthenticated write surface onto it.
            //
            // `appeal_subject` owns what counts (post obligation row,
            // `content_meta` flag, the post-delete floor row, or a conv
            // record's `segment_records.legal_takedown_ref`) and whose appeal it
            // is: a post-side record names its author, and only they appeal; a
            // conv record persists no sender, so any caller holding its id does.
            let subject = state
                .db
                .appeal_subject(&content_id)
                .await
                .map_err(|e| {
                    tracing::error!("appeal_subject error: {e}");
                    internal("storage error")
                })?
                .ok_or_else(|| not_found("no enforcement record for this content"))?;
            if let AppealSubject::Author(author) = subject
                && author != actor_id
            {
                return Err(permission_denied(
                    "only the author may appeal this decision",
                ));
            }
            // At most one pending appeal per (caller, content): a repeat before
            // the next decision collapses onto the recorded one and writes
            // nothing — idempotent for a retry, and no chain growth per call.
            let recorded = state
                .db
                .record_appeal(&content_id, &actor_id, &req.reason)
                .await
                .map_err(|e| {
                    tracing::error!("record_appeal error: {e}");
                    internal("storage error")
                })?;
            let status = match recorded {
                AppealRecord::Recorded => "appeal_recorded",
                AppealRecord::AlreadyPending => "appeal_already_recorded",
            };
            encode_reply(&ModerationAppealReply {
                status: status.into(),
                content_id: req.content_id,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.train ─────────────────────────────────────────────

/// `fauna.moderation.train` — the NEST half of a training correction on a post.
/// It no longer trains: the per-user spam model rests sealed and only a
/// capability holder mutates it, so the model half is the client's own sealed
/// write (`fauna.bridges.put_spam_model`). What stays here: the permission
/// check, the per-actor rate limit, request validation, the read gate through
/// `get_post_core` (a taken-down, quarantined-to-this-caller or absent post is
/// the SAME `not_found`), and the opt-in report capture (`report-sharing.md`
/// § Report capture). The reply is unchanged — `status: "trained"`
/// acknowledges the correction.
fn train_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.train").await?;
            // Per-actor rate-limit the correction: each call
            // fetches + decodes a post and may recompute a report aggregate, so
            // a User looping it is bounded. Keyed by the authenticated caller.
            if !state
                .spam_train_rate_limit
                .check(&actor_id, &actor_id, "moderation.train")
            {
                return Err(rate_limited());
            }
            let req: ModerationTrainRequest = decode(&payload).map_err(malformed)?;
            if req.verdict != "spam" && req.verdict != "ham" {
                return Err(invalid_params("verdict must be \"spam\" or \"ham\""));
            }
            let post_id_bytes: [u8; 32] = fauna_core::hex32::decode(&req.content_id)
                .map_err(|_| invalid_params("content_id must be 32-byte hex"))?;

            // a verdict is only recorded on a post the caller may READ, so the
            // post goes THROUGH the post-read core every post serve path shares
            // (`get_post_core`), never the flag-blind primitive, and inherits
            // both gates in the core's order — the legal takedown first
            // (withheld from every caller, author and admin included;
            // `moderation.md` § Legal takedown → *Posts*), then quarantine
            // (author/admin only; `mail-spam.md` § Cross-actor isolation).
            // Every withheld outcome is reported as the SAME `not_found` as an
            // absent post, so a quarantined post leaks no existence signal and
            // a withheld one captures no report.
            match crate::routes::get_post_core(&state, Some(actor_id), post_id_bytes).await {
                crate::routes::GetPostOutcome::Found(_) => {}
                crate::routes::GetPostOutcome::LegalTakedown { .. }
                | crate::routes::GetPostOutcome::NotFound => {
                    return Err(not_found("post not found"));
                }
                crate::routes::GetPostOutcome::Error => {
                    return Err(internal("storage error"));
                }
            }

            // Report capture (report-sharing.md § Report capture): the
            // explicit flag emits/withdraws a k-anonymized report row — a
            // post's content-addressed id IS its report-hash. Never fails the
            // call (log-and-continue).
            let report_key = crate::db::reports::ReportKey {
                content_hash: post_id_bytes,
                factor: fauna_core::scoring::factor::REPORT_SPAM.to_string(),
                content_kind: "post".to_string(),
            };
            if let Err(e) = crate::db::reports::capture_report(
                &state.db,
                &actor_id,
                &report_key,
                req.verdict == "spam",
            )
            .await
            {
                tracing::warn!("report capture failed: {e}");
            } else {
                // The local aggregate may have transitioned — nudge the
                // federation exchange originator's debounced push.
                state.notify_exchange_transition();
            }

            encode_reply(&ModerationTrainReply {
                status: "trained".into(),
                verdict: req.verdict,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.legal_takedown ────────────────────────────────────

/// The narrow legal-compulsion social-takedown carve-out (`moderation.md`
/// § Categories & enforcement item 1 / `content-moderation-and-ranking.md` Q5).
/// Admin-gated (allowlist: `matches!(class, Admin)`) — the admin is the
/// deployment's legal-compliance responder (there is no operator). It is
/// **structurally incapable of being a "remove for policy" lever** by four
/// guards enforced here: `restore == false` **requires a non-empty
/// `legal_reference`**; the takedown produces a **visible tombstone** (the
/// withheld body + the `legal_takedown_ref` the serve path renders); an
/// `obligation_action_records` row (`ObligationAction::TakenDown`) surfaces it
/// to the author's queue; and a permanent **audit** row records who did it and
/// why. It is **never** a silent removal. `restore == true` overturns an upheld
/// appeal — it clears the flag (tombstone-not-delete, so the content row
/// survives and re-serves) and audits the overturn; the historical takedown row
/// stays (appeals + labels are additive history, `moderation.md` § Persistence).
/// Conversation half of the legal-obligation takedown — **best-effort relay
/// withholding** of an E2E MLS message (`moderation.md` § Categories &
/// enforcement item 1 / `content-moderation-and-ranking.md` Q5; the "posts /
/// conversations" scope). The nest relays sealed blobs it cannot read, so this
/// is intentionally weaker than the post case:
///
/// - **In reach:** the per-record serve paths the nest addresses by
///   `record_cid` (`channel.fetch` local, `federation.channel.fetch` peer,
///   `federation.mls.pull` paired). Setting `segment_records.legal_takedown_ref`
///   withholds the sealed record from **future** fetches there and carries the
///   tombstone into the thread (`ChannelFetchEntry.legal_takedown`).
/// - **Out of reach (documented, not a bug):** content **already delivered** to
///   a device that synced before the takedown (E2E — the nest cannot recall it),
///   and the **opaque client-sealed snapshot/replica blobs** (`mls_replica`,
///   sealed under the client's own `BackupKey` — the nest cannot surgically
///   remove one message from an aggregate blob it cannot read).
///
/// **No obligation-queue row** (unlike the post path): conv records persist no
/// sender (`segment_records_insert_conv`), so the nest cannot attribute a stored
/// sealed message to an author post-hoc and there is no author to key
/// `obligation_action_records` to. The **in-thread tombstone** (every member
/// sees it on `channel.fetch`) is the member-visible transparency surface, and
/// `fauna.moderation.appeal` accepts the `content_id` from any member, so the
/// required transparency triple (tombstone, appeal, audit) holds. Structural
/// guards otherwise mirror the post path: takedown demands a non-empty
/// `legal_reference`; `restore` overturns (tombstone-not-delete — the record
/// survives and re-serves); a permanent audit row records who and why.
async fn conversation_legal_takedown(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    req: &ModerationLegalTakedownRequest,
) -> Result<Bytes, RpcError> {
    // `content_id` is the 32-byte hex `record_id` of the conv message; its
    // `record_cid` is the dag-cbor Cid over that digest (the segment mirror key).
    let record_id: [u8; 32] = fauna_core::hex32::decode(&req.content_id)
        .map_err(|_| invalid_params("content_id must be 32-byte hex"))?;
    let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
    // What the decision records is the id in its one spelling (lowercase hex),
    // whatever the admin sent: `record_appeal` finds the last decision by exact
    // `target`, so a re-spelled overturn would otherwise leave the pending
    // appeal pending (`moderation.md` § Errors & edge cases, the appeal bullet).
    // The reply still echoes the caller's own spelling.
    let content_id_hex = hex::encode(record_id);

    // The message must exist as a conv record (also yields the channel for the
    // audit context). Absent → not_found (nothing to withhold).
    let (channel, _current_ref) = state
        .db
        .conv_record_scope_and_takedown(&record_cid)
        .await
        .map_err(|e| {
            tracing::error!("conv_record_scope_and_takedown error: {e}");
            internal("storage error")
        })?
        .ok_or_else(|| not_found("conversation message not found"))?;
    let channel_hex = hex::encode(channel);
    let admin_hex = hex::encode(actor_id);

    if req.restore {
        // Overturn: clear the flag (the message re-serves) + audit, atomically
        // (one transaction — the flag write is asserted to have matched the
        // record, and neither the flag nor the audit row lands without the
        // other; review 2026-07-06 §§ F1/F4).
        state
            .db
            .conv_legal_takedown_txn(
                &record_cid,
                &content_id_hex,
                None,
                &format!(
                    "admin={admin_hex} channel={channel_hex} kind=conversation reason={}",
                    req.legal_reference
                ),
            )
            .await
            .map_err(|e| {
                tracing::error!("conv legal-takedown restore txn error: {e}");
                internal("storage error")
            })?;
        return encode_reply(&ModerationLegalTakedownReply {
            status: "restored".into(),
            content_id: req.content_id.clone(),
            extra: BTreeMap::new(),
        });
    }

    // Takedown: a legal-obligation reference is MANDATORY — the structural guard
    // that this is compulsion, not policy/opinion (mirrors the post path).
    if req.legal_reference.trim().is_empty() {
        return Err(invalid_params(
            "legal_reference required for a takedown (the reference is what makes this legal compulsion, not policy)",
        ));
    }

    // Withhold the sealed record from future relay fetches + carry the
    // tombstone, and write the permanent audit row (who, which channel, the
    // legal reference) — atomically, in one transaction: `"taken_down"` is
    // never replied or audited unless the flag write actually matched the
    // record, and a withheld record always has its audit row (§§ F1/F4). No
    // obligation-action row — see the fn doc (E2E sender-unattributable).
    state
        .db
        .conv_legal_takedown_txn(
            &record_cid,
            &content_id_hex,
            Some(&req.legal_reference),
            &format!(
                "admin={admin_hex} channel={channel_hex} kind=conversation reference={}",
                req.legal_reference
            ),
        )
        .await
        .map_err(|e| {
            tracing::error!("conv legal-takedown txn error: {e}");
            internal("storage error")
        })?;

    encode_reply(&ModerationLegalTakedownReply {
        status: "taken_down".into(),
        content_id: req.content_id.clone(),
        extra: BTreeMap::new(),
    })
}

fn legal_takedown_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let reply = legal_takedown_core(state.clone(), actor_id, payload).await?;
            // The flag just moved in one direction or the other, so the
            // derived blob-serve withhold is now stale. Rebuild it here,
            // synchronously, before replying: the admin's confirm returning
            // "taken_down" must mean the attachment door is already shut, not
            // that it will shut at the next sweep (`moderation.md` § Legal
            // takedown -> *The blob-serve door*). Non-fatal on its own — the
            // record's own withhold, the obligation row and the audit row all
            // landed atomically inside the call above, and every complete blob
            // GC sweep rebuilds this set again.
            if let Err(e) = crate::moderation_withhold::recompute(
                &state.db,
                crate::backup::gc::PostBodySource {
                    segments: &state.post_segments,
                },
            )
            .await
            {
                tracing::error!(
                    "rebuilding the legal-takedown blob withhold after a flag change: {e}"
                );
            }
            Ok(reply)
        })
    })
}

/// The takedown/restore act itself — everything that must land atomically with
/// the flag. Split out from [`legal_takedown_handler`] so the derived
/// blob-withhold rebuild runs exactly once, on the one path where the flag
/// actually changed, rather than being repeated before each of the four
/// replies (post takedown / post restore, and the conversation twins).
async fn legal_takedown_core(
    state: Arc<AppState>,
    actor_id: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    require_permission(&state, &actor_id, "fauna.moderation.legal_takedown").await?;
    let req: ModerationLegalTakedownRequest = decode(&payload).map_err(malformed)?;

    // Conversation takedown is the MLS relay-withhold half — separate
    // storage (`segment_records.legal_takedown_ref`, keyed on the
    // message's record_cid) and no obligation-queue row (the nest cannot
    // attribute a sealed E2E message to a sender). Handled entirely in
    // its own path, returning early.
    if req.content_type == "conversation" {
        return conversation_legal_takedown(&state, &actor_id, &req).await;
    }

    // Besides conversations above, only social posts are withholdable at
    // serve time today; a future kind must add its own serve-withhold
    // path before it can be named.
    if req.content_type != "post" {
        return Err(invalid_params(
            "content_type must be \"post\" or \"conversation\"",
        ));
    }
    let post_id: [u8; 32] = fauna_core::hex32::decode(&req.content_id)
        .map_err(|_| invalid_params("content_id must be 32-byte hex"))?;
    // One spelling per record — see `conversation_legal_takedown`.
    let content_id_hex = hex::encode(post_id);
    let admin_hex = hex::encode(actor_id);
    let now_us = fauna_core::data::Timestamp::now_or_zero().as_i64();

    // The post must exist to be taken down (also gives us the author to
    // surface the obligation row on). `None` here is either a post this
    // nest never held, or one its own author already deleted — the
    // `content` row this reads is exactly what a delete removes
    // (`moderation.md` § Legal takedown → *Posts*, "deleted, then the
    // order arrives"); the split-out arm below tells the two apart and,
    // for the latter, still finds the bytes through the segment's
    // tombstone-inclusive lookup.
    let author = match state.db.get_content_author(&post_id).await.map_err(|e| {
        tracing::error!("get_content_author error: {e}");
        internal("storage error")
    })? {
        Some(a) => a,
        None => {
            return legal_takedown_of_deleted_post(
                &state, actor_id, &admin_hex, &post_id, &req, now_us,
            )
            .await;
        }
    };
    let author_hex = hex::encode(author);

    if req.restore {
        // Overturn: clear the flag (the content re-serves) + audit,
        // atomically (one transaction — the flag write is asserted to
        // have matched a `content_meta` row, and neither the flag nor
        // the audit row lands without the other; review 2026-07-06
        // §§ F1/F4). The historical takedown obligation row is kept
        // (additive history).
        state
            .db
            .post_legal_takedown_txn(
                &post_id,
                &content_id_hex,
                None,
                &author,
                actor_id.as_slice(),
                &format!("admin={admin_hex} reason={}", req.legal_reference),
                now_us,
            )
            .await
            .map_err(|e| {
                tracing::error!("post legal-takedown restore txn error: {e}");
                internal("storage error")
            })?;
        // The author's web site is a pull surface this nest serves, like the
        // ActivityPub outbox (which re-serves on the cleared flag with no act of
        // its own): the author's per-post publish link is the consent, and a
        // takedown never withdrew it. So the overturn re-renders and the post
        // reappears — it does not wait for some unrelated render to notice.
        rerender_web_site_after_flag_change(&state, &author, &post_id).await;
        return encode_reply(&ModerationLegalTakedownReply {
            status: "restored".into(),
            content_id: req.content_id,
            extra: BTreeMap::new(),
        });
    }

    // Takedown: a legal-obligation reference is MANDATORY — this is the
    // structural guard that this is compulsion, not policy/opinion.
    if req.legal_reference.trim().is_empty() {
        return Err(invalid_params(
            "legal_reference required for a takedown (the reference is what makes this legal compulsion, not policy)",
        ));
    }

    // One transaction for all three writes (§§ F1/F4 — `"taken_down"`
    // is never replied or audited unless the withhold flag actually
    // landed on a `content_meta` row, and a withheld post always has
    // its transparency rows):
    // 1) the withhold flag + tombstone reference (serve gate),
    // 2) the obligation-action row so the author sees it in their
    //    queue (`fauna.moderation.actions`) and can appeal
    //    (`category="illegal"` is the enforcement descriptor, NOT a
    //    classifier emission; `obligation_id` records the issuing
    //    admin; confidence 1.0 = a definitive legal action),
    // 3) the permanent transparent audit row (who, what, reference).
    state
        .db
        .post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            Some(&req.legal_reference),
            &author,
            actor_id.as_slice(),
            &format!(
                "admin={admin_hex} author={author_hex} reference={}",
                req.legal_reference
            ),
            now_us,
        )
        .await
        .map_err(|e| {
            tracing::error!("post legal-takedown txn error: {e}");
            internal("storage error")
        })?;

    // Nostr retraction — a derived relay event must not outlive a post
    // the takedown just made unservable (kind-5 leg, `moderation.md`
    // § Legal takedown; same lifecycle rule as `delete_post_core`'s
    // leg 4). Non-fatal — the takedown stands; the sync worker's
    // reconcile sweep re-chases anything this call leaves behind.
    #[cfg(feature = "nostr")]
    if let Err(e) = crate::nostr::propagate_post_delete(&state, &author, &post_id).await {
        tracing::warn!("nostr kind-5 takedown propagation: {e}");
    }

    // The author's web site: the render's enumeration now skips the post, but
    // pages already rendered keep serving it until something re-renders — and
    // nothing periodic does. So re-render here, before the reply, the way the
    // blob withhold is rebuilt before it: "taken_down" means the site is shut.
    rerender_web_site_after_flag_change(&state, &author, &post_id).await;

    encode_reply(&ModerationLegalTakedownReply {
        status: "taken_down".into(),
        content_id: req.content_id,
        extra: BTreeMap::new(),
    })
}

/// The post arm of [`legal_takedown_core`] when `get_content_author`
/// answered `None` for `post_id`: either this nest never held the post, or
/// its own author deleted it — possibly before a compelled order against it
/// could even land (`moderation.md` § Legal takedown → *Posts*, "deleted,
/// then the order arrives"; the mirror-image ordering — takedown, then
/// delete — is what [`legal_takedown_core`]'s live branch above and
/// `delete_post_core`'s own capture handle instead). Split out so the live
/// flow above stays a straight-line read of the common case.
///
/// `restore` and a fresh takedown read different tables: a restore only
/// needs to know the post was EVER recorded in `legal_takedown_deleted_posts`
/// (permanent once written — "the row is permanent and inert"), while a
/// takedown needs the bytes themselves, which only the segment's
/// tombstone-inclusive lookup can still resolve, and only until compaction
/// reclaims them.
async fn legal_takedown_of_deleted_post(
    state: &Arc<AppState>,
    actor_id: [u8; 32],
    admin_hex: &str,
    post_id: &[u8; 32],
    req: &ModerationLegalTakedownRequest,
    now_us: i64,
) -> Result<Bytes, RpcError> {
    // One spelling per record — see `conversation_legal_takedown`.
    let content_id_hex = hex::encode(post_id);
    if req.restore {
        // An overturn against a post the delete already outran the order
        // to: there is no `content_meta` flag to clear and no body to
        // re-serve (both left with the author's own delete), so this is
        // audit-only. The `legal_takedown_deleted_posts` row stays exactly
        // as permanent and inert as `moderation.md` § Legal takedown →
        // *Posts* rules it — the bytes it withholds are already the
        // author's own delete's to account for, so clearing anything here
        // would buy nothing. A post never recorded there is a genuinely
        // unknown restore target.
        state
            .db
            .get_taken_down_deleted_post(post_id)
            .await
            .map_err(|e| {
                tracing::error!("get_taken_down_deleted_post error: {e}");
                internal("storage error")
            })?
            .ok_or_else(|| not_found("post not found"))?;
        state
            .db
            .audit(
                None,
                "moderation:legal-takedown-restore",
                Some(&content_id_hex),
                Some(&format!(
                    "admin={admin_hex} reason={} (post already deleted by its author; \
                     the legal_takedown_deleted_posts floor is unchanged — moderation.md \
                     § Legal takedown -> Posts)",
                    req.legal_reference
                )),
            )
            .await
            .map_err(|e| {
                tracing::error!("audit error: {e}");
                internal("storage error")
            })?;
        return encode_reply(&ModerationLegalTakedownReply {
            status: "restored".into(),
            content_id: req.content_id.clone(),
            extra: BTreeMap::new(),
        });
    }

    // Takedown: a legal-obligation reference is MANDATORY — the same
    // structural guard as the live path.
    if req.legal_reference.trim().is_empty() {
        return Err(invalid_params(
            "legal_reference required for a takedown (the reference is what makes this legal compulsion, not policy)",
        ));
    }

    // Is there still a segment record naming the bytes? The live-only
    // lookup answers None for exactly this record — the delete tombstoned
    // the mirror row the instant it landed — so only the tombstone-
    // inclusive lookup can still find it. Note this lookup alone does NOT
    // prove the bytes still exist: `segment_records` rows are tombstoned,
    // never deleted, so it stays `Some` even once physical reclaim has
    // dropped the segment file out from under it — the READ below is what
    // actually answers that question.
    let Some((author, _segment_id)) =
        crate::segments::post::lookup_scope_by_post_id_including_tombstoned(&state.db, post_id)
            .await
            .map_err(|e| {
                tracing::error!("lookup_scope_by_post_id_including_tombstoned error: {e}");
                internal("storage error")
            })?
    else {
        // Never a live post and never a segment record: genuinely unknown.
        return Err(not_found("post not found"));
    };
    let author_hex = hex::encode(author);

    // The digests: read through the tombstone-inclusive body reader — a
    // reader only, nothing derived from these bytes leaves the process;
    // they feed `legal_takedown_deleted_posts`, never a serve path. `None`
    // here means physical reclaim already dropped the bytes (the mirror
    // row outlives the segment file) — there is nothing left to withhold or
    // back a takedown with, so this refuses exactly like a never-stored
    // post rather than fabricate a takedown over empty digests.
    let Some(body) = crate::segments::post::read_body_by_post_id_including_tombstoned(
        &state.post_segments,
        &state.db,
        post_id,
    )
    .await
    .map_err(|e| {
        tracing::error!("read_body_by_post_id_including_tombstoned error: {e}");
        internal("storage error")
    })?
    else {
        return Err(not_found("post not found"));
    };
    let blob_digests: Vec<[u8; 32]> = crate::db::posts::decode_stored_post(&body)
        .map(|post| post.blob_refs().into_iter().map(|h| h.digest()).collect())
        .unwrap_or_default();

    // One transaction: the obligation row (so the author still sees
    // "taken_down" in their queue and can appeal) + the permanent audit
    // row + `legal_takedown_deleted_posts` — deliberately NOT the legs the
    // delete already ran on its way past: no `content_meta` flag (no row
    // to write it to), no second ATProto retraction witness, no Nostr
    // kind-5, no web re-render (`moderation.md` § Legal takedown →
    // *Posts*).
    state
        .db
        .post_legal_takedown_of_deleted_post_txn(
            post_id,
            &content_id_hex,
            &req.legal_reference,
            &author,
            actor_id.as_slice(),
            &blob_digests,
            &format!(
                "admin={admin_hex} author={author_hex} reference={} \
                 (post already deleted by its author before the order arrived)",
                req.legal_reference
            ),
            now_us,
        )
        .await
        .map_err(|e| {
            tracing::error!("post legal-takedown-of-deleted-post txn error: {e}");
            internal("storage error")
        })?;

    encode_reply(&ModerationLegalTakedownReply {
        status: "taken_down".into(),
        content_id: req.content_id.clone(),
        extra: BTreeMap::new(),
    })
}

/// A legal takedown or its overturn just moved the flag on `post_id`: if the
/// post is published on its author's web site, re-render that site now,
/// synchronously, fail-closed
/// ([`WebContentService::rerender_after_moderation_change`](crate::web_content::service::WebContentService::rerender_after_moderation_change)).
///
/// Gated on the post actually being published — a takedown is rare, but a
/// render of a site the post was never on would change nothing. A lookup error
/// re-renders anyway: not knowing whether the post is on the site is resolved
/// toward shutting it. Non-fatal for the same reason the blob-withhold rebuild
/// is: the flag, obligation and audit rows landed atomically already; this is
/// the derived serve surface catching up, logged loudly if it cannot.
async fn rerender_web_site_after_flag_change(
    state: &Arc<AppState>,
    author: &[u8; 32],
    post_id: &[u8; 32],
) {
    let Some(wcs) = &state.web_content_service else {
        return;
    };
    let published = match state.db.list_web_published(author).await {
        Ok(rows) => rows
            .iter()
            .any(|(id, _, _)| id.as_slice() == post_id.as_slice()),
        Err(e) => {
            tracing::error!("web_published lookup after a legal-takedown flag change: {e:#}");
            true
        }
    };
    if !published {
        return;
    }
    if let Err(e) = wcs.rerender_after_moderation_change(author).await {
        tracing::error!("web re-render after a legal-takedown flag change: {e:#}");
    }
}

// ── fauna.moderation.report_share.set ──────────────────────────────────

/// Set the connection actor's report-sharing opt-in
/// (`report-sharing.md` § Client wire). **Caller-scoped**: the subject is the
/// connection `actor_id` — there is no `actor_id` on the wire, so even an admin
/// flips only their own preference. `share=false` also runs the opt-out sweep
/// (delete the reporter's rows + recompute each affected aggregate, which may
/// fall below k and withdraw its bus rows) — all inside `set_share_reports`.
fn report_share_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.report_share.set").await?;
            let req: ModerationReportShareSetRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .set_share_reports(&actor_id, req.share)
                .await
                .map_err(|e| {
                    tracing::error!("set_share_reports error: {e}");
                    internal("storage error")
                })?;
            if !req.share {
                // The opt-out sweep may have withdrawn exportable aggregates —
                // nudge the exchange originator so peers see the withdrawal.
                state.notify_exchange_transition();
            }
            encode_reply(&ModerationReportShareSetReply {
                share: req.share,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.report_share.status ───────────────────────────────

/// Read the connection actor's opt-in state **and** exactly what this nest
/// publishes to the world (`report-sharing.md` § Client wire). `published` is
/// the **federation export view**, byte-identical to what a peer nest would
/// receive over `fauna.federation.reports.export` — one function, so the
/// transparency guarantee is structural: every entry has already passed the
/// k-anonymity gate (`export_report_aggregates`), no reply carries a below-k
/// count or a reporter identity, and there is no other read of
/// `content_reports` anywhere. The `share` bit is caller-scoped; the
/// `published` list is nest-wide by design (it shows what the nest exports).
fn report_share_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.report_share.status").await?;
            let _req: ModerationReportShareStatusRequest = decode(&payload).map_err(malformed)?;
            let share = state
                .db
                .share_reports_enabled(&actor_id)
                .await
                .map_err(|e| {
                    tracing::error!("share_reports_enabled error: {e}");
                    internal("storage error")
                })?;
            let published = state
                .db
                .export_report_aggregates()
                .await
                .map_err(|e| {
                    tracing::error!("export_report_aggregates error: {e}");
                    internal("storage error")
                })?
                .into_iter()
                .map(|(hash, factor, count)| ReportShareEntry {
                    content_hash: hex::encode(hash),
                    factor,
                    count,
                    extra: BTreeMap::new(),
                })
                .collect();
            encode_reply(&ModerationReportShareStatusReply {
                share,
                published,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.signal_share.set ──────────────────────────────────

/// Set the connection actor's engagement-signal-sharing opt-in
/// (`engagement-cues.md` § Layer B nest legs). The Layer-B sibling of
/// `report_share.set` — **caller-scoped** (the subject is always the connection
/// actor) and INDEPENDENT of `report_share`: `share=false` runs the opt-out
/// sweep scoped to the `signal:` factor family only (`set_share_signals`), so a
/// contributor's `report:spam` rows survive.
fn signal_share_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.signal_share.set").await?;
            let req: ModerationSignalShareSetRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .set_share_signals(&actor_id, req.share)
                .await
                .map_err(|e| {
                    tracing::error!("set_share_signals error: {e}");
                    internal("storage error")
                })?;
            if !req.share {
                // The opt-out sweep may have withdrawn exportable aggregates —
                // nudge the exchange originator so peers see the withdrawal.
                state.notify_exchange_transition();
            }
            encode_reply(&ModerationSignalShareSetReply {
                share: req.share,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.signal_share.status ───────────────────────────────

/// Read the connection actor's signal opt-in state **and** exactly what this
/// nest publishes (`engagement-cues.md` § Layer B nest legs). `published` is
/// the **same** federation export view `report_share.status` returns —
/// `export_report_aggregates()`, byte-identical to what a peer receives over
/// `fauna.federation.reports.export` — so it carries every ≥k aggregate the
/// nest publishes, `report:*` and `signal:*` alike. One export function, one
/// transparency guarantee: every entry has passed the k-gate, no reply carries
/// a below-k count or a reporter identity. The `share` bit is caller-scoped;
/// the `published` list is nest-wide by design.
fn signal_share_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.signal_share.status").await?;
            let _req: ModerationSignalShareStatusRequest = decode(&payload).map_err(malformed)?;
            let share = state
                .db
                .share_signals_enabled(&actor_id)
                .await
                .map_err(|e| {
                    tracing::error!("share_signals_enabled error: {e}");
                    internal("storage error")
                })?;
            let published = state
                .db
                .export_report_aggregates()
                .await
                .map_err(|e| {
                    tracing::error!("export_report_aggregates error: {e}");
                    internal("storage error")
                })?
                .into_iter()
                .map(|(hash, factor, count)| ReportShareEntry {
                    content_hash: hex::encode(hash),
                    factor,
                    count,
                    extra: BTreeMap::new(),
                })
                .collect();
            encode_reply(&ModerationSignalShareStatusReply {
                share,
                published,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.signal_contribute ─────────────────────────────────

/// Contribute one derived engagement-cue verdict for a public post
/// (`engagement-cues.md` § Layer B — the write path). Caller-scoped: the
/// contributor is the connection actor. `capture_signal` honors a positive
/// verdict only when the actor opted in (`signal_share.set{share:true}`); a
/// `withdraw` always applies (a retraction). **Public posts only:** a NEW
/// verdict (`watch-complete`/`skip`) about a gated or unseen post is rejected —
/// a `signal:*` aggregate on restricted content would leak readership. A
/// `withdraw` is exempt from the public gate (it only removes the caller's own
/// row, reveals nothing, and must succeed even if the post later went gated —
/// user-controls-their-data).
fn signal_contribute_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.signal_contribute").await?;
            let req: ModerationSignalContributeRequest = decode(&payload).map_err(malformed)?;
            let post_id: [u8; 32] = fauna_core::hex32::decode(&req.content_id)
                .map_err(|_| invalid_params("content_id must be 32-byte hex"))?;
            let verdict =
                crate::db::signals::SignalVerdict::from_wire(&req.signal).ok_or_else(|| {
                    invalid_params("signal must be \"watch-complete\", \"skip\", or \"withdraw\"")
                })?;

            // A NEW judgment is limited to public posts; a withdrawal is exempt.
            if verdict != crate::db::signals::SignalVerdict::Withdraw
                && !state.db.content_is_public(&post_id).await.map_err(|e| {
                    tracing::error!("content_is_public error: {e}");
                    internal("storage error")
                })?
            {
                return Err(invalid_params(
                    "signal contribution is limited to public posts",
                ));
            }

            state
                .db
                .capture_signal(&actor_id, &post_id, verdict)
                .await
                .map_err(|e| {
                    tracing::error!("capture_signal error: {e}");
                    internal("storage error")
                })?;
            // The local aggregate may have transitioned — nudge the federation
            // exchange originator's debounced push.
            state.notify_exchange_transition();

            encode_reply(&ModerationSignalContributeReply {
                status: "recorded".into(),
                signal: req.signal,
                extra: BTreeMap::new(),
            })
        })
    })
}

// ── fauna.moderation.abuse_report.* ────────────────────────────────────
//
// User-initiated reporting (`moderation.md` § User-initiated reporting). A
// report is evidence for an admin and nothing else: these handlers write
// `abuse_reports` and `notifications` rows only — never `sender_reports`,
// `content_reports` or `content_labels` (§ Anti-abuse posture bound 4) — and
// never tell the reported author anything (§ What the reporter is told).

/// The admins' doorbell `notif_type`.
const ABUSE_REPORT_RECEIVED_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::AbuseReportReceived;
/// The reporter's outcome `notif_type`.
const ABUSE_REPORT_RESOLVED_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::AbuseReportResolved;

/// The longest channel / record id a message subject may name — ids, not text.
const MAX_ABUSE_SUBJECT_ID_BYTES: usize = 256;

/// Validate a subject and canonicalize its ids (a 32-byte hex id is keyed
/// lowercase, so a case-varied repeat is the same subject to the dedupe).
pub(crate) fn canonical_abuse_subject(
    subject: AbuseReportSubject,
) -> Result<AbuseReportSubject, RpcError> {
    let hex32 = |field: &str, v: &str| {
        fauna_core::hex32::decode(v)
            .map(hex::encode)
            .map_err(|_| invalid_params(&format!("{field} must be a 32-byte hex id")))
    };
    let bounded = |field: &str, v: String| {
        if v.is_empty() || v.len() > MAX_ABUSE_SUBJECT_ID_BYTES {
            Err(invalid_params(&format!("{field} required")))
        } else {
            Ok(v)
        }
    };
    Ok(match subject {
        AbuseReportSubject::Post { cid } => AbuseReportSubject::Post {
            cid: hex32("cid", &cid)?,
        },
        AbuseReportSubject::Actor { actor_id } => AbuseReportSubject::Actor {
            actor_id: hex32("actor_id", &actor_id)?,
        },
        AbuseReportSubject::Message {
            channel,
            record_cid,
        } => AbuseReportSubject::Message {
            channel: bounded("channel", channel)?,
            record_cid: bounded("record_cid", record_cid)?,
        },
        // A kind this nest cannot key, dedupe or act on: refused for this one
        // request, never recorded under a kind it does know.
        AbuseReportSubject::Unknown(_) => {
            return Err(invalid_params("subject kind is not one this nest knows"));
        }
    })
}

/// Rebuild a stored row's subject for the own-reports and queue replies. A
/// `subject_kind` this nest does not recognise — a newer nest wrote the row —
/// reads as [`AbuseReportSubject::Unknown`] carrying that kind, never as an
/// actor: a misread subject would show an admin the wrong target to act on
/// (`transport.md` § Schema and forward-compat discipline, rule 3).
fn abuse_subject_of(row: &AbuseReportRow) -> AbuseReportSubject {
    match row.subject_kind.as_str() {
        "post" => AbuseReportSubject::Post {
            cid: row.subject_id.clone(),
        },
        "message" => AbuseReportSubject::Message {
            channel: row.subject_channel.clone().unwrap_or_default(),
            record_cid: row.subject_id.clone(),
        },
        "actor" => AbuseReportSubject::Actor {
            actor_id: row.subject_id.clone(),
        },
        other => AbuseReportSubject::Unknown(CarriedValue(Value::Map(BTreeMap::from([(
            "kind".to_string(),
            Value::String(other.to_string()),
        )])))),
    }
}

/// Where a local report went: this nest first, then the author's home nest
/// once it accepted the forward.
fn abuse_routed_to(state: &AppState, row: &AbuseReportRow) -> Vec<String> {
    let mut routed = vec![state.handle_domain()];
    routed.extend(
        row.forwarded_to
            .as_deref()
            .map(crate::abuse_report_federation::url_host),
    );
    routed
}

/// How a forwarded copy's origin is named to the admin — "a user of
/// <nest>": the verified origin's address this nest knows, else its id.
async fn abuse_origin_name(state: &AppState, origin_hex: &str) -> Result<String, RpcError> {
    let Ok(origin) = fauna_core::hex32::decode(origin_hex) else {
        return Ok(origin_hex.to_string());
    };
    Ok(state
        .db
        .resolve_foreign_nest_urls(&origin, 1)
        .await
        .map_err(internal)?
        .first()
        .map(|u| crate::abuse_report_federation::url_host(u))
        .unwrap_or_else(|| origin_hex.to_string()))
}

/// The subject's author, from this nest's own record where it has one, else
/// the reporter's word (`AbuseReportSubmitRequest::subject_actor`).
async fn abuse_subject_actor(
    state: &AppState,
    subject: &AbuseReportSubject,
    claimed: Option<&str>,
) -> Result<Option<String>, RpcError> {
    let claimed = claimed
        .and_then(|a| fauna_core::hex32::decode(a).ok())
        .map(hex::encode);
    Ok(match subject {
        AbuseReportSubject::Actor { actor_id } => Some(actor_id.clone()),
        AbuseReportSubject::Post { cid } => {
            let id = fauna_core::hex32::decode(cid).map_err(|_| invalid_params("cid"))?;
            state
                .db
                .abuse_report_post_author(&id)
                .await
                .map_err(internal)?
                .map(hex::encode)
                .or(claimed)
        }
        AbuseReportSubject::Message { .. } => claimed,
        // Refused before this point ([`canonical_abuse_subject`]); names no one.
        AbuseReportSubject::Unknown(_) => None,
    })
}

/// Write one notification row and push it live — the shape every nest
/// doorbell uses. `dedup` is the row's `content_id`, the key
/// `insert_notification` dedupes on.
async fn ring_abuse_notification(
    state: &AppState,
    actor: &[u8; 32],
    notif_type: &fauna_protocol::notifications::NotifType,
    dedup: &[u8],
    text: &NotificationText,
) -> Result<(), RpcError> {
    let now = fauna_core::data::Timestamp::now().as_i64();
    if let Some(notification_id) = state
        .db
        .insert_notification(
            actor,
            notif_type,
            "fauna",
            None,
            Some(dedup),
            None,
            text,
            now,
        )
        .await
        .map_err(internal)?
    {
        state.ws.notify_push(
            actor,
            fauna_protocol::PushEvent::Notification(
                fauna_protocol::push_events::NotificationPayload {
                    notification_id,
                    notif_type: notif_type.clone(),
                    source: "fauna".into(),
                    sender_id: None,
                    content_id: Some(hex::encode(dedup)),
                    summary: text.summary().to_string(),
                    body: text.body().cloned(),
                    timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                    extra: BTreeMap::new(),
                },
            ),
        );
    }
    Ok(())
}

/// Ring every admin's doorbell for a new open report, deduped per (subject,
/// day) so a pile-on is one doorbell and the queue's open count is the truth
/// (`moderation.md` § Where it lands). The nest holds no admin's timezone, so
/// the day is the UTC day. The reporter is not rung about their own report.
pub(crate) async fn ring_abuse_report_doorbell(
    state: &AppState,
    row: &AbuseReportRow,
) -> Result<(), RpcError> {
    let day = fauna_core::day_bucket::local_day_bucket(crate::db::now_epoch_secs(), 0);
    let dedup = format!("{}:{}:{day}", row.subject_kind, row.subject_id).into_bytes();
    let text = NotificationText::localized(LocalizedText::new(
        "notifications.row_abuse_report_received",
    ));
    for (admin, _) in state.db.list_admin_actors().await.map_err(internal)? {
        let Ok(admin) = <[u8; 32]>::try_from(admin) else {
            continue;
        };
        if row.reporter_actor.as_deref() == Some(admin.as_slice()) {
            continue;
        }
        ring_abuse_notification(
            state,
            &admin,
            &ABUSE_REPORT_RECEIVED_NOTIF_TYPE,
            &dedup,
            &text,
        )
        .await?;
    }
    Ok(())
}

/// Tell a local reporter their report's outcome — the outcome only, never the
/// admin's reasoning (`moderation.md` § What the reporter is told). Keyed on the
/// report id, so a resolve retry never rings twice.
pub(crate) async fn notify_abuse_reporter(
    state: &AppState,
    reporter: &[u8; 32],
    report_id: &str,
    outcome: AbuseReportOutcome,
) -> Result<(), RpcError> {
    let word_key = match outcome {
        AbuseReportOutcome::Dismissed => "moderation.report.outcome_dismissed",
        _ => "moderation.report.outcome_acted",
    };
    let word = fauna_i18n::strings::lookup(word_key)
        .unwrap_or_default()
        .to_string();
    let text = NotificationText::localized(
        LocalizedText::new("notifications.row_abuse_report_resolved").with_arg("outcome", word),
    );
    ring_abuse_notification(
        state,
        reporter,
        &ABUSE_REPORT_RESOLVED_NOTIF_TYPE,
        report_id.as_bytes(),
        &text,
    )
    .await
}

fn abuse_report_submit_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.abuse_report.submit").await?;
            let req: AbuseReportSubmitRequest = decode(&payload).map_err(malformed)?;
            let subject = canonical_abuse_subject(req.subject)?;
            let note = req
                .note
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty());
            if note
                .as_ref()
                .is_some_and(|n| n.len() > MAX_ABUSE_REPORT_NOTE_BYTES)
            {
                return Err(invalid_params("note too long"));
            }
            let excerpt = req.excerpt.filter(|e| !e.trim().is_empty());
            if excerpt
                .as_ref()
                .is_some_and(|e| e.len() > MAX_ABUSE_REPORT_EXCERPT_BYTES)
            {
                return Err(invalid_params("excerpt too long"));
            }
            let subject_actor =
                abuse_subject_actor(&state, &subject, req.subject_actor.as_deref()).await?;
            let channel = match &subject {
                AbuseReportSubject::Message { channel, .. } => Some(channel.clone()),
                _ => None,
            };
            let report = NewAbuseReport {
                reporter_actor: actor_id,
                subject_kind: subject.kind().to_string(),
                subject_id: subject.id().to_string(),
                subject_channel: channel,
                subject_actor,
                reason: req.reason.token().to_string(),
                note,
                excerpt,
                block_author: req.block_author,
            };
            let now = fauna_core::data::Timestamp::now().as_i64();
            let row = match state
                .db
                .insert_abuse_report(
                    report,
                    fauna_core::scoring::reports::ABUSE_REPORTS_PER_HOUR,
                    now,
                )
                .await
                .map_err(internal)?
            {
                AbuseReportInsert::RateLimited => return Err(rate_limited()),
                // A retry, or a second report on a subject already open for
                // this reporter: answered with the one on record, nothing rung.
                AbuseReportInsert::AlreadyOpen(row) => row,
                AbuseReportInsert::Inserted(row) => {
                    // The report is recorded; a doorbell that fails to ring is
                    // logged, not surfaced — the queue's open count is the truth.
                    if let Err(e) = ring_abuse_report_doorbell(&state, &row).await {
                        tracing::error!("abuse report doorbell: {}", e.code);
                    }
                    // A foreign author's home nest gets it too, reporter-
                    // anonymously (`moderation.md` § Routing). A forward that
                    // cannot be queued is logged: the report stands here.
                    let home = crate::abuse_report_federation::foreign_home_url(
                        &state,
                        &subject,
                        row.subject_actor.as_deref(),
                    )
                    .await;
                    match home {
                        Ok(Some(url)) => {
                            if let Err(e) = crate::abuse_report_federation::forward_new_report(
                                &state,
                                &row,
                                subject.clone(),
                                &url,
                            )
                            .await
                            {
                                tracing::error!("abuse report forward: {e:#}");
                            }
                            state
                                .db
                                .get_abuse_report(&row.id)
                                .await
                                .map_err(internal)?
                                .unwrap_or(row)
                        }
                        Ok(None) => row,
                        Err(e) => {
                            tracing::error!("abuse report home-nest lookup: {e:#}");
                            row
                        }
                    }
                }
            };
            encode_reply(&AbuseReportSubmitReply {
                routed_to: abuse_routed_to(&state, &row),
                report_id: row.id,
                extra: BTreeMap::new(),
            })
        })
    })
}

fn abuse_report_mine_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.abuse_report.mine").await?;
            let _req: AbuseReportMineRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_abuse_reports_by_reporter(&actor_id)
                .await
                .map_err(internal)?;
            let reports = rows
                .iter()
                .map(|row| AbuseReportMineEntry {
                    report_id: row.id.clone(),
                    subject: abuse_subject_of(row),
                    reason: AbuseReportReason::from_token(&row.reason),
                    created_at: row.created_at,
                    status: AbuseReportStatus::from_token(&row.status),
                    outcome: row.outcome.as_deref().map(AbuseReportOutcome::from_token),
                    routed_to: abuse_routed_to(&state, row),
                    extra: BTreeMap::new(),
                })
                .collect();
            encode_reply(&AbuseReportMineReply {
                reports,
                extra: BTreeMap::new(),
            })
        })
    })
}

fn abuse_report_withdraw_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.abuse_report.withdraw").await?;
            let req: AbuseReportWithdrawRequest = decode(&payload).map_err(malformed)?;
            match state
                .db
                .withdraw_abuse_report(&actor_id, &req.report_id)
                .await
                .map_err(internal)?
            {
                AbuseReportWithdraw::Withdrawn(row) => {
                    // The forwarded copy's note and excerpt go too.
                    if let Err(e) =
                        crate::abuse_report_federation::propagate_withdrawal(&state, &row).await
                    {
                        tracing::error!("abuse report withdrawal forward: {e:#}");
                    }
                }
                AbuseReportWithdraw::AlreadyWithdrawn => {}
                AbuseReportWithdraw::Resolved => {
                    return Err(crate::rpc_errors::conflict_ns(
                        "moderation",
                        "a resolved report cannot be withdrawn",
                    ));
                }
                AbuseReportWithdraw::NotFound => return Err(not_found("no such report")),
            }
            encode_reply(&AbuseReportWithdrawReply::default())
        })
    })
}

fn abuse_report_queue_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.abuse_report.queue").await?;
            let _req: AbuseReportQueueRequest = decode(&payload).map_err(malformed)?;
            let rows = state.db.list_open_abuse_reports().await.map_err(internal)?;
            let mut reports = Vec::with_capacity(rows.len());
            for row in rows {
                let reporter_handle = match row
                    .reporter_actor
                    .as_deref()
                    .and_then(|r| <[u8; 32]>::try_from(r).ok())
                {
                    Some(reporter) => Some(
                        state
                            .db
                            .get_user(&reporter)
                            .await
                            .map_err(internal)?
                            .and_then(|u| u.handle)
                            .filter(|h| !h.is_empty())
                            .unwrap_or_else(|| hex::encode(reporter)),
                    ),
                    None => None,
                };
                let origin_nest = match row.origin_nest_id.as_deref() {
                    Some(origin) => Some(abuse_origin_name(&state, origin).await?),
                    None => None,
                };
                reports.push(AbuseReportQueueEntry {
                    report_id: row.id.clone(),
                    subject: abuse_subject_of(&row),
                    subject_actor: row.subject_actor.clone(),
                    reason: AbuseReportReason::from_token(&row.reason),
                    note: row.note.clone(),
                    excerpt: row.excerpt.clone(),
                    reporter_handle,
                    origin_nest,
                    created_at: row.created_at,
                    extra: BTreeMap::new(),
                });
            }
            encode_reply(&AbuseReportQueueReply {
                reports,
                extra: BTreeMap::new(),
            })
        })
    })
}

fn abuse_report_resolve_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.moderation.abuse_report.resolve").await?;
            let req: AbuseReportResolveRequest = decode(&payload).map_err(malformed)?;
            if req.outcome == AbuseReportOutcome::Unknown {
                return Err(invalid_params("outcome must be acted or dismissed"));
            }
            let now = fauna_core::data::Timestamp::now().as_i64();
            match state
                .db
                .resolve_abuse_report(&req.report_id, req.outcome.token(), &actor_id, now)
                .await
                .map_err(internal)?
            {
                AbuseReportResolve::Resolved(row) => {
                    // A forwarded copy's outcome goes back to the origin nest,
                    // which alone knows who reported.
                    if row.origin_nest_id.is_some()
                        && let Err(e) = crate::abuse_report_federation::return_outcome(
                            &state,
                            &row,
                            req.outcome,
                        )
                        .await
                    {
                        tracing::error!("abuse report outcome forward: {e:#}");
                    }
                    if let Some(reporter) = row
                        .reporter_actor
                        .as_deref()
                        .and_then(|r| <[u8; 32]>::try_from(r).ok())
                        && let Err(e) =
                            notify_abuse_reporter(&state, &reporter, &row.id, req.outcome).await
                    {
                        tracing::error!("abuse report outcome notification: {}", e.code);
                    }
                }
                AbuseReportResolve::AlreadyResolved => {}
                AbuseReportResolve::Conflict => {
                    return Err(crate::rpc_errors::conflict_ns(
                        "moderation",
                        "the report is no longer open",
                    ));
                }
                AbuseReportResolve::NotFound => return Err(not_found("no such report")),
            }
            encode_reply(&AbuseReportResolveReply::default())
        })
    })
}

// ── registration entry point ───────────────────────────────────────────

pub fn register_moderation_handlers(b: &mut RpcRouterBuilder) {
    let quick = || Duration::from_secs(5);
    let heavy = || Duration::from_secs(10);
    b.add(
        "fauna.moderation.stats",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: stats_handler(),
        },
    );
    b.add(
        "fauna.moderation.actions",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: actions_handler(),
        },
    );
    b.add(
        "fauna.moderation.appeal",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: appeal_handler(),
        },
    );
    b.add(
        "fauna.moderation.train",
        RpcKindMeta {
            // Replay-safe: a read gate plus the report capture, whose
            // insert/delete on the `(content_hash, factor, reporter)` key is
            // idempotent. Mirror any change in
            // `KindRegistry::register_moderation_kinds`.
            forbid_replay: false,
            default_deadline: heavy(),
            handler: train_handler(),
        },
    );
    b.add(
        "fauna.moderation.legal_takedown",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: legal_takedown_handler(),
        },
    );
    b.add(
        "fauna.moderation.report_share.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: report_share_set_handler(),
        },
    );
    b.add(
        "fauna.moderation.report_share.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: report_share_status_handler(),
        },
    );
    b.add(
        "fauna.moderation.signal_share.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: signal_share_set_handler(),
        },
    );
    b.add(
        "fauna.moderation.signal_share.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: signal_share_status_handler(),
        },
    );
    b.add(
        "fauna.moderation.signal_contribute",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: signal_contribute_handler(),
        },
    );
    // User-initiated reporting. All replay-safe (see
    // `KindRegistry::register_moderation_kinds`, which this mirrors).
    for (kind, deadline, handler) in [
        (
            "fauna.moderation.abuse_report.submit",
            heavy(),
            abuse_report_submit_handler(),
        ),
        (
            "fauna.moderation.abuse_report.mine",
            quick(),
            abuse_report_mine_handler(),
        ),
        (
            "fauna.moderation.abuse_report.withdraw",
            heavy(),
            abuse_report_withdraw_handler(),
        ),
        (
            "fauna.moderation.abuse_report.queue",
            quick(),
            abuse_report_queue_handler(),
        ),
        (
            "fauna.moderation.abuse_report.resolve",
            heavy(),
            abuse_report_resolve_handler(),
        ),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: deadline,
                handler,
            },
        );
    }
}

#[cfg(test)]
mod abuse_subject_tests {
    use super::*;

    fn stored_row(subject_kind: &str) -> AbuseReportRow {
        AbuseReportRow {
            id: "r1".into(),
            created_at: 0,
            reporter_actor: None,
            origin_nest_id: None,
            origin_report_ref: None,
            subject_kind: subject_kind.into(),
            subject_id: "ab".repeat(32),
            subject_channel: None,
            subject_actor: None,
            reason: "spam".into(),
            note: None,
            excerpt: None,
            block_author: false,
            forwarded_to: None,
            status: "open".into(),
            outcome: None,
            resolved_at: None,
            resolved_by: None,
            forwarded_nest_id: None,
        }
    }

    fn unknown_kind(kind: &str) -> AbuseReportSubject {
        AbuseReportSubject::Unknown(CarriedValue(Value::Map(BTreeMap::from([(
            "kind".to_string(),
            Value::String(kind.into()),
        )]))))
    }

    /// The own-reports and moderator-queue replies rebuild each row's subject
    /// with [`abuse_subject_of`]. A `subject_kind` a newer nest wrote reads as
    /// the carrying unknown arm, never as an actor — an admin shown the wrong
    /// target could act on it (`transport.md` § Schema and forward-compat
    /// discipline, rule 3).
    #[test]
    fn a_stored_subject_kind_this_nest_does_not_know_is_never_read_as_an_actor() {
        assert_eq!(
            abuse_subject_of(&stored_row("group")),
            unknown_kind("group")
        );
        assert_eq!(
            abuse_subject_of(&stored_row("actor")),
            AbuseReportSubject::Actor {
                actor_id: "ab".repeat(32)
            }
        );
    }

    /// A new report about a subject kind this nest cannot key is refused for
    /// that one request, never recorded under a kind it knows.
    #[test]
    fn a_report_about_an_unknown_subject_kind_is_refused() {
        assert!(canonical_abuse_subject(unknown_kind("group")).is_err());
    }
}
