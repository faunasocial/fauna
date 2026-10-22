//! The transparency triple's appeal leg across a nest boundary —
//! `fauna.federation.moderation.{obligation.deliver,appeal}`
//! (`docs/goal/behavior/moderation.md` § Legal takedown → *The author homed on
//! another nest*).
//!
//! An obligation row is keyed by its author on the nest whose admin decided —
//! the channel's home for a conversation record, and for the local projection
//! of a foreign post the nest serving that copy — while the author reads their
//! queue and appeals on their own home. So a takedown or block whose author
//! this nest does not host **pushes** the row to the author's home, on the
//! moderation plane's one durable queue (the report triad's
//! `abuse_report_outbox`, drained by
//! [`crate::abuse_report_federation`]), and the home **relays** the author's
//! appeal back inline to the issuer, which decides and keeps it.
//!
//! Serving side: a delivery lands only for an author this nest hosts (and, for
//! a post, one that author wrote), is free on a re-delivery of a stored
//! `(origin, ref)` and spends two per-target budgets otherwise; a relayed
//! appeal is accepted only from the nest the row was delivered to, for the
//! row's own author. An overturn delivers nothing — the row stays history on
//! both nests.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_core::obligation::ObligationAction as Action;
use fauna_protocol::moderation::{
    AbuseReportSubject, ModerationAppealReply, ObligationAppealRelayRequest, ObligationDeliverAck,
    ObligationDeliverRequest,
};
use fauna_protocol::{RpcError, decode_strict, encode_canonical};

use crate::db::moderation::{DeliveredObligation, OBLIGATION_CALL_DELIVER, ObligationDeliveryRow};
use crate::federation_router::{FederationRouterBuilder, RpcHandler, RpcKindMeta};
use crate::routes::AppState;
use crate::rpc_errors::{internal, malformed};

/// `fauna.federation.moderation.obligation.deliver` — the author's queue row,
/// pushed to their home.
pub const DELIVER_KIND: &str = "fauna.federation.moderation.obligation.deliver";
/// `fauna.federation.moderation.appeal` — the author's appeal, relayed by
/// their home to the issuer.
pub const APPEAL_KIND: &str = "fauna.federation.moderation.appeal";

/// How long a takedown or block waits on its delivery's first try before
/// replying — the queue keeps trying either way.
const INLINE_DELIVER_WAIT: Duration = Duration::from_secs(4);

/// The longest issuer `ref` and enforcement descriptor a delivery may carry.
const MAX_REF_BYTES: usize = 64;
const MAX_CATEGORY_BYTES: usize = 64;

fn invalid(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("federation", reason)
}

fn not_hosted() -> RpcError {
    crate::rpc_errors::not_found_ns("federation", "author not hosted here")
}

/// The handle domain a delivered row names its issuer by: the host its
/// verified id answers at, a dial-proven address first.
pub(crate) async fn issuer_host(state: &AppState, issuer_nest_id_hex: &str) -> Option<String> {
    let id = fauna_core::hex32::decode(issuer_nest_id_hex).ok()?;
    match state.db.resolve_foreign_nest_urls(&id, 1).await {
        Ok(urls) => urls
            .first()
            .map(|u| crate::abuse_report_federation::url_host(u)),
        Err(e) => {
            tracing::warn!("obligation issuer address: {e:#}");
            None
        }
    }
}

// ── issuer side ─────────────────────────────────────────────────────────────

/// Row `row_id` was just written by an admin's takedown or block: when its
/// author is not hosted here, queue the row's delivery to their home and try
/// it once, bounded by [`INLINE_DELIVER_WAIT`]. Non-fatal — the decision and
/// its transparency rows already landed; a failure here is logged, the queue
/// retries a delivery it holds, and one a stop lost before it was queued is
/// queued again by [`reconcile_undelivered_obligations`] at the next boot.
pub(crate) async fn deliver_obligation(state: &Arc<AppState>, row_id: i64) {
    if let Err(e) = try_deliver_obligation(state, row_id).await {
        tracing::error!("obligation row {row_id} delivery: {e:#}");
    }
}

