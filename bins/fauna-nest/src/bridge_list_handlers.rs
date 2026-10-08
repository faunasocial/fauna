//! User-class mailing-list RPC handlers (`fauna.bridges.*_account_list` +
//! the member sub-surface) — item #10a,
//! backing `docs/goal/behavior/mail-mass-mailing.md` § Wire shapes.
//!
//! A list is **per-user**: every handler derives the owning actor from the
//! authenticated connection (`actor_id`), never a wire param, and scopes every
//! read/write by it (`owner_actor_id = caller`). The member sub-surface first
//! verifies ownership via [`CacheDb::get_list_for_owner`] (a list the caller
//! does not own resolves to `not_found`, never leaking another user's members).
//!
//! The DB layer ([`crate::db::mail_lists`]) owns the SQL; this layer owns
//! decode → validate → derive-token → call-DB → encode. The one-click token for
//! a subscription is derived here from the nest-held deployment secret
//! ([`fauna_mail::lists::UnsubscribeTokenGenerator`]) over the canonical
//! (lower-cased) address, so it matches the cached
//! `mail_list_members.one_click_unsubscribe_token` index the HTTPS / mailto
//! unsubscribe handlers (#4 / #5) resolve against.
//!
//! Deferred to their consuming items: `send_list_message` +
//! `list_list_send_history` (#6 + #10b — they need the list-mode submission
//! pipeline + a send-history table) and the Admin `rotate_list_unsubscribe_secret`
//! (#11).

use std::sync::Arc;
use std::time::Duration;

use serde_bytes::ByteBuf;

use fauna_protocol::bridge_routing::{
    AddListMemberReply, AddListMemberRequest, BatchImportListMembersReply,
    BatchImportListMembersRequest, CreateAccountListReply, CreateAccountListRequest,
    DeleteAccountListReply, DeleteAccountListRequest, ListAccountListsReply,
    ListAccountListsRequest, ListListMembersReply, ListListMembersRequest,
    ListListSendHistoryReply, ListListSendHistoryRequest, ListSendHistoryRow, MailListMemberRow,
    MailListRow, ResubscribeListMemberReply, ResubscribeListMemberRequest,
    RotateListUnsubscribeSecretReply, RotateListUnsubscribeSecretRequest, SendListMessageReply,
    SendListMessageRequest, UnsubscribeListMemberReply, UnsubscribeListMemberRequest,
    UpdateAccountListReply, UpdateAccountListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::bridge_routing_handlers::{
    conflicts_with_existing_alias, encode_reply, internal, malformed, not_found,
    primary_mail_domain, require_class, submit_outbound, validate_alias_pattern, validate_label,
};
use crate::db::mail_lists::{
    AddMemberOutcome, ListQuotaOutcome, ListSendCaps, ListWriteError, MailListMemberRecord,
    MailListRecord,
};
use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
use crate::db::{now_epoch_millis, now_epoch_secs};
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

use fauna_mail::lists::{ListHeaderInputs, UnsubscribeTokenGenerator, list_headers};
use uuid::Uuid;

// ── shared helpers ──────────────────────────────────────────────

fn list_id_from_wire(b: &ByteBuf) -> Result<[u8; 16], RpcError> {
    b.as_ref()
        .try_into()
        .map_err(|_| malformed("list_id must be 16 bytes"))
}

/// `fauna.bridges.recipient_on_local_domain` — a list member address points at
/// a domain this deployment hosts (§ Don't "allow a list to send to recipients
/// on `mail.local_domains`" — add an alias, not a list member).
fn recipient_on_local_domain() -> RpcError {
    RpcError::new(
        "fauna.bridges.recipient_on_local_domain",
        "error.bridges.recipient_on_local_domain",
    )
}

/// `fauna.bridges.list_per_send_cap_exceeded` — the send has more recipients
/// than the per-list per-send ceiling (§ The per-send cap, SMTP `552 5.3.4`, no
/// auto-chunking). Hard reject: the owner sends to fewer recipients / splits.
fn list_per_send_cap_exceeded(cap: i64) -> RpcError {
    RpcError::new(
        "fauna.bridges.list_per_send_cap_exceeded",
        "error.bridges.list_per_send_cap_exceeded",
    )
    .with_details_text(format!(
        "a list send may not exceed {cap} recipients (552 5.3.4); split into multiple sends"
    ))
}

/// `fauna.bridges.list_daily_cap_exceeded` — the per-account or per-deployment
/// per-day list-recipient ceiling would be exceeded (§ per-day caps, SMTP
/// `452 4.7.0`, tempfail). The owner retries the next UTC day.
fn list_daily_cap_exceeded(scope: &str, cap: i64) -> RpcError {
    RpcError::new(
        "fauna.bridges.list_daily_cap_exceeded",
        "error.bridges.list_daily_cap_exceeded",
    )
    .with_details_text(format!(
        "the {scope} daily list-recipient limit ({cap}) is reached (452 4.7.0); try again tomorrow"
    ))
}

/// Build the deployment's one-click token generator from the nest-held secret.
async fn list_token_generator(
    state: &Arc<AppState>,
) -> Result<UnsubscribeTokenGenerator, RpcError> {
    let secret = state
        .db
        .get_active_list_unsubscribe_secret()
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("list unsubscribe secret unavailable (not seeded)"))?;
    let key: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| internal("list unsubscribe secret is not 32 bytes"))?;
    Ok(UnsubscribeTokenGenerator::new(key))
}

