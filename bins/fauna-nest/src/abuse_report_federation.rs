//! The abuse-report federation triad — `fauna.federation.abuse_report.{deliver,
//! withdraw,outcome}` (`docs/goal/behavior/moderation.md` § Routing, § What the
//! reporter is told).
//!
//! A report always lands on the reporter's own nest; when the reported author
//! is foreign it is also forwarded, **reporter-anonymously**, to the author's
//! home nest, whose admin holds the account-side levers. The reporter's
//! withdrawal follows the forward (the note and excerpt are their data), and
//! the home admin's outcome comes back to the one nest that knows who reported.
//!
//! Both directions ride one durable queue, `abuse_report_outbox`: a leg is
//! queued in the same act that makes it due and drained by
//! [`spawn_abuse_report_forwarder`] with backoff, so a peer that is down when a
//! report is filed still gets it. Every leg is idempotent on the receiver (a
//! delivery is keyed on the origin's `report_ref`), so a re-send is always
//! safe. A submit tries its delivery once inline, bounded, so the
//! acknowledgement can name the home nest when it was reached.
//!
//! Serving side: the receiver accepts a delivery only for a subject it hosts
//! (never a relay for third nests), rides the channel's per-origin throttle
//! like every federation kind, and stores no reporter identity because none
//! was sent. Nothing here writes `sender_reports`, `content_reports` or
//! `content_labels` (§ Anti-abuse posture bound 4).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_protocol::moderation::{
    AbuseReportDeliverRequest, AbuseReportFederationAck, AbuseReportFederationOutcomeRequest,
    AbuseReportFederationWithdrawRequest, AbuseReportOutcome, AbuseReportReason,
    AbuseReportSubject, MAX_ABUSE_REPORT_EXCERPT_BYTES, MAX_ABUSE_REPORT_NOTE_BYTES,
};
use fauna_protocol::{RpcError, Value, decode_strict, encode_canonical};

use crate::db::moderation::{
    ABUSE_CALL_DELIVER, ABUSE_CALL_OUTCOME, ABUSE_CALL_WITHDRAW, AbuseReportOutboxEntry,
    AbuseReportRow, ForwardedAbuseReport,
};
use crate::federation_router::{FederationRouterBuilder, RpcHandler, RpcKindMeta};
use crate::routes::AppState;
use crate::rpc_errors::{internal, malformed};

/// `fauna.federation.abuse_report.deliver` — forward a report to the author's
/// home nest.
pub const DELIVER_KIND: &str = "fauna.federation.abuse_report.deliver";
/// `fauna.federation.abuse_report.withdraw` — the reporter withdrew it.
pub const WITHDRAW_KIND: &str = "fauna.federation.abuse_report.withdraw";
/// `fauna.federation.abuse_report.outcome` — the home admin resolved it.
pub const OUTCOME_KIND: &str = "fauna.federation.abuse_report.outcome";

/// How long a submit waits on its own delivery before answering with the
/// reporter's nest alone — under the submit kind's 10 s deadline. The queue
/// keeps trying either way.
const INLINE_DELIVER_WAIT: Duration = Duration::from_secs(4);

/// How often the forwarder drains the queue.
const DRAIN_INTERVAL: Duration = Duration::from_secs(30);

/// Calls drained per pass.
const DRAIN_BATCH: i64 = 50;

/// The first retry waits this long; each later one doubles, up to
/// [`MAX_RETRY_DELAY_SECS`].
const FIRST_RETRY_DELAY_SECS: i64 = 60;
const MAX_RETRY_DELAY_SECS: i64 = 6 * 3_600;

/// A call no peer has answered for this long is dropped: a report stays on
/// the reporter's own nest's queue whatever happens to its forward.
const MAX_CALL_AGE_SECS: i64 = 7 * 24 * 3_600;

fn kind_for(call: &str) -> Option<&'static str> {
    match call {
        ABUSE_CALL_DELIVER => Some(DELIVER_KIND),
        ABUSE_CALL_WITHDRAW => Some(WITHDRAW_KIND),
        ABUSE_CALL_OUTCOME => Some(OUTCOME_KIND),
        _ => None,
    }
}

fn invalid(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("federation", reason)
}

fn not_hosted() -> RpcError {
    crate::rpc_errors::not_found_ns("federation", "subject not hosted here")
}

fn encode(value: &impl serde::Serialize) -> anyhow::Result<Vec<u8>> {
    Ok(encode_canonical(value)?.to_vec())
}