async fn try_deliver_obligation(state: &Arc<AppState>, row_id: i64) -> anyhow::Result<()> {
    let Some(id) = queue_obligation_delivery(state, row_id).await? else {
        return Ok(());
    };
    let report_id = row_id.to_string();
    let Some(entry) = state
        .db
        .abuse_report_call(&report_id, OBLIGATION_CALL_DELIVER)
        .await?
        .filter(|e| e.id == id)
    else {
        return Ok(());
    };
    if tokio::time::timeout(
        INLINE_DELIVER_WAIT,
        crate::abuse_report_federation::attempt(state, &entry),
    )
    .await
    .is_err()
    {
        tracing::debug!("obligation delivery still pending; the forwarder retries it");
    }
    Ok(())
}

/// The boot reconcile of the issuer's half (`nest/common.md` § Client-state
/// recoverability): a delivery is queued after the decision's transaction
/// commits, so a nest that stopped in between holds a row nothing delivers.
/// Every own row within the queue's call ceiling that no home acknowledged
/// and no queued call carries is queued again — a re-send the home's
/// `(origin, ref)` dedupe makes free. A row the home refused for good, or one
/// whose author's home is unknown, is retried once per boot until it ages out.
/// Returns how many deliveries were queued.
pub async fn reconcile_undelivered_obligations(state: &Arc<AppState>) -> usize {
    let since_us = (crate::db::now_epoch_secs()
        - crate::abuse_report_federation::MAX_CALL_AGE_SECS)
        .saturating_mul(1_000_000);
    let ids = match state.db.undelivered_obligation_row_ids(since_us).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("obligation delivery reconcile read: {e:#}");
            return 0;
        }
    };
    let mut queued = 0;
    for row_id in ids {
        match queue_obligation_delivery(state, row_id).await {
            Ok(Some(_)) => queued += 1,
            Ok(None) => {}
            Err(e) => tracing::error!("obligation row {row_id} delivery reconcile: {e:#}"),
        }
    }
    queued
}

/// Queue row `row_id`'s delivery to its foreign author's home; the queued
/// call's id, or `None` when the row is not this nest's own, its author is
/// hosted here, or no home is known.
async fn queue_obligation_delivery(
    state: &Arc<AppState>,
    row_id: i64,
) -> anyhow::Result<Option<i64>> {
    let Some(row) = state.db.obligation_row_for_delivery(row_id).await? else {
        return Ok(None);
    };
    let Ok(author) = fauna_core::hex32::decode(&row.author_hex) else {
        return Ok(None);
    };
    if state.db.is_actor_registered(&author).await? {
        return Ok(None);
    }
    let Some((peer_url, pin)) = author_home(state, &row, &author).await? else {
        tracing::warn!(
            "obligation row {row_id}: no home nest known for its foreign author; \
             the row stays on this nest alone"
        );
        return Ok(None);
    };
    let req = ObligationDeliverRequest {
        issuer_ref: row.id.to_string(),
        content_type: row.content_type.clone(),
        content_id: row.content_id.clone(),
        author: row.author_hex.clone(),
        action: row.action_taken,
        category: row.category.clone(),
        timestamp: row.timestamp,
        issuer_nest_id: state.handle_domain(),
        issuer_nest_url: state.handle_domain_if_set().map(|d| format!("https://{d}")),
        extra: BTreeMap::new(),
    };
    let id = state
        .db
        .enqueue_abuse_report_call(
            &row.id.to_string(),
            OBLIGATION_CALL_DELIVER,
            &peer_url,
            pin.as_deref(),
            &encode_canonical(&req)?,
            crate::db::now_epoch_secs(),
        )
        .await?;
    Ok(Some(id))
}

/// The author's home: its URL, and its verified id when this nest already
/// knows it (the dial is then pinned to it). A post resolves as a forwarded
/// report's does (`moderation.md` § Routing → *Home-nest resolution*); a
/// conversation record through the author's seat on the record's channel —
/// the foreign-member binding every channel relay uses.
async fn author_home(
    state: &AppState,
    row: &ObligationDeliveryRow,
    author: &[u8; 32],
) -> anyhow::Result<Option<(String, Option<String>)>> {
    match row.content_type.as_str() {
        "post" => Ok(crate::abuse_report_federation::foreign_home_url(
            state,
            &AbuseReportSubject::Post {
                cid: row.content_id.clone(),
            },
            Some(&row.author_hex),
        )
        .await?
        .map(|url| (url, None))),
        "conversation" => {
            let record_id = fauna_core::hex32::decode(&row.content_id)?;
            let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
            let Some((channel, _)) = state.db.conv_record_scope_and_takedown(&record_cid).await?
            else {
                return Ok(None);
            };
            let Some(home) = state.db.foreign_member_home_nest(&channel, author).await? else {
                return Ok(None);
            };
            let url = match state.db.resolve_foreign_nest_urls(&home, 1).await?.pop() {
                Some(url) => Some(url),
                None => state.db.foreign_member_nest_url(&channel, author).await?,
            };
            Ok(url.map(|url| (url, Some(hex::encode(home)))))
        }
        _ => Ok(None),
    }
}