/// The deployment's hosted domain names (for the member local-domain reject).
async fn local_domain_names(state: &Arc<AppState>) -> Result<Vec<String>, RpcError> {
    Ok(state
        .db
        .list_active_mail_domains()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|d| d.domain_name)
        .collect())
}

/// Validate + canonicalize one member address: RFC 5321 syntactic and **not**
/// on a hosted domain (`recipient_on_local_domain`). Returns the canonical
/// (trimmed, lower-cased) address used for both storage and token derivation,
/// so the token matches the `UNIQUE(list_id, recipient_address)` key.
fn canonical_member_address(address: &str, local_domains: &[&str]) -> Result<String, RpcError> {
    use fauna_mail::forward_config::ForwardTargetError;
    let canonical = address.trim().to_ascii_lowercase();
    match fauna_mail::validate_forward_target(&canonical, local_domains) {
        Ok(()) => Ok(canonical),
        Err(ForwardTargetError::IsLocalDomain) => Err(recipient_on_local_domain()),
        Err(other) => Err(malformed(other)),
    }
}

/// Per-list `recipients_per_send` override is bounded by the admin ceiling
/// (§ Don't "let a list's per-recipient rate-cap be raised above the admin
/// ceiling"). The `ceiling` is the admin-tunable effective policy
/// (`get_mass_mailing_policy().effective().list_recipients_per_send_ceiling`,
/// #7); the create/update handlers resolve it before calling. `None` = no
/// override. A non-positive value is malformed.
fn validate_recipients_per_send(value: Option<i64>, ceiling: i64) -> Result<(), RpcError> {
    if let Some(v) = value {
        if v <= 0 {
            return Err(malformed("recipients_per_send must be positive"));
        }
        if v > ceiling {
            return Err(malformed(format!(
                "recipients_per_send {v} exceeds the admin ceiling {ceiling}"
            )));
        }
    }
    Ok(())
}

fn list_record_to_row(r: MailListRecord) -> MailListRow {
    MailListRow {
        list_id: ByteBuf::from(r.list_id.to_vec()),
        alias_id: ByteBuf::from(r.alias_id.to_vec()),
        owner_actor_id: ByteBuf::from(r.owner_actor_id),
        local_domain: r.local_domain,
        pattern: r.pattern,
        friendly_name: r.friendly_name,
        description: r.description,
        list_help_url: r.list_help_url,
        list_archive_url: r.list_archive_url,
        recipients_per_send: r.recipients_per_send,
        created_at: r.created_at,
        last_send_at: r.last_send_at,
        member_count: r.member_count,
        sends_today: r.sends_today,
        recipients_today: r.recipients_today,
    }
}