fn ack() -> Result<Bytes, RpcError> {
    Ok(Bytes::from(
        encode(&AbuseReportFederationAck::default()).map_err(internal)?,
    ))
}

/// The host (with any port) of a peer URL — how a destination is named to the
/// reporter and the admin.
pub(crate) fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_string()
}

// ── origin side ─────────────────────────────────────────────────────────────

/// The home nest URL of a foreign subject's author (`moderation.md` § Routing
/// → *Home-nest resolution*), or `None` when the author is local or this nest
/// knows no home for them — the report then stays on this nest alone.
///
/// A post names the nest it arrived from; an account or a message sender is
/// resolved through a post of theirs this nest holds, then through the channel
/// rosters that seat them. Each answer is only a candidate: the dial proves
/// which nest answers, and that nest refuses a subject it does not host.
pub(crate) async fn foreign_home_url(
    state: &AppState,
    subject: &AbuseReportSubject,
    subject_actor: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let actor = subject_actor.and_then(|a| fauna_core::hex32::decode(a).ok());
    if let Some(actor) = &actor
        && state.db.is_actor_registered(actor).await?
    {
        return Ok(None);
    }
    if let AbuseReportSubject::Post { cid } = subject
        && let Ok(post_id) = fauna_core::hex32::decode(cid)
        && let Some(url) = state.db.get_post_origin_nest_url(&post_id).await?
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    let Some(actor) = actor else {
        return Ok(None);
    };
    if let Some(url) = state.db.abuse_report_author_origin_url(&actor).await? {
        return Ok(Some(url));
    }
    if let Some(nest) = state.db.oldest_foreign_member_nest_id(&actor).await? {
        return Ok(state
            .db
            .resolve_foreign_nest_urls(&nest, 1)
            .await?
            .into_iter()
            .next());
    }
    Ok(None)
}

/// The `withdraw` leg's request for `report_ref` — the one home of its
/// encoding, which the account-deletion leg in `db/moderation.rs`
/// (`withdraw_abuse_reports_for_deleted_reporter`) queues under the purge
/// walk's own lock.
pub(crate) fn withdraw_payload(report_ref: &str) -> anyhow::Result<Vec<u8>> {
    encode(&AbuseReportFederationWithdrawRequest {
        report_ref: report_ref.to_string(),
        extra: BTreeMap::new(),
    })
}

/// Queue a new local report's delivery to the author's home nest and try it
/// once, bounded by [`INLINE_DELIVER_WAIT`]. When the home nest accepts, the
/// report's `forwarded_to` is set before this returns; otherwise the
/// forwarder keeps trying.
pub(crate) async fn forward_new_report(
    state: &Arc<AppState>,
    row: &AbuseReportRow,
    subject: AbuseReportSubject,
    home_url: &str,
) -> anyhow::Result<()> {
    let req = AbuseReportDeliverRequest {
        report_ref: row.id.clone(),
        subject,
        reason: AbuseReportReason::from_token(&row.reason),
        note: row.note.clone(),
        excerpt: row.excerpt.clone(),
        origin_nest_id: state.handle_domain(),
        origin_nest_url: state.handle_domain_if_set().map(|d| format!("https://{d}")),
        subject_actor: row.subject_actor.clone(),
        extra: BTreeMap::new(),
    };
    let now = crate::db::now_epoch_secs();
    let id = state
        .db
        .enqueue_abuse_report_call(
            &row.id,
            ABUSE_CALL_DELIVER,
            home_url,
            None,
            &encode(&req)?,
            now,
        )
        .await?;
    let Some(entry) = state
        .db
        .abuse_report_call(&row.id, ABUSE_CALL_DELIVER)
        .await?
        .filter(|e| e.id == id)
    else {
        return Ok(());
    };
    if tokio::time::timeout(INLINE_DELIVER_WAIT, attempt(state, &entry))
        .await
        .is_err()
    {
        tracing::debug!("abuse report delivery still pending; the forwarder retries it");
    }
    Ok(())
}

/// The reporter withdrew a local report: a delivery that never left is
/// dropped; one that reached the home nest is followed by a withdrawal. A
/// delivery in flight right now is covered from the other side — its landing
/// queues the withdrawal ([`crate::db::CacheDb::mark_abuse_report_forwarded`]).
pub(crate) async fn propagate_withdrawal(
    state: &Arc<AppState>,
    row: &AbuseReportRow,
) -> anyhow::Result<()> {
    if state.db.cancel_abuse_report_deliver(&row.id).await? {
        return Ok(());
    }
    let Some(peer_url) = row.forwarded_to.as_deref() else {
        return Ok(());
    };
    state
        .db
        .enqueue_abuse_report_call(
            &row.id,
            ABUSE_CALL_WITHDRAW,
            peer_url,
            row.forwarded_nest_id.as_deref(),
            &withdraw_payload(&row.id)?,
            crate::db::now_epoch_secs(),
        )
        .await?;
    nudge(state);
    Ok(())
}