// ── home side ───────────────────────────────────────────────────────────────

/// The author's appeal of a row `issuer_nest_id` delivered: relay it inline to
/// the issuer — the `fauna.posts.get_remote` posture, the issuer's `status`
/// returned verbatim and its typed refusals through the peer-relay map — and,
/// once the issuer recorded it, write `moderation:appeal-relayed` on this
/// nest's own chain. Nothing is written on a failed relay: the author holds
/// their text and retries.
pub(crate) async fn relay_appeal(
    state: &Arc<AppState>,
    author: &[u8; 32],
    content_id: &str,
    issuer_nest_id: &[u8; 32],
    issuer_ref: &str,
    reason: &str,
) -> Result<String, RpcError> {
    let peer_url = state
        .db
        .resolve_foreign_nest_urls(issuer_nest_id, 1)
        .await
        .map_err(internal)?
        .pop()
        .ok_or_else(|| {
            crate::rpc_errors::unavailable_ns("moderation", "no address for the deciding nest")
        })?;
    let req = ObligationAppealRelayRequest {
        issuer_ref: issuer_ref.to_string(),
        content_id: content_id.to_string(),
        author: hex::encode(author),
        reason: reason.to_string(),
        extra: BTreeMap::new(),
    };
    let reply = match crate::federation_pool::originate_moderation_appeal(
        &state.federation_pool,
        state,
        &peer_url,
        issuer_nest_id,
        &req,
    )
    .await
    {
        Ok(Ok(reply)) => reply,
        Ok(Err(peer_err)) => {
            return Err(crate::rpc_errors::map_peer_relay_error(
                peer_err,
                "the relayed moderation appeal",
            ));
        }
        Err(pool_err) => {
            tracing::warn!("moderation appeal relay: {pool_err}");
            return Err(crate::rpc_errors::unavailable_ns(
                "moderation",
                "the deciding nest could not be reached",
            ));
        }
    };
    if reply.status == "appeal_recorded"
        && let Err(e) = state
            .db
            .audit(
                Some(author.as_slice()),
                "moderation:appeal-relayed",
                Some(content_id),
                Some(&format!(
                    "issuer={} reason={reason}",
                    hex::encode(issuer_nest_id)
                )),
            )
            .await
    {
        // The issuer holds the appeal; this nest's own record of the relay is
        // what failed, and that never un-records it.
        tracing::error!("moderation:appeal-relayed audit: {e:#}");
    }
    Ok(reply.status)
}

// ── serving handlers ────────────────────────────────────────────────────────