fn member_record_to_row(r: MailListMemberRecord) -> MailListMemberRow {
    MailListMemberRow {
        member_id: ByteBuf::from(r.member_id.to_vec()),
        recipient_address: r.recipient_address,
        subscribed_at: r.subscribed_at,
        unsubscribed_at: r.unsubscribed_at,
    }
}

/// Resolve a caller-owned list or return `not_found` — the ownership gate every
/// member-surface handler runs first.
async fn owned_list(
    state: &Arc<AppState>,
    list_id: &[u8; 16],
    actor_id: &[u8; 32],
) -> Result<MailListRecord, RpcError> {
    state
        .db
        .get_list_for_owner(list_id, actor_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("list not found or not owned by caller"))
}

// ── list CRUD ───────────────────────────────────────────────────

fn list_account_lists_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_account_lists").await?;
            let _req: ListAccountListsRequest = decode(&payload).map_err(malformed)?;
            let records = state
                .db
                .list_lists_for_actor(&actor_id)
                .await
                .map_err(internal)?;
            let lists = records.into_iter().map(list_record_to_row).collect();
            // The per-account meter + the cap a send is checked against, for
            // the compose form's pre-send "Today's quota: N / M" (§ Composing a
            // list message). The cap is the same effective value
            // `send_list_message` reserves under.
            let account_recipients_today = state
                .db
                .account_list_recipients_today(&actor_id, now_epoch_millis())
                .await
                .map_err(internal)?;
            let account_recipients_per_day = state
                .db
                .get_mass_mailing_policy()
                .await
                .map_err(internal)?
                .effective()
                .list_recipients_per_account_per_day_ceiling
                as i64;
            encode_reply(&ListAccountListsReply {
                lists,
                account_recipients_today,
                account_recipients_per_day,
            })
        })
    })
}

pub(crate) fn create_account_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.create_account_list").await?;
            let req: CreateAccountListRequest = decode(&payload).map_err(malformed)?;
            if req.local_domain.trim().is_empty() {
                return Err(malformed("local_domain must not be empty"));
            }
            // Same reserved-local-part list + character class the alias CRUD
            // enforces (a list address is a sixth alias kind). #5 extends the
            // reserved set with `unsubscribe`; reading it from the same source
            // means create_account_list picks that up for free.
            let reserved_owned: Vec<String> = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective()
                .reserved_local_parts;
            let reserved: Vec<&str> = reserved_owned.iter().map(String::as_str).collect();
            validate_alias_pattern(&req.local_part, &reserved)?;
            if let Some(name) = &req.friendly_name {
                validate_label(name)?;
            }
            let per_send_ceiling = state
                .db
                .get_mass_mailing_policy()
                .await
                .map_err(internal)?
                .effective()
                .list_recipients_per_send_ceiling as i64;
            validate_recipients_per_send(req.recipients_per_send, per_send_ceiling)?;

            // A list's posting address shares the exact-key tier with exact
            // aliases and admin forwarders: `create_list` refuses a key either
            // one already holds (one address, one holder), mapped to
            // `conflicts_with_existing_alias` like its UNIQUE conflict.
            let (list_id, _alias_id) = state
                .db
                .create_list(
                    &actor_id,
                    &req.local_domain,
                    &req.local_part,
                    req.friendly_name.as_deref(),
                    req.description.as_deref(),
                    req.list_help_url.as_deref(),
                    req.list_archive_url.as_deref(),
                    req.recipients_per_send,
                )
                .await
                .map_err(map_list_write_err)?;
            encode_reply(&CreateAccountListReply {
                list_id: ByteBuf::from(list_id.to_vec()),
            })
        })
    })
}

