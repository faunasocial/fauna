//! Permanent-failure NDR (bounce) generation for the production
//! outbound path.
//!
//! When the Go MTA bridge gives up on an `outbound_mail_queue` row (a 5xx
//! that classified permanent, no MX, or retry-budget exhausted) it calls
//! `fauna.bridges.mark_outbound_bounced`. This module is what that handler
//! invokes to turn the terminal row into a real RFC 3464 Non-Delivery
//! Report sent back to the original sender — closing the "send to a bad
//! address fails silently" gap (`docs/goal/behavior/smtp-server.md`
//! §§ Permanent-failure bounce generation / NDR rate-limit per recipient /
//! Backscatter suppression).
//!
//! It mirrors the legacy in-process queue orchestration
//! (`libs/fauna-bridge-smtp/src/queue.rs`, which deletes at the I4/I5
//! cutover) but consumes the **shared** pure builders in
//! `fauna_mail::outbound::{dsn, backscatter}` plus the nest outbound DB
//! methods, so production nest carries no dependency on the legacy crate.

use anyhow::Result;
use fauna_mail::outbound::backscatter::{
    self, InboundVerdictsSnapshot as MailVerdicts, SuppressReason as MailSuppress,
    SuppressorToggles,
};
use fauna_mail::outbound::dsn::{DsnAction, DsnReport, build_dsn};

use std::sync::Arc;

use crate::bridge_routing_handlers::submit_outbound;
use crate::db::CacheDb;
use crate::db::outbound::{
    InboundVerdictsSnapshot as DbVerdicts, NewOutbound, OutboundRow, SuppressReason as DbSuppress,
};
use crate::routes::AppState;

/// Fallback enhanced status when the bridge's `reason` carries no
/// `5.x.x` token (e.g. a retry-budget timeout, whose underlying wire
/// error is a 4xx). RFC 3463 generic permanent failure.
const DEFAULT_PERMANENT_STATUS: &str = "5.0.0";

/// What the bounce pipeline decided for one terminal row. Returned for
/// logging / metrics; the DB status transition has already been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BounceOutcome {
    /// DSN built + enqueued back to the sender; row → `bounced`.
    Bounced,
    /// A forwarded row permanently failed: DSN built + sealed straight into
    /// the **forwarder**'s INBOX (a known in-domain actor) via the shared
    /// sealed-ingest path, no MX relay (`mail-forwarding.md` § NDR routing,
    /// N4b); row → `bounced`. Distinct from `Bounced` so the cold reader (and
    /// the tests) can tell the forwarder NDR from the to-sender NDR.
    BouncedToForwarder,
    /// A backscatter suppressor fired; row → `suppressed_backscatter`.
    SuppressedBackscatter,
    /// A bounce was already sent for this `(sender, msgid)` within the
    /// admin-configured NDR rate-limit window; row → `suppressed_rate`.
    SuppressedRate,
}