fn deliver_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: ObligationDeliverRequest = decode_strict(&payload).map_err(malformed)?;
            if req.issuer_ref.is_empty() || req.issuer_ref.len() > MAX_REF_BYTES {
                return Err(invalid("ref"));
            }
            if req.content_type != "post" && req.content_type != "conversation" {
                return Err(invalid("content_type"));
            }
            let content_id =
                fauna_core::hex32::decode(&req.content_id).map_err(|_| invalid("content_id"))?;
            let author = fauna_core::hex32::decode(&req.author).map_err(|_| invalid("author"))?;
            if req.action != Action::TakenDown as u8 && req.action != Action::Blocked as u8 {
                return Err(invalid("action"));
            }
            if req.category.is_empty() || req.category.len() > MAX_CATEGORY_BYTES {
                return Err(invalid("category"));
            }
            if !state
                .db
                .is_actor_registered(&author)
                .await
                .map_err(internal)?
            {
                return Err(not_hosted());
            }
            // The home holds every own post, a deleted one as a tombstone: a
            // post its author never wrote here is refused, so a forged post row
            // cannot land. A conversation record it cannot check — it keeps no
            // record of its members' foreign channels — so there the hosted
            // author and the budgets are the gate.
            if req.content_type == "post" {
                let owner = crate::segments::post::lookup_scope_by_post_id_including_tombstoned(
                    &state.db,
                    &content_id,
                )
                .await
                .map_err(internal)?
                .map(|(owner, _)| owner);
                if owner != Some(author) {
                    return Err(not_hosted());
                }
            }
            // Both halves of the dedupe key are the caller's to choose, so a
            // new row spends two per-target budgets — the author and the whole
            // nest, summed over every origin — before anything is written. A
            // re-delivery of a stored row stays free, and a refusal is
            // `rate_limited`, which the sender's queue retries.
            let origin = hex::encode(origin_nest_id);
            if !state
                .db
                .delivered_obligation_exists(&origin, &req.issuer_ref)
                .await
                .map_err(internal)?
            {
                let budget = &state.federation_target_budget;
                if !budget.check_with_max(
                    &author,
                    &[0u8; 32],
                    "obligation_deliver",
                    crate::bridge_rate_limit::OBLIGATION_DELIVER_AUTHOR_TARGET_MAX,
                ) || !budget.check_with_max(
                    &[0u8; 32],
                    &[0u8; 32],
                    "obligation_deliver",
                    crate::bridge_rate_limit::OBLIGATION_DELIVER_NEST_TARGET_MAX,
                ) {
                    return Err(crate::rpc_errors::rate_limited());
                }
            }
            // The issuer's declared address, recorded against its verified id,
            // is how the author's appeal finds its way back.
            crate::federation_handlers::resolve_origin_home_url(
                &state,
                &origin_nest_id,
                req.issuer_nest_url.as_deref(),
            )
            .await?;
            state
                .db
                .insert_delivered_obligation(&DeliveredObligation {
                    issuer_nest_id: origin,
                    issuer_ref: req.issuer_ref,
                    content_type: req.content_type,
                    content_id: hex::encode(content_id),
                    author,
                    category: req.category,
                    action_taken: req.action,
                    timestamp: req.timestamp,
                })
                .await
                .map_err(internal)?;
            Ok(Bytes::from(
                encode_canonical(&ObligationDeliverAck::default())
                    .map_err(internal)?
                    .to_vec(),
            ))
        })
    })
}

fn appeal_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: ObligationAppealRelayRequest = decode_strict(&payload).map_err(malformed)?;
            let row_id: i64 = req.issuer_ref.parse().map_err(|_| invalid("ref"))?;
            // Structural: the row must have been delivered to THIS origin — the
            // author's home, verified on the channel — and name this author and
            // content. Anything else is not that home relaying its author.
            let Some((content_id, author_hex)) = state
                .db
                .obligation_row_delivered_to(row_id, &hex::encode(origin_nest_id))
                .await
                .map_err(internal)?
            else {
                return Err(crate::rpc_errors::forbidden_ns(
                    "federation",
                    "no row was delivered to this nest under that ref",
                ));
            };
            let req_content = fauna_core::hex32::decode(&req.content_id)
                .map(hex::encode)
                .unwrap_or_else(|_| req.content_id.clone());
            let author = fauna_core::hex32::decode(&author_hex)
                .map_err(|_| internal("delivered row's author is not 32-byte hex"))?;
            if req_content != content_id
                || fauna_core::hex32::decode(&req.author).ok() != Some(author)
            {
                return Err(crate::rpc_errors::forbidden_ns(
                    "federation",
                    "the appeal names another content or author than the row",
                ));
            }
            let status = crate::moderation_handlers::record_appeal_for(
                &state,
                &author,
                &content_id,
                &req.reason,
            )
            .await?;
            Ok(Bytes::from(
                encode_canonical(&ModerationAppealReply {
                    status,
                    content_id: req.content_id,
                    extra: BTreeMap::new(),
                })
                .map_err(internal)?
                .to_vec(),
            ))
        })
    })
}

/// Register both kinds. Each is idempotent here — a delivery keyed on the
/// origin's `ref`, an appeal collapsing onto the pending one — so each is
/// retry-safe (`forbid_replay: false`).
pub fn register_moderation_federation_handlers(b: &mut FederationRouterBuilder) {
    for (kind, handler) in [
        (DELIVER_KIND, deliver_handler()),
        (APPEAL_KIND, appeal_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(10),
                handler,
            },
        );
    }
}