fn update_account_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.update_account_list").await?;
            let req: UpdateAccountListRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            if let Some(name) = &req.friendly_name {
                validate_label(name)?;
            }
            let per_send_ceiling = state
                .db
                .get_mass_mailing_policy()
                .await
                .map_err(internal)?
                .effective()
                .list_recipients_per_send_ceiling as i64;
            validate_recipients_per_send(req.recipients_per_send, per_send_ceiling)?;
            let ok = state
                .db
                .update_list_metadata(
                    &list_id,
                    &actor_id,
                    req.friendly_name.as_deref(),
                    req.description.as_deref(),
                    req.list_help_url.as_deref(),
                    req.list_archive_url.as_deref(),
                    req.recipients_per_send,
                )
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("list not found or not owned by caller"));
            }
            encode_reply(&UpdateAccountListReply { ok: true })
        })
    })
}

fn delete_account_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.delete_account_list").await?;
            let req: DeleteAccountListRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            let ok = state
                .db
                .delete_list(&list_id, &actor_id)
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("list not found or not owned by caller"));
            }
            encode_reply(&DeleteAccountListReply { ok: true })
        })
    })
}

// ── member sub-surface ──────────────────────────────────────────

fn list_list_members_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_list_members").await?;
            let req: ListListMembersRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            let records = state
                .db
                .list_members(&list_id, req.include_unsubscribed)
                .await
                .map_err(internal)?;
            let (subscribed_count, unsubscribed_count) =
                state.db.count_members(&list_id).await.map_err(internal)?;
            let members = records.into_iter().map(member_record_to_row).collect();
            encode_reply(&ListListMembersReply {
                members,
                subscribed_count,
                unsubscribed_count,
            })
        })
    })
}

fn add_list_member_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.add_list_member").await?;
            let req: AddListMemberRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            let domains_owned = local_domain_names(&state).await?;
            let domains: Vec<&str> = domains_owned.iter().map(String::as_str).collect();
            let address = canonical_member_address(&req.recipient_address, &domains)?;
            let token = list_token_generator(&state)
                .await?
                .token_for(&list_id, &address);
            let outcome = state
                .db
                .add_member(&list_id, &address, &token)
                .await
                .map_err(internal)?;
            let added = matches!(outcome, AddMemberOutcome::Added(_));
            encode_reply(&AddListMemberReply {
                member_id: ByteBuf::from(outcome.member_id().to_vec()),
                added,
            })
        })
    })
}

fn batch_import_list_members_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.batch_import_list_members").await?;
            let req: BatchImportListMembersRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            let max = state
                .db
                .get_mass_mailing_policy()
                .await
                .map_err(internal)?
                .effective()
                .list_max_import_per_batch as usize;
            if req.addresses.len() > max {
                return Err(malformed(format!(
                    "batch import of {} exceeds the {max}-address limit",
                    req.addresses.len()
                )));
            }
            let domains_owned = local_domain_names(&state).await?;
            let domains: Vec<&str> = domains_owned.iter().map(String::as_str).collect();
            let generator = list_token_generator(&state).await?;
            // Validate + canonicalize + tokenize. Syntactically invalid /
            // local-domain addresses are skipped (counted, never fatal — a bad
            // line in a paste-list must not sink the whole import). The DB layer
            // then counts duplicates; both are surfaced in the reply.
            let mut valid: Vec<(String, String)> = Vec::with_capacity(req.addresses.len());
            let mut skipped_invalid: u32 = 0;
            for raw in &req.addresses {
                match canonical_member_address(raw, &domains) {
                    Ok(address) => {
                        let token = generator.token_for(&list_id, &address);
                        valid.push((address, token));
                    }
                    Err(_) => skipped_invalid += 1,
                }
            }
            let (added, skipped_duplicate) = state
                .db
                .batch_add_members(&list_id, &valid)
                .await
                .map_err(internal)?;
            encode_reply(&BatchImportListMembersReply {
                added: added as u32,
                skipped_invalid,
                skipped_duplicate: skipped_duplicate as u32,
            })
        })
    })
}