/// Run the permanent-failure NDR pipeline for one bounced row:
/// backscatter suppression → NDR rate-limit → DSN build → enqueue
/// (null-sender, addressed to the original sender) → record bounce.
/// Owns the terminal status transition in every branch.
pub async fn generate_permfail_bounce(
    state: &Arc<AppState>,
    row: &OutboundRow,
    reason: &str,
    now: i64,
) -> Result<BounceOutcome> {
    let db: &CacheDb = &state.db;
    let enhanced = parse_enhanced_status(reason);
    let final_was_5xx = enhanced.starts_with('5');
    // Admin-configured NDR rate-limit window (`mail-policy-config.md` §
    // Implementation status today; `smtp-server.md` § NDR rate-limit per
    // recipient). `ndr_rate_limit_days` is a pure allowance knob — `0` is a
    // legitimate admin choice ("no rate limiting beyond a same-instant
    // duplicate") and gets no floor, unlike `permanent_failure_timeout_hours`
    // (`mail-policy-config.md` ruling 2).
    let ndr_rate_limit_secs = db
        .get_outbound_policy()
        .await?
        .effective()
        .ndr_rate_limit_days as i64
        * 86_400;

    // 0. Forwarded rows bounce to the **forwarder**, not the original sender,
    //    sealed straight into the forwarder's INBOX (a known in-domain actor)
    //    (`mail-forwarding.md` § NDR routing :195-:203, N4b). This is a
    //    distinct mechanism from the to-original-sender DSN below; the
    //    `ForwardedAndExternallyBounced` suppressor that fires in step 1 only
    //    governs the (suppressed) bounce *to the original sender*.
    if row.is_forwarded
        && let Some(forward_actor) = row.forward_actor_id
    {
        return generate_forwarder_ndr(
            state,
            row,
            &forward_actor,
            reason,
            &enhanced,
            now,
            ndr_rate_limit_secs,
        )
        .await;
    }

    // 1. Backscatter suppression — evaluated against the inbound verdicts
    //    snapshot persisted on the row at DATA-accept time.
    let verdicts = MailVerdicts {
        spf: row.inbound_verdicts.spf.clone(),
        dmarc: row.inbound_verdicts.dmarc.clone(),
        dmarc_policy: row.inbound_verdicts.dmarc_policy.clone(),
    };
    if let Some(reason_kind) = backscatter::should_suppress(
        &verdicts,
        &row.original_sender,
        row.is_forwarded,
        final_was_5xx,
        &SuppressorToggles::default(),
    ) {
        db.mark_outbound_suppressed(row.id, map_suppress(reason_kind))
            .await?;
        tracing::info!(
            id = row.id,
            recipient = %row.recipient,
            sender = %row.original_sender,
            reason = ?reason_kind,
            "outbound bounce suppressed (backscatter)"
        );
        return Ok(BounceOutcome::SuppressedBackscatter);
    }

    // 2. NDR rate-limit per (sender, msgid) over the admin-configured window.
    if db
        .bounce_rate_limit_hit(
            &row.original_sender,
            &row.original_msgid,
            now,
            ndr_rate_limit_secs,
        )
        .await?
    {
        db.mark_outbound_suppressed(row.id, DbSuppress::NdrRateLimit)
            .await?;
        tracing::info!(
            id = row.id,
            recipient = %row.recipient,
            sender = %row.original_sender,
            "outbound bounce suppressed (NDR rate-limit)"
        );
        return Ok(BounceOutcome::SuppressedRate);
    }

    // 3. Build the RFC 3464 DSN.
    let reporting_mta = reporting_domain(db, &row.original_sender).await;
    let headers = extract_headers(&row.raw_message);
    let arrival = format_rfc2822(row.created_at);
    let last_attempt = format_rfc2822(now);
    let summary = format!(
        "Message to <{}> was not delivered. The remote server said: {}",
        row.recipient, reason
    );
    let report = DsnReport {
        reporting_mta: &reporting_mta,
        arrival_date: &arrival,
        last_attempt_date: &last_attempt,
        recipient: &row.recipient,
        original_sender: &row.original_sender,
        status: &enhanced,
        action: DsnAction::Failed,
        diagnostic_code: reason,
        original_headers: &headers,
        failure_summary_text: &summary,
    };
    let dsn_bytes = build_dsn(&report);

    // 4. Submit the DSN as a new null-envelope-sender message addressed to the
    //    original sender (RFC 5321 §4.5.5) through the `submit_outbound`
    //    chokepoint. An **external** sender enqueues onto the outbound queue +
    //    retry curve (the null-sender backscatter rule short-circuits a
    //    bounce-of-a-bounce on its own first failure); an **in-domain** sender
    //    (a local user whose own outbound send permanently failed) delivers
    //    locally into their sealed INBOX instead of MX-self-looping — which on a
    //    containerized deploy would hairpin and `554`-bounce at the inbound
    //    HELO-identity check (smtp-server.md § Outbound submission flow).
    let bounce_msgid = format!(
        "<bounce-{}-{}@{}>",
        now,
        row.id,
        reporting_mta.replace('@', "")
    );
    submit_outbound(
        state,
        NewOutbound {
            original_msgid: &bounce_msgid,
            original_sender: "",
            recipients: &[row.original_sender.as_str()],
            raw_message: &dsn_bytes,
            inbound_verdicts: DbVerdicts {
                spf: "none".into(),
                dmarc: "none".into(),
                dmarc_policy: "none".into(),
            },
            is_forwarded: false,
            forward_actor_id: None,
            forward_rule_id: None,
            forward_copy_mode: None,
            submit_actor_id: None,
        },
        now,
        // An NDR/DSN is never warm-up-deferred (it must reach the original
        // sender promptly; mail-deliverability.md scopes warm-up to authed
        // 465/587 submissions).
        false,
    )
    .await
    .map_err(|e| anyhow::anyhow!("submit_outbound DSN: {}", e.code))?;

    // 5. Record the bounce for the NDR rate-limit and flip the original
    //    row to `bounced`, persisting the bridge's reason for correlation.
    db.record_bounce(&row.original_sender, &row.original_msgid, now)
        .await?;
    db.mark_outbound_bounced_with_reason(row.id, reason).await?;
    tracing::info!(
        id = row.id,
        recipient = %row.recipient,
        sender = %row.original_sender,
        status = %enhanced,
        "outbound NDR enqueued"
    );
    Ok(BounceOutcome::Bounced)
}