// ── home side ───────────────────────────────────────────────────────────────

/// The home admin resolved a forwarded copy: return the outcome — only the
/// outcome — to the origin nest, the one party holding the reporter's
/// identity, pinned to the nest the delivery was verified as.
pub(crate) async fn return_outcome(
    state: &Arc<AppState>,
    row: &AbuseReportRow,
    outcome: AbuseReportOutcome,
) -> anyhow::Result<()> {
    let (Some(origin_hex), Some(report_ref)) = (&row.origin_nest_id, &row.origin_report_ref) else {
        return Ok(());
    };
    let origin = fauna_core::hex32::decode(origin_hex)?;
    // Any address this nest has for the verified origin — a dial-proven one
    // first. The dial is pinned to the origin's id, so an unproven sighting
    // can route the call but never redirect it.
    let Some(peer_url) = state
        .db
        .resolve_foreign_nest_urls(&origin, 1)
        .await?
        .into_iter()
        .next()
    else {
        tracing::warn!("abuse report outcome: no address for the origin nest");
        return Ok(());
    };
    let payload = encode(&AbuseReportFederationOutcomeRequest {
        report_ref: report_ref.clone(),
        outcome,
        extra: BTreeMap::new(),
    })?;
    state
        .db
        .enqueue_abuse_report_call(
            &row.id,
            ABUSE_CALL_OUTCOME,
            &peer_url,
            Some(origin_hex),
            &payload,
            crate::db::now_epoch_secs(),
        )
        .await?;
    nudge(state);
    Ok(())
}

// ── the queue ───────────────────────────────────────────────────────────────

/// What one attempt at a queued call came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The peer accepted it; the call left the queue.
    Delivered,
    /// The peer refused it for good (a subject it does not host, a kind it
    /// does not serve), or the call aged out; it left the queue.
    Dropped,
    /// Not reached this time; retried later.
    Deferred,
}

/// A refusal worth retrying: the peer is throttling, timing out, or failing
/// internally. Anything else is its considered answer.
fn transient(err: &RpcError) -> bool {
    [
        ".rate_limited",
        ".timeout",
        ".internal",
        ".disconnected",
        ".unavailable",
    ]
    .iter()
    .any(|suffix| err.code.ends_with(suffix))
}

/// Try one queued call.
pub async fn attempt(state: &Arc<AppState>, entry: &AbuseReportOutboxEntry) -> CallOutcome {
    match try_call(state, entry).await {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!("abuse report {} call: {e:#}", entry.kind);
            CallOutcome::Deferred
        }
    }
}

async fn try_call(
    state: &Arc<AppState>,
    entry: &AbuseReportOutboxEntry,
) -> anyhow::Result<CallOutcome> {
    let now = crate::db::now_epoch_secs();
    let drop = |why: &str| {
        tracing::warn!("abuse report {} call dropped: {why}", entry.kind);
    };
    let (Some(kind), Ok(payload)) = (
        kind_for(&entry.kind),
        decode_strict::<Value>(&entry.payload),
    ) else {
        drop("unreadable entry");
        state.db.finish_abuse_report_call(entry.id).await?;
        return Ok(CallOutcome::Dropped);
    };
    if now - entry.created_at > MAX_CALL_AGE_SECS {
        drop("no answer within the retry window");
        state.db.finish_abuse_report_call(entry.id).await?;
        return Ok(CallOutcome::Dropped);
    }
    // A delivery is pinned too: to whichever nest answers at the URL now, so
    // the id recorded for the withdrawal and the outcome is the one reached.
    let pin = match &entry.peer_nest_id {
        Some(hex) => Some(fauna_core::hex32::decode(hex)?),
        None => match state
            .federation_pool
            .resolve_peer_nest_id(&entry.peer_url)
            .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::debug!("abuse report {}: peer unresolved: {e}", entry.kind);
                return defer(state, entry, now).await;
            }
        },
    };
    let reply = crate::federation_pool::originate_abuse_report_call(
        &state.federation_pool,
        state,
        &entry.peer_url,
        pin.as_ref().map(|p| p.as_slice()),
        kind,
        payload,
    )
    .await;
    match reply {
        Ok(Ok(_)) => {
            if entry.kind == ABUSE_CALL_DELIVER
                && let Some(pin) = pin
            {
                state
                    .db
                    .mark_abuse_report_forwarded(
                        &entry.report_id,
                        &entry.peer_url,
                        &hex::encode(pin),
                        &withdraw_payload(&entry.report_id)?,
                        now,
                    )
                    .await?;
            }
            state.db.finish_abuse_report_call(entry.id).await?;
            Ok(CallOutcome::Delivered)
        }
        Ok(Err(err)) if transient(&err) => defer(state, entry, now).await,
        Ok(Err(err)) => {
            drop(&err.code);
            state.db.finish_abuse_report_call(entry.id).await?;
            Ok(CallOutcome::Dropped)
        }
        Err(e) => {
            tracing::debug!("abuse report {}: {e}", entry.kind);
            defer(state, entry, now).await
        }
    }
}