fn unsubscribe_list_member_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.unsubscribe_list_member").await?;
            let req: UnsubscribeListMemberRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            let address = req.recipient_address.trim().to_ascii_lowercase();
            let ok = state
                .db
                .unsubscribe_member_by_address(&list_id, &address)
                .await
                .map_err(internal)?;
            encode_reply(&UnsubscribeListMemberReply { ok })
        })
    })
}

fn resubscribe_list_member_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.resubscribe_list_member").await?;
            let req: ResubscribeListMemberRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            let address = req.recipient_address.trim().to_ascii_lowercase();
            let ok = state
                .db
                .resubscribe_member_by_address(&list_id, &address)
                .await
                .map_err(internal)?;
            encode_reply(&ResubscribeListMemberReply { ok })
        })
    })
}

// ── list send (#10b / #6 / #7) ──────────────────────────────────

/// `fauna.bridges.send_list_message` — the canonical list-send fan-out
/// (§ Composing a list message). Validates ownership + the per-list rate caps
/// (§ Per-list rate accounting), then enqueues one outbound per subscribed
/// member with that member's `List-*` headers stamped into the body (the
/// nest signs each copy at the outbound hand-out; the RFC 8058 List-* are in
/// the signed `h=` set). This is the ONLY list-send path — an
/// external SMTP submission with MAIL FROM = a list address is rejected at
/// `enqueue_outbound_mail` (per-recipient stamping is impossible for a single
/// body). The per-list rate caps are reserved atomically *before* the fan-out;
/// the deployment fresh-IP warm-up gate (inside `submit_outbound`) is the
/// independent broader admission control and may defer individual recipients to
/// the next day without affecting the reservation.
fn send_list_message_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.send_list_message").await?;
            let req: SendListMessageRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            let list = owned_list(&state, &list_id, &actor_id).await?;

            // RFC 5322 §3.6: exactly one From field, before any cap is reserved
            // (`smtp-server.md` § Architectural rules → *Exactly one From
            // field*). The outbound worker picks each copy's DKIM key by the
            // From domain, read from the LAST From field, while a receiver's
            // DMARC may align against the first.
            let from_fields = fauna_mail::from_field::from_field_count(&req.message);
            if from_fields != 1 {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "bridges",
                    format!(
                        "message must carry exactly one From header field, found {from_fields}"
                    ),
                ));
            }

            let members = state
                .db
                .list_subscribed_members_for_send(&list_id)
                .await
                .map_err(internal)?;

            // Effective caps: the per-list `recipients_per_send` override (≤ the
            // ceiling, enforced at create/update) else the admin-tunable ceiling.
            let policy = state
                .db
                .get_mass_mailing_policy()
                .await
                .map_err(internal)?
                .effective();
            let per_send = list
                .recipients_per_send
                .unwrap_or(policy.list_recipients_per_send_ceiling as i64);
            let caps = ListSendCaps {
                per_send,
                per_account_per_day: policy.list_recipients_per_account_per_day_ceiling as i64,
                per_deployment_per_day: policy.list_recipients_per_deployment_per_day_ceiling
                    as i64,
            };

            // An empty list is a no-op (don't consume quota / record a send).
            if members.is_empty() {
                return encode_reply(&SendListMessageReply {
                    queued_count: 0,
                    estimated_quota_remaining: caps.per_account_per_day.max(0) as u64,
                });
            }
            let recipient_count = members.len() as i64;

            let now_ms = now_epoch_millis();
            let account_remaining = match state
                .db
                .try_consume_list_quota(&list_id, &actor_id, recipient_count, caps, now_ms)
                .await
                .map_err(internal)?
            {
                ListQuotaOutcome::Allowed {
                    account_remaining, ..
                } => account_remaining,
                ListQuotaOutcome::PerSendExceeded { cap } => {
                    return Err(list_per_send_cap_exceeded(cap));
                }
                ListQuotaOutcome::PerAccountDayExceeded { cap, .. } => {
                    return Err(list_daily_cap_exceeded("per-account", cap));
                }
                ListQuotaOutcome::PerDeploymentDayExceeded { cap, .. } => {
                    return Err(list_daily_cap_exceeded("per-deployment", cap));
                }
            };

            // Shared per-send context. The envelope sender is the list's own
            // posting address (local-domain → SPF-aligned); the From header is
            // the client's (composed message) — the Go worker selects the DKIM
            // key by the From domain. One Message-ID for the whole fan-out (the
            // delivered message keeps the client's own Message-ID header).
            let primary_domain = primary_mail_domain(&state)
                .await?
                .unwrap_or_else(|| list.local_domain.clone());
            let list_id_label = Uuid::from_bytes(list_id).to_string();
            let original_sender = format!("{}@{}", list.pattern, list.local_domain);
            let msgid = format!("<{}@{}>", Uuid::new_v4(), list.local_domain);
            let now_secs = now_epoch_secs();

            let mut queued: u64 = 0;
            for m in &members {
                let hdrs = list_headers(&ListHeaderInputs {
                    friendly_name: list.friendly_name.as_deref().unwrap_or(""),
                    list_id_label: &list_id_label,
                    list_pattern: &list.pattern,
                    list_domain: &list.local_domain,
                    primary_domain: &primary_domain,
                    token: &m.one_click_unsubscribe_token,
                    list_help_url: list.list_help_url.as_deref(),
                    list_archive_url: list.list_archive_url.as_deref(),
                });
                let body = fauna_mail::lists::stamp_list_headers_on_message(&req.message, &hdrs);
                let recipients = [m.recipient_address.as_str()];
                let ids = submit_outbound(
                    &state,
                    NewOutbound {
                        original_msgid: &msgid,
                        original_sender: &original_sender,
                        recipients: &recipients,
                        raw_message: &body,
                        inbound_verdicts: InboundVerdictsSnapshot {
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
                    now_secs,
                    // List mail counts against the deployment warm-up quota
                    // (mail-deliverability.md § Reading list) and is DKIM-signed
                    // at delivery (Option C seam).
                    true,
                )
                .await?;
                queued += ids.len() as u64;
            }

            state
                .db
                .record_list_send(
                    &list_id,
                    &actor_id,
                    now_ms,
                    recipient_count,
                    queued as i64,
                    0,
                )
                .await
                .map_err(internal)?;

            // ONE durable Sent copy for the whole send — the client-composed
            // message, not the per-member stamped copies — so the owner's sent
            // mail shows the send once, on every device (§ Composing a list
            // message, step 5). The app's local echo dedups against it by the
            // client's Message-ID, exactly as for `fauna.email.send`.
            // Best-effort like that path: the fan-out is already queued, so a
            // Sent-copy failure is logged, never a send failure (a failure here
            // would invite a retry that double-sends to every member).
            if let Err(e) = crate::bridge_routing_handlers::seal_and_store_sent_copy(
                &state,
                &actor_id,
                &req.message,
                &list.local_domain,
            )
            .await
            {
                tracing::warn!(
                    error = %e.code,
                    "storing server-side Sent copy for fauna.bridges.send_list_message failed"
                );
            }

            encode_reply(&SendListMessageReply {
                queued_count: queued,
                estimated_quota_remaining: account_remaining.max(0) as u64,
            })
        })
    })
}