/// Permanent-failure NDR for a **forwarded** row: the bounce goes to the
/// forwarding-config owner, NOT the original sender (`mail-forwarding.md`
/// § NDR routing — "The forwarder gets the bounce, NOT the original
/// sender").
///
/// The forwarder is **always a known in-domain actor** (we hold its 32-byte
/// id on the row's `forward_actor_id`), so — exactly like a security
/// notification (`security_notify.rs` Channel 3) — we seal the
/// self-generated DSN straight into the forwarder's sealed INBOX via
/// `seal_and_ingest_local`, with no MX relay. The older design enqueued the
/// DSN to the forward row's own `SRS0=…@<primary-domain>` envelope and
/// relied on the inbound SRS-bounce path (N4a) to decode the short-id back
/// to this actor; that workaround predated `seal_and_ingest_local` (nest
/// could not then seal a self-generated message to a mailbox), but on a
/// containerized deploy the loopback hairpins through the docker bridge and
/// `554`-bounces at the inbound HELO-identity check — the same self-loop
/// class fixed for in-domain mailbox / DSN / auto-reply delivery. Sealing
/// directly removes the hairpin and the catch-all-capture risk, and needs
/// neither the SRS secret nor a round-trip (we already hold the forwarder).
/// `decode_srs_bounce` / N4a still serve **external** downstream-MX bounces
/// that legitimately arrive over the wire as `SRS0=…` recipients.
///
/// Owns the terminal status transition in every branch.
async fn generate_forwarder_ndr(
    state: &Arc<AppState>,
    row: &OutboundRow,
    forward_actor: &[u8; 32],
    reason: &str,
    enhanced: &str,
    now: i64,
    ndr_rate_limit_secs: i64,
) -> Result<BounceOutcome> {
    let db: &CacheDb = &state.db;

    // 1. Don't bounce a bounce (`:207`): a null-sender source must not
    //    generate an NDR. N2 already rejects null-sender forwards at enqueue;
    //    this is defense-in-depth for any terminal null-sender forwarded row.
    if row.original_sender.is_empty() {
        db.mark_outbound_suppressed(row.id, DbSuppress::NullSender)
            .await?;
        tracing::info!(id = row.id, "forward NDR suppressed (null source sender)");
        return Ok(BounceOutcome::SuppressedBackscatter);
    }

    // 2. NDR rate-limit per (forwarder, source-msgid) over the admin-
    //    configured window (`:209`). The key is the forwarding actor (hex),
    //    distinct from the original-sender bucket the to-sender path uses.
    let forwarder_key = forwarder_ndr_key(forward_actor);
    if db
        .bounce_rate_limit_hit(
            &forwarder_key,
            &row.original_msgid,
            now,
            ndr_rate_limit_secs,
        )
        .await?
    {
        db.mark_outbound_suppressed(row.id, DbSuppress::NdrRateLimit)
            .await?;
        tracing::info!(
            id = row.id,
            forwarder = %forwarder_key,
            "forward NDR suppressed (NDR rate-limit)"
        );
        return Ok(BounceOutcome::SuppressedRate);
    }

    // 3. The forwarder is a known in-domain actor; we seal the DSN straight to
    //    it (step 5). Without a primary mail domain we cannot build a
    //    well-formed DSN (`From: postmaster@<domain>`) — mark bounced WITHOUT a
    //    DSN rather than fall through and leak the bounce to the original
    //    sender.
    let Some(domain) = db
        .lookup_primary_mail_domain()
        .await?
        .map(|d| d.domain_name)
    else {
        tracing::error!(
            id = row.id,
            "forward NDR: missing primary mail domain; cannot address the DSN \
             without leaking to the original sender — marking bounced, no DSN"
        );
        db.mark_outbound_bounced_with_reason(row.id, reason).await?;
        return Ok(BounceOutcome::Bounced);
    };

    // 4. Build the RFC 3464 DSN. The `To:` header is the forwarder's handle
    //    address; the seal target (step 5) is the actor id regardless, so a
    //    handle-less forwarder still receives the bounce (`To:` falls back to
    //    `postmaster@<domain>`).
    let to_addr = match db.get_handle(forward_actor).await? {
        Some(handle) => format!("{handle}@{domain}"),
        None => format!("postmaster@{domain}"),
    };
    let dsn_bytes = forwarder_dsn_bytes(row, &domain, &to_addr, reason, enhanced, now);

    // 5. Seal the DSN directly into the forwarder's sealed INBOX — no MX
    //    loopback (which would hairpin + `554`-bounce on a containerized
    //    deploy). A seal failure (e.g. the forwarder has no encryption key on
    //    file) must NOT fall through to the original sender — mark bounced
    //    without a DSN.
    //    `MailIngress::System`: a forwarder NDR is nest-generated. Holding a
    //    delivery-failure notice behind a guardian's review would strand a
    //    supervised forwarder with no way to learn their mail bounced
    //    (`family-safety.md` § The mail gate).
    if let Err(e) = crate::bridge_routing_handlers::seal_and_ingest_local(
        state,
        forward_actor,
        &dsn_bytes,
        &domain,
        fauna_core::data::MailIngress::System,
        // Nest-generated: no alias was matched, so there is nothing to stamp.
        &[],
    )
    .await
    {
        tracing::error!(
            id = row.id,
            error = %e.code,
            "forward NDR: local seal to forwarder failed; marking bounced, no DSN"
        );
        db.mark_outbound_bounced_with_reason(row.id, reason).await?;
        return Ok(BounceOutcome::Bounced);
    }

    // 6. Record the bounce for the (forwarder, source-msgid) rate-limit key
    //    and flip the original forwarded row to `bounced`.
    db.record_bounce(&forwarder_key, &row.original_msgid, now)
        .await?;
    db.mark_outbound_bounced_with_reason(row.id, reason).await?;
    tracing::info!(
        id = row.id,
        forwarder = %forwarder_key,
        downstream = %row.recipient,
        status = %enhanced,
        "forward NDR sealed to forwarder INBOX (local delivery)"
    );
    Ok(BounceOutcome::BouncedToForwarder)
}