async fn defer(
    state: &AppState,
    entry: &AbuseReportOutboxEntry,
    now: i64,
) -> anyhow::Result<CallOutcome> {
    let shift = entry.attempts.clamp(0, 20) as u32;
    let delay = FIRST_RETRY_DELAY_SECS
        .saturating_mul(1i64 << shift)
        .min(MAX_RETRY_DELAY_SECS);
    state
        .db
        .defer_abuse_report_call(entry.id, now + delay)
        .await?;
    Ok(CallOutcome::Deferred)
}

/// Drain every call due now; returns how many were attempted.
pub async fn drain_abuse_report_calls(state: &Arc<AppState>) -> usize {
    let due = match state
        .db
        .due_abuse_report_calls(crate::db::now_epoch_secs(), DRAIN_BATCH)
        .await
    {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!("abuse report queue read: {e:#}");
            return 0;
        }
    };
    for entry in &due {
        attempt(state, entry).await;
    }
    due.len()
}

/// Try the queue now rather than at the next tick.
fn nudge(state: &Arc<AppState>) {
    let state2 = state.clone();
    state.spawn_scoped(async move {
        drain_abuse_report_calls(&state2).await;
    });
}

/// The forwarder: drains the triad's queue every [`DRAIN_INTERVAL`].
pub fn spawn_abuse_report_forwarder(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        DRAIN_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                drain_abuse_report_calls(&state).await;
            }
        },
    ));
}

// ── serving handlers ────────────────────────────────────────────────────────

/// Is `subject` hosted here — an account of this nest, a post one of them
/// authored, a message one of them sent? A nest is never a relay for reports
/// about third nests.
async fn hosts(
    state: &AppState,
    subject: &AbuseReportSubject,
    subject_actor: Option<&str>,
) -> Result<Option<String>, RpcError> {
    let local = |actor: [u8; 32]| async move {
        state
            .db
            .is_actor_registered(&actor)
            .await
            .map_err(internal)
            .map(|registered| registered.then(|| hex::encode(actor)))
    };
    match subject {
        AbuseReportSubject::Actor { actor_id } => {
            let actor = fauna_core::hex32::decode(actor_id).map_err(|_| invalid("actor_id"))?;
            local(actor).await
        }
        AbuseReportSubject::Post { cid } => {
            let id = fauna_core::hex32::decode(cid).map_err(|_| invalid("cid"))?;
            match state
                .db
                .abuse_report_post_author(&id)
                .await
                .map_err(internal)?
            {
                Some(author) => local(author).await,
                None => Ok(None),
            }
        }
        // A sealed message's sender is known only to the reporting client;
        // the origin's word names them, and the account must be ours.
        AbuseReportSubject::Message { .. } => {
            match subject_actor.and_then(|a| fauna_core::hex32::decode(a).ok()) {
                Some(actor) => local(actor).await,
                None => Ok(None),
            }
        }
        // A kind this nest cannot resolve to an account: the forwarded report
        // is refused, never filed against a guessed author.
        AbuseReportSubject::Unknown(_) => Err(invalid("subject")),
    }
}