fn list_list_send_history_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_list_send_history").await?;
            let req: ListListSendHistoryRequest = decode(&payload).map_err(malformed)?;
            let list_id = list_id_from_wire(&req.list_id)?;
            owned_list(&state, &list_id, &actor_id).await?;
            // Cap the page size defensively (a 0 means "use a sane default").
            let limit = if req.limit == 0 {
                100
            } else {
                req.limit.min(1000)
            };
            let records = state
                .db
                .list_send_history(&list_id, limit)
                .await
                .map_err(internal)?;
            let sends = records
                .into_iter()
                .map(|r| ListSendHistoryRow {
                    sent_at: r.sent_at,
                    recipient_count: r.recipient_count,
                    delivered_count: r.delivered_count,
                    unsubscribed_during_send: r.unsubscribed_during_send,
                })
                .collect();
            encode_reply(&ListListSendHistoryReply { sends })
        })
    })
}

// ── admin ───────────────────────────────────────────────────────

fn rotate_list_unsubscribe_secret_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.rotate_list_unsubscribe_secret",
            )
            .await?;
            let _req: RotateListUnsubscribeSecretRequest = decode(&payload).map_err(malformed)?;
            let count = state
                .db
                .rotate_list_unsubscribe_secret()
                .await
                .map_err(internal)?;
            encode_reply(&RotateListUnsubscribeSecretReply {
                members_retokenized: count as u64,
            })
        })
    })
}