/// Build the RFC 3464 DSN bytes for a forwarded-row NDR: the failed
/// recipient is the downstream address; the `To:` header is `to_addr` (the
/// forwarder); the tail blurb names the rule, the original sender, and the
/// downstream address (`mail-forwarding.md:203`). Pure — no DB, no seal — so
/// the DSN shape stays unit-testable after the body is sealed into the
/// forwarder's INBOX.
fn forwarder_dsn_bytes(
    row: &OutboundRow,
    domain: &str,
    to_addr: &str,
    reason: &str,
    enhanced: &str,
    now: i64,
) -> Vec<u8> {
    let headers = extract_headers(&row.raw_message);
    let arrival = format_rfc2822(row.created_at);
    let last_attempt = format_rfc2822(now);
    let rule = row.forward_rule_id.as_deref().unwrap_or("forward-all");
    let blurb = format!(
        "This bounce was generated because your forward-rule '{rule}' attempted to \
         forward a message from {} to {}, and the destination MX rejected it \
         permanently. You may want to remove or fix the rule.",
        row.original_sender, row.recipient
    );
    let report = DsnReport {
        reporting_mta: domain,
        arrival_date: &arrival,
        last_attempt_date: &last_attempt,
        recipient: &row.recipient,
        original_sender: to_addr,
        status: enhanced,
        action: DsnAction::Failed,
        diagnostic_code: reason,
        original_headers: &headers,
        failure_summary_text: &blurb,
    };
    build_dsn(&report)
}

/// `bounce_history` / NDR rate-limit key for a forward NDR: the forwarding
/// actor as hex, prefixed so it can never collide with a real original-sender
/// address. The forward-NDR window is per (forwarder, source-msgid)
/// (`mail-forwarding.md:209`), distinct from the to-sender window.
fn forwarder_ndr_key(actor: &[u8; 32]) -> String {
    format!("forwarder:{}", hex::encode(actor))
}

/// Reporting-MTA for the DSN headers: the deployment's primary local
/// domain, falling back to the original sender's domain when no primary
/// is claimed (degraded but still a well-formed DSN).
pub(crate) async fn reporting_domain(db: &CacheDb, original_sender: &str) -> String {
    if let Ok(domains) = db.list_active_mail_domains().await
        && let Some(primary) = domains.iter().find(|d| d.is_primary)
    {
        return primary.domain_name.clone();
    }
    original_sender
        .rsplit_once('@')
        .map(|(_, d)| d.to_string())
        .unwrap_or_else(|| "localhost".to_string())
}

fn map_suppress(reason: MailSuppress) -> DbSuppress {
    match reason {
        MailSuppress::SpfHardfail => DbSuppress::SpfHardfail,
        MailSuppress::DmarcRejectQuarantine => DbSuppress::DmarcRejectQuarantine,
        MailSuppress::NullSender => DbSuppress::NullSender,
        MailSuppress::ForwardedAndExternallyBounced => DbSuppress::ForwardedAndExternallyBounced,
    }
}

/// Pluck a `5.x.y` enhanced-status token out of the bridge's free-form
/// reason string; default to a generic permanent status when none is
/// present (e.g. retry-budget timeouts whose wire error was a 4xx).
fn parse_enhanced_status(reason: &str) -> String {
    for tok in reason.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if tok.starts_with("5.") && tok.matches('.').count() == 2 {
            let parts: Vec<&str> = tok.split('.').collect();
            if parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            {
                return tok.to_string();
            }
        }
    }
    DEFAULT_PERMANENT_STATUS.to_string()
}

/// Slice the RFC 5322 header block (everything up to and including the
/// header-terminating blank line's first CRLF). The DSN carries headers
/// only, never the body (RFC 3464 anti-amplification).
pub(crate) fn extract_headers(raw: &[u8]) -> Vec<u8> {
    if let Some(idx) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
        return raw[..idx + 2].to_vec();
    }
    if let Some(idx) = raw.windows(2).position(|w| w == b"\n\n") {
        return raw[..idx + 1].to_vec();
    }
    raw.to_vec()
}