fn deliver_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: AbuseReportDeliverRequest = decode_strict(&payload).map_err(malformed)?;
            if req.report_ref.is_empty() || req.report_ref.len() > 64 {
                return Err(invalid("report_ref"));
            }
            let subject = crate::moderation_handlers::canonical_abuse_subject(req.subject)?;
            let note = req.note.filter(|n| !n.trim().is_empty());
            if note
                .as_ref()
                .is_some_and(|n| n.len() > MAX_ABUSE_REPORT_NOTE_BYTES)
            {
                return Err(invalid("note too long"));
            }
            let excerpt = req.excerpt.filter(|e| !e.trim().is_empty());
            if excerpt
                .as_ref()
                .is_some_and(|e| e.len() > MAX_ABUSE_REPORT_EXCERPT_BYTES)
            {
                return Err(invalid("excerpt too long"));
            }
            let Some(subject_actor) = hosts(&state, &subject, req.subject_actor.as_deref()).await?
            else {
                return Err(not_hosted());
            };
            // The origin's declared address, recorded against its verified
            // id, is how the outcome finds its way back.
            crate::federation_handlers::resolve_origin_home_url(
                &state,
                &origin_nest_id,
                req.origin_nest_url.as_deref(),
            )
            .await?;
            let channel = match &subject {
                AbuseReportSubject::Message { channel, .. } => Some(channel.clone()),
                _ => None,
            };
            let copy = ForwardedAbuseReport {
                origin_nest_id: hex::encode(origin_nest_id),
                origin_report_ref: req.report_ref,
                subject_kind: subject.kind().to_string(),
                subject_id: subject.id().to_string(),
                subject_channel: channel,
                subject_actor: Some(subject_actor),
                reason: req.reason.token().to_string(),
                note,
                excerpt,
            };
            let now = fauna_core::data::Timestamp::now().as_i64();
            if let Some(row) = state
                .db
                .insert_forwarded_abuse_report(copy, now)
                .await
                .map_err(internal)?
                && let Err(e) =
                    crate::moderation_handlers::ring_abuse_report_doorbell(&state, &row).await
            {
                tracing::error!("forwarded abuse report doorbell: {}", e.code);
            }
            ack()
        })
    })
}

fn withdraw_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: AbuseReportFederationWithdrawRequest =
                decode_strict(&payload).map_err(malformed)?;
            state
                .db
                .withdraw_forwarded_abuse_report(&hex::encode(origin_nest_id), &req.report_ref)
                .await
                .map_err(internal)?;
            ack()
        })
    })
}

fn outcome_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: AbuseReportFederationOutcomeRequest =
                decode_strict(&payload).map_err(malformed)?;
            if req.outcome == AbuseReportOutcome::Unknown {
                return Err(invalid("outcome must be acted or dismissed"));
            }
            let now = fauna_core::data::Timestamp::now().as_i64();
            if let Some(row) = state
                .db
                .record_forwarded_abuse_outcome(
                    &req.report_ref,
                    &hex::encode(origin_nest_id),
                    req.outcome.token(),
                    now,
                )
                .await
                .map_err(internal)?
                && let Some(reporter) = row
                    .reporter_actor
                    .as_deref()
                    .and_then(|r| <[u8; 32]>::try_from(r).ok())
                && let Err(e) = crate::moderation_handlers::notify_abuse_reporter(
                    &state,
                    &reporter,
                    &row.id,
                    req.outcome,
                )
                .await
            {
                tracing::error!("forwarded abuse report outcome notification: {}", e.code);
            }
            ack()
        })
    })
}

/// Register the triad beside the report-exchange pair. All three are
/// idempotent on this side — a delivery keyed on the origin's `report_ref`, a
/// withdrawal and an outcome that change nothing the second time — so each is
/// retry-safe (`forbid_replay: false`).
pub fn register_abuse_report_federation_handlers(b: &mut FederationRouterBuilder) {
    for (kind, handler) in [
        (DELIVER_KIND, deliver_handler()),
        (WITHDRAW_KIND, withdraw_handler()),
        (OUTCOME_KIND, outcome_handler()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_is_named_by_its_host() {
        assert_eq!(url_host("https://b.example"), "b.example");
        assert_eq!(url_host("https://b.example/"), "b.example");
        assert_eq!(url_host("http://127.0.0.1:8443/x"), "127.0.0.1:8443");
        assert_eq!(url_host("b.example"), "b.example");
    }

    #[test]
    fn only_a_throttle_timeout_or_fault_is_retried() {
        let e = |code: &str| RpcError::new(code, "x");
        assert!(transient(&e("fauna.protocol.rate_limited")));
        assert!(transient(&e("fauna.protocol.timeout")));
        assert!(transient(&e("fauna.federation.internal")));
        assert!(!transient(&e("fauna.federation.not_found")));
        assert!(!transient(&e("fauna.protocol.unauthenticated")));
    }
}