fn map_list_write_err(e: ListWriteError) -> RpcError {
    match e {
        ListWriteError::Conflict => conflicts_with_existing_alias(),
        ListWriteError::Other(err) => internal(err),
    }
}

/// Register the user-class mailing-list kinds. Called from `lib.rs::build_app`
/// alongside `register_bridge_routing_handlers`.
pub fn register_bridge_list_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.list_account_lists",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_account_lists_handler(),
        },
    );
    b.add(
        "fauna.bridges.create_account_list",
        RpcKindMeta {
            // Server-enforced UNIQUE(local_domain, pattern, kind): an auto-retry
            // replay surfaces as a spurious conflict, so the caller re-decides
            // (mirrors `create_account_alias`).
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: create_account_list_handler(),
        },
    );
    b.add(
        "fauna.bridges.update_account_list",
        RpcKindMeta {
            // Idempotent full-overwrite (same payload twice → same state).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: update_account_list_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_account_list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_account_list_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_list_members",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_list_members_handler(),
        },
    );
    b.add(
        "fauna.bridges.add_list_member",
        RpcKindMeta {
            // Idempotent on the UNIQUE(list_id, address) key (a duplicate returns
            // the existing member), so a replay is safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: add_list_member_handler(),
        },
    );
    b.add(
        "fauna.bridges.batch_import_list_members",
        RpcKindMeta {
            // Idempotent (duplicates skipped); larger payload → a longer deadline.
            forbid_replay: false,
            default_deadline: Duration::from_secs(15),
            handler: batch_import_list_members_handler(),
        },
    );
    b.add(
        "fauna.bridges.unsubscribe_list_member",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: unsubscribe_list_member_handler(),
        },
    );
    b.add(
        "fauna.bridges.resubscribe_list_member",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: resubscribe_list_member_handler(),
        },
    );
    b.add(
        "fauna.bridges.send_list_message",
        RpcKindMeta {
            // NOT idempotent (each call fans out + consumes rate quota); an
            // auto-retry replay would double-send. A larger fan-out → a wider
            // deadline than the CRUD kinds.
            forbid_replay: true,
            default_deadline: Duration::from_secs(60),
            handler: send_list_message_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_list_send_history",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_list_send_history_handler(),
        },
    );
    b.add(
        "fauna.bridges.rotate_list_unsubscribe_secret",
        RpcKindMeta {
            // Each call mints a *new* secret + re-tokenizes; an auto-retry replay
            // would needlessly invalidate the just-minted tokens, so forbid it.
            // Re-tokenization is one transaction over every member → a wider
            // deadline than the CRUD kinds.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: rotate_list_unsubscribe_secret_handler(),
        },
    );
}