/// Format `epoch` seconds as an RFC 2822 UTC date-time
/// (`Day, DD Mon YYYY HH:MM:SS +0000`). Civil-date math delegates to the
/// canonical `fauna_core::caltime::civil_from_days` (dep-free, no chrono in
/// the production nest binary).
pub(crate) fn format_rfc2822(epoch: i64) -> String {
    fauna_core::imf_date::format_rfc5322_date(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::db::mail_policy::OutboundPolicyOverrides;
    use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound, OutboundStatus};
    use crate::routes::AppState;
    use std::sync::Arc;

    /// A test `AppState` with **no** mail domain configured, so `submit_outbound`
    /// classifies every recipient as external — exactly the pre-`submit_outbound`
    /// direct-enqueue behaviour these suppression / rate-limit / DSN-shape tests
    /// assert against. The in-domain local-delivery short-circuit is exercised
    /// separately by `dsn_to_in_domain_sender_delivers_locally_no_relay`.
    async fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test(Arc::new(
            CacheDb::open_in_memory().unwrap(),
        )))
    }

    fn verdicts(spf: &str, dmarc: &str, pol: &str) -> InboundVerdictsSnapshot {
        InboundVerdictsSnapshot {
            spf: spf.into(),
            dmarc: dmarc.into(),
            dmarc_policy: pol.into(),
        }
    }

    async fn seed(
        db: &CacheDb,
        sender: &str,
        msgid: &str,
        v: InboundVerdictsSnapshot,
        forwarded: bool,
    ) -> OutboundRow {
        let raw = b"From: a@x\r\nTo: b@y\r\nMessage-ID: <m@x>\r\nSubject: hi\r\n\r\nbody".to_vec();
        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: msgid,
                original_sender: sender,
                recipients: &["nonexistent@external.test"],
                raw_message: &raw,
                inbound_verdicts: v,
                is_forwarded: forwarded,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();
        db.fetch_outbound_by_id(ids[0]).await.unwrap().unwrap()
    }

    /// Seed an `is_forwarded` row: a forward of `original_sender`'s message
    /// to the external `downstream`, attributed to `forwarder`.
    async fn seed_forwarded(
        db: &CacheDb,
        forwarder: &[u8; 32],
        original_sender: &str,
        downstream: &str,
        msgid: &str,
        rule: &str,
    ) -> OutboundRow {
        let raw =
            b"From: bob@external.test\r\nTo: alice@fwd.test\r\nMessage-ID: <m@x>\r\nSubject: hi\r\n\r\nbody"
                .to_vec();
        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: msgid,
                original_sender,
                recipients: &[downstream],
                raw_message: &raw,
                inbound_verdicts: verdicts("pass", "pass", "none"),
                is_forwarded: true,
                forward_actor_id: Some(forwarder),
                forward_rule_id: Some(rule),
                forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                submit_actor_id: None,
            })
            .await
            .unwrap();
        db.fetch_outbound_by_id(ids[0]).await.unwrap().unwrap()
    }

    // ── N4b — synchronous-permfail bounce → forwarder ────────────────

    #[tokio::test]
    async fn forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        db.add_mail_domain("fwd.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        // The forwarder is a real in-domain actor with an encryption key + handle.
        let forwarder = [9u8; 32];
        crate::test_support::seed_recipient_seal_key(
            db,
            &forwarder,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        db.set_handle(&forwarder, "fwadmin").await.unwrap();
        let now = 1_000_000;
        let row = seed_forwarded(
            db,
            &forwarder,
            "bob@external.test",
            "alice@example.net",
            "src-1@external.test",
            "forward-all",
        )
        .await;

        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 mailbox unknown", now)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::BouncedToForwarder);

        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        // Original forwarded row flipped to bounced.
        assert_eq!(
            rows.iter().find(|r| r.id == row.id).unwrap().status,
            OutboundStatus::Bounced
        );
        // NO new outbound row: the DSN sealed locally into the forwarder's
        // INBOX — never an MX-relay / `SRS0=` loopback (which would hairpin and
        // `554`-bounce on a containerized deploy at the inbound HELO-identity
        // check), and never the original sender (no backscatter).
        assert!(
            !rows.iter().any(|r| r.recipient == "bob@external.test"),
            "must never bounce to the original sender"
        );
        assert!(
            !rows.iter().any(|r| r.recipient.starts_with("SRS0=")),
            "no SRS0 MX loopback — the forwarder NDR delivers locally now"
        );
        assert!(
            !rows.iter().any(|r| r.original_sender.is_empty()),
            "no null-sender DSN relay row enqueued"
        );
        // The forwarder received the NDR sealed into their INBOX.
        let inbox = db
            .query_bridge_imap_messages(&forwarder, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "the forwarder must receive the NDR in their sealed INBOX"
        );
        // Bounce recorded under the forwarder key (not the original sender),
        // so the rate-limit bucket is per-forwarder.
        let bh = db.fetch_bounce_history_for_test().await.unwrap();
        assert_eq!(bh.len(), 1);
        assert_eq!(
            bh[0]["original_sender"],
            format!("forwarder:{}", hex::encode(forwarder))
        );
        assert_eq!(bh[0]["original_msgid"], "src-1@external.test");
    }

    /// The forwarder NDR's RFC 3464 DSN names the rule, the original sender,
    /// and the downstream failed recipient — verified on the pure builder
    /// (`forwarder_dsn_bytes`) since the delivered copy is sealed into the
    /// forwarder's INBOX and unreadable from the test.
    #[tokio::test]
    async fn forwarder_dsn_body_carries_rule_and_recipients() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed_forwarded(
            db,
            &[9u8; 32],
            "bob@external.test",
            "alice@example.net",
            "src-1@external.test",
            "forward-all",
        )
        .await;
        let body = String::from_utf8(forwarder_dsn_bytes(
            &row,
            "fwd.test",
            "fwadmin@fwd.test",
            "550 5.1.1 mailbox unknown",
            "5.1.1",
            1_000_000,
        ))
        .unwrap();
        assert!(body.contains("multipart/report"), "DSN body: {body}");
        assert!(body.contains("To: fwadmin@fwd.test"), "DSN body: {body}");
        assert!(
            body.contains("Final-Recipient: rfc822; alice@example.net"),
            "DSN body: {body}"
        );
        assert!(body.contains("Status: 5.1.1"), "DSN body: {body}");
        assert!(body.contains("forward-all"), "tail blurb: {body}");
        assert!(body.contains("bob@external.test"), "tail blurb: {body}");
        assert!(body.contains("alice@example.net"), "tail blurb: {body}");
    }

    #[tokio::test]
    async fn forwarded_permfail_rate_limited_per_forwarder_and_msgid() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        db.add_mail_domain("fwd.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        // Both forwarders are real in-domain actors with encryption keys, so
        // the direct local seal succeeds and the outcome is `BouncedToForwarder`.
        let forwarder = [9u8; 32];
        crate::test_support::seed_recipient_seal_key(
            db,
            &forwarder,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let row = seed_forwarded(
            db,
            &forwarder,
            "bob@external.test",
            "alice@example.net",
            "dup-src@external.test",
            "forward-all",
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::BouncedToForwarder
        );
        // A second forward of the SAME source message by the SAME forwarder
        // (e.g. another downstream attempt) is rate-limited within 7 days.
        let row2 = seed_forwarded(
            db,
            &forwarder,
            "bob@external.test",
            "alice@example.net",
            "dup-src@external.test",
            "forward-all",
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row2, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::SuppressedRate
        );
        // A DIFFERENT forwarder with the same source-msgid is NOT rate-limited
        // (the window is per (forwarder, source-msgid)).
        let other = [3u8; 32];
        crate::test_support::seed_recipient_seal_key(
            db,
            &other,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let row3 = seed_forwarded(
            db,
            &other,
            "bob@external.test",
            "alice@example.net",
            "dup-src@external.test",
            "forward-all",
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row3, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::BouncedToForwarder
        );
        // Two bounce_history rows: one per forwarder, both for the same msgid.
        assert_eq!(db.fetch_bounce_history_for_test().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn forwarded_null_source_sender_suppresses() {
        // Defense-in-depth: don't bounce a bounce (`:207`). N2 already rejects
        // null-sender forwards at enqueue, but a terminal null-sender forwarded
        // row must still suppress rather than NDR.
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        db.add_mail_domain("fwd.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        let forwarder = [9u8; 32];
        let row = seed_forwarded(
            db,
            &forwarder,
            "",
            "alice@example.net",
            "src@external.test",
            "forward-all",
        )
        .await;
        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::SuppressedBackscatter);
        // No DSN row beyond the original; no bounce recorded.
        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(db.fetch_bounce_history_for_test().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forwarded_permfail_without_primary_domain_does_not_leak() {
        // No primary mail domain → we cannot address a well-formed DSN. Mark
        // the row terminal WITHOUT a DSN rather than fall through and bounce to
        // the original sender (backscatter).
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let forwarder = [9u8; 32];
        let row = seed_forwarded(
            db,
            &forwarder,
            "bob@external.test",
            "alice@example.net",
            "src@external.test",
            "forward-all",
        )
        .await;
        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
            .await
            .unwrap();
        assert_ne!(outcome, BounceOutcome::BouncedToForwarder);
        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        // No leak: no row addressed to the original sender, no SRS0 loopback row.
        assert!(
            !rows.iter().any(|r| r.recipient == "bob@external.test"),
            "must not bounce to the original sender"
        );
        assert!(
            !rows.iter().any(|r| r.recipient.starts_with("SRS0=")),
            "no routable loopback could be built"
        );
        // Original row is terminal (not left pending for re-attempt).
        assert_ne!(
            rows.iter().find(|r| r.id == row.id).unwrap().status,
            OutboundStatus::Pending
        );
    }

    #[tokio::test]
    async fn forwarded_permfail_forwarder_without_key_does_not_leak() {
        // Primary domain present, but the forwarder has no encryption key on
        // file → the local seal fails. Must mark the row terminal WITHOUT a DSN
        // rather than fall through and bounce to the original sender.
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        db.add_mail_domain("fwd.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        let forwarder = [9u8; 32]; // deliberately NO seal key seeded
        let now = 1_000_000;
        let row = seed_forwarded(
            db,
            &forwarder,
            "bob@external.test",
            "alice@example.net",
            "src@external.test",
            "forward-all",
        )
        .await;
        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 nope", now)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::Bounced);
        // No leak: nothing to the original sender, no relay row, no SRS0 loopback.
        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            !rows.iter().any(|r| r.recipient == "bob@external.test"),
            "must not bounce to the original sender"
        );
        assert!(
            !rows.iter().any(|r| r.original_sender.is_empty()),
            "no DSN relay row enqueued"
        );
        assert_eq!(
            rows.iter().find(|r| r.id == row.id).unwrap().status,
            OutboundStatus::Bounced
        );
        // The seal failed BEFORE record_bounce → no rate-limit bucket consumed.
        assert!(db.fetch_bounce_history_for_test().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clean_permfail_enqueues_dsn_and_records_bounce() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed(
            db,
            "alice@local.test",
            "orig-1@local.test",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;

        let outcome =
            generate_permfail_bounce(&state, &row, "550 5.1.1 mailbox unknown", 1_000_000)
                .await
                .unwrap();
        assert_eq!(outcome, BounceOutcome::Bounced);

        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        // Original row flipped to bounced.
        let orig = rows.iter().find(|r| r.id == row.id).unwrap();
        assert_eq!(orig.status, OutboundStatus::Bounced);
        // A null-sender DSN row addressed back to the sender exists, and
        // its body is an RFC 3464 multipart/report.
        let dsn = rows
            .iter()
            .find(|r| r.original_sender.is_empty() && r.recipient == "alice@local.test")
            .expect("null-sender DSN row enqueued");
        let body = String::from_utf8_lossy(&dsn.raw_message);
        assert!(body.contains("multipart/report"), "DSN body: {body}");
        assert!(body.contains("Status: 5.1.1"), "DSN body: {body}");
        // Bounce recorded for the rate-limit key.
        let bh = db.fetch_bounce_history_for_test().await.unwrap();
        assert_eq!(bh.len(), 1);
        assert_eq!(bh[0]["original_sender"], "alice@local.test");
    }

    #[tokio::test]
    async fn null_sender_suppresses_backscatter() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed(
            db,
            "",
            "orig-2@local.test",
            verdicts("none", "none", "none"),
            false,
        )
        .await;

        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::SuppressedBackscatter);

        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(
            rows.iter().find(|r| r.id == row.id).unwrap().status,
            OutboundStatus::SuppressedBackscatter
        );
        // No DSN row, no bounce_history.
        assert_eq!(rows.len(), 1);
        assert!(db.fetch_bounce_history_for_test().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn spf_hardfail_suppresses_backscatter() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed(
            db,
            "spoofed@victim.test",
            "orig-3@x",
            verdicts("fail", "none", "none"),
            false,
        )
        .await;
        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::SuppressedBackscatter);
        assert!(db.fetch_bounce_history_for_test().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn second_bounce_in_window_suppresses_rate() {
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed(
            db,
            "carol@local.test",
            "dup@local.test",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;
        // First bounce goes through.
        assert_eq!(
            generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::Bounced
        );
        // A second terminal row for the SAME (sender, msgid) is rate-limited.
        let row2 = seed(
            db,
            "carol@local.test",
            "dup@local.test",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row2, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::SuppressedRate
        );
        // Exactly one bounce_history row for the pair.
        assert_eq!(db.fetch_bounce_history_for_test().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bounce_rate_limit_respects_admin_ndr_rate_limit_days_override() {
        // An admin-set `ndr_rate_limit_days` override must actually govern the
        // NDR suppression window — it used to be a silent no-op, hardcoded to
        // 7 days regardless of any stored override
        // (mail-policy-config.md § Implementation status today).
        let state = test_state().await;
        let db: &CacheDb = &state.db;
        let row = seed(
            db,
            "carol@local.test",
            "dup-override@local.test",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row, "550 5.1.1 nope", 1_000_000)
                .await
                .unwrap(),
            BounceOutcome::Bounced
        );

        // Narrow the window to 0 days — a legitimate admin allowance choice
        // ("no rate limiting beyond a same-instant duplicate"; ruled a pure
        // allowance knob, no floor, unlike `permanent_failure_timeout_hours`).
        db.put_outbound_policy(OutboundPolicyOverrides {
            ndr_rate_limit_days: Some(0),
            ..Default::default()
        })
        .await
        .unwrap();

        // A second terminal row for the SAME (sender, msgid) one hour later:
        // under the 7-day catalog default this would still be suppressed, but
        // the 0-day override must let it through — proving the stored value
        // reached the suppression decision, not just the struct.
        let row2 = seed(
            db,
            "carol@local.test",
            "dup-override@local.test",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;
        assert_eq!(
            generate_permfail_bounce(&state, &row2, "550 5.1.1 nope", 1_000_000 + 3_600)
                .await
                .unwrap(),
            BounceOutcome::Bounced,
            "0-day override should not rate-limit a bounce one hour later"
        );
    }

    #[test]
    fn parse_enhanced_status_extracts_5xx_else_default() {
        assert_eq!(parse_enhanced_status("550 5.1.1 mailbox unknown"), "5.1.1");
        assert_eq!(parse_enhanced_status("552 5.2.2 over quota"), "5.2.2");
        // Retry-budget reason embeds a 4xx → generic permanent default.
        assert_eq!(
            parse_enhanced_status("retry budget exhausted (10 attempts): 421 4.7.0 try later"),
            "5.0.0"
        );
        assert_eq!(
            parse_enhanced_status("no MX or A record for x.test"),
            "5.0.0"
        );
    }

    #[test]
    fn format_rfc2822_epoch_zero_is_thursday() {
        assert_eq!(format_rfc2822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        // 2026-05-23 12:00:00 UTC = 1779537600.
        assert_eq!(
            format_rfc2822(1_779_537_600),
            "Sat, 23 May 2026 12:00:00 +0000"
        );
    }

    #[test]
    fn extract_headers_stops_at_blank_line() {
        let raw = b"A: 1\r\nB: 2\r\n\r\nbody bytes";
        assert_eq!(extract_headers(raw), b"A: 1\r\nB: 2\r\n".to_vec());
    }

    // ── In-domain DSN local delivery (direct-enqueue self-loop fix) ──────────
    //
    // A bounce addressed back to an **in-domain** original sender (a local user
    // whose own outbound send permanently failed) must deliver into that user's
    // sealed INBOX, NEVER the MX-relay queue — routing it to MX self-loops and,
    // on a containerized deploy, `554`-bounces at the inbound HELO-identity
    // check (smtp-server.md § Outbound submission flow). `generate_permfail_bounce`
    // routes the to-sender DSN through the `submit_outbound` chokepoint, which
    // partitions in-domain recipients to local sealed delivery.

    /// A test `AppState` whose primary mail domain is `domain`, so
    /// `submit_outbound` classifies in-domain vs external recipients.
    async fn test_state_with_domain(domain: &str) -> Arc<AppState> {
        let st = AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()));
        st.db
            .add_mail_domain(domain, true, "testing", "self_signed", None, None)
            .await
            .unwrap();
        Arc::new(st)
    }

    #[tokio::test]
    async fn dsn_to_in_domain_sender_delivers_locally_no_relay() {
        let state = test_state_with_domain("example.com").await;
        let db: &CacheDb = &state.db;

        // A real local mailbox: alice@example.com with an MLS pubkey + exact alias.
        let alice = [7u8; 32];
        crate::test_support::seed_recipient_seal_key(
            db,
            &alice,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        db.put_exact_alias("example.com", "alice", "exact", &alice)
            .await
            .unwrap();

        // alice's own outbound send to an external address permanently failed.
        // (spf/dmarc=pass → no backscatter suppression of the to-sender DSN.)
        let now = 1_000_000;
        let row = seed(
            db,
            "alice@example.com",
            "<sent-1@example.com>",
            verdicts("pass", "pass", "none"),
            false,
        )
        .await;

        let outcome = generate_permfail_bounce(&state, &row, "550 5.1.1 mailbox unknown", now)
            .await
            .unwrap();
        assert_eq!(outcome, BounceOutcome::Bounced);

        // No NEW MX-relay row for the DSN — the in-domain bounce delivered
        // locally. The only outbound row is alice's original (now `bounced`);
        // a self-looping DSN would add a null-sender row to alice@example.com.
        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            !rows.iter().any(|r| r.original_sender.is_empty()),
            "in-domain DSN must NOT be enqueued for MX relay (would self-loop); rows: {:?}",
            rows.iter()
                .map(|r| (r.original_sender.clone(), r.recipient.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            rows.iter().find(|r| r.id == row.id).unwrap().status,
            OutboundStatus::Bounced
        );

        // alice received the bounce locally (sealed into her INBOX).
        let inbox = db
            .query_bridge_imap_messages(&alice, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "the in-domain original sender must receive the DSN in their sealed INBOX"
        );
    }
}
