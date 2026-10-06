//! WS-RPC handlers for the user-facing `fauna.email.*` surface — the
//! sieve-like per-account filter-rule CRUD (`fauna.email.filters.*`)
//! plus outbound submission (`fauna.email.send`). The goal-doc home
//! is `docs/goal/behavior/smtp-server.md` (§ Outbound submission flow
//! / § Email filter rules) and `mail-content-scanning.md`; the
//! namespace split from `fauna.bridges.*` reflects a deliberate namespace
//! decision made during the WS-RPC migration (tracked internally).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::
//! is_permitted` (User-only arms for these kinds); the same gate the
//! `fauna.bridges.*` kinds use, partitioned by `CallerClass`.
//!
//! `fauna_protocol::email::{EmailFilterRule, EmailFilterAction}` is the **sole
//! filter type** (canonical wire + on-disk shape — there is no separate "store"
//! mirror): rules serialize straight into `email_filters.rules` as canonical
//! dag-cbor (Layer-6 Domain E; this handler validates field lengths but does
//! not convert the rule type), and the action projects to/from the on-disk
//! `email_filters.action` string via `action_to_string` / `action_from_string`.
//! The Go MTA perimeter re-decodes the rule bytes into
//! `fauna_mail::filter::FilterCondition` to evaluate (`smtp-server.md` § Email
//! filter rules).
//!
//! `fauna.email.send` reuses `fauna_mail::routing` for recipient
//! parsing + partition, seals in-domain (Fauna→Fauna) recipients through
//! the shared sealed-ingest core
//! (`bridge_routing_handlers::persist_decoded_inbound_mail` → the `__mail`
//! segment store + `bridge_imap_messages` INBOX, so in-domain mail is
//! `fauna.email.inbox.fetch`/IMAP-visible), and enqueues out-of-domain
//! recipients via `state.db.enqueue_outbound`. The wire shape collapses
//! the legacy HTTP fast-paths (`{"delivered":"local"}`, etc.) into a
//! single `SendEmailReply` counter triple.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::carried::CarriedValue;
use fauna_mail::segments::placement::MailPlacementRecord;
use fauna_protocol::{
    RpcError, Value,
    bridge_routing::{MailboxStateEvent, MoveSide},
    decode_strict as decode,
    email::{
        ApplySpamDispositionReply, ApplySpamDispositionRequest, CreateEmailFilterReply,
        CreateEmailFilterRequest, DeleteEmailFilterReply, DeleteEmailFilterRequest,
        EmailFilter as WireEmailFilter, EmailFilterAction as WireEmailFilterAction,
        EmailFilterRule as WireEmailFilterRule, FlagChange, FlagChangesReply, FlagChangesRequest,
        GetEmailFilterReply, GetEmailFilterRequest, InboxFetchReply, InboxFetchRequest,
        InboxMessage, ListEmailFiltersReply, ListEmailFiltersRequest, MarkSeenReply,
        MarkSeenRequest, SPAM_SCORED_KEYWORD, SendEmailReply, SendEmailRequest,
        UpdateEmailFilterReply, UpdateEmailFilterRequest,
    },
};

use crate::bridge_imap_handlers::emit_mailbox_state_event;
use crate::db::EmailFilterRow;
use crate::db::bridge_imap::StoreFlagsDbOp;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── fauna.email.send constants ─────────────────────────────────
//
// Parity with the HTTP twin (`email_routes.rs` — being deleted in the
// same commit) on the outbound rate limit. The cap is a defensive
// boundary, not a feature gate, so it stays a module-level constant
// here rather than a per-deployment config knob.
pub const SEND_RATE_LIMIT_PER_HOUR: u32 = 100;
const SEND_RATE_WINDOW_SECS: i64 = 3600;

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("email", reason)
}

use crate::rpc_errors::internal;

pub(crate) fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("email", reason)
}

fn not_found(id: i64) -> RpcError {
    crate::rpc_errors::not_found_ns("email", format!("email filter {id} not found"))
}

/// The typed first-party sibling of the SMTP perimeter's `552 5.3.4`: a raw
/// message over `effective_max_raw_message_bytes` (`smtp-server.md` § Message
/// size limits). Shared code so `import_message` and `append` report the same
/// identifier. `size_bytes` is raw for the SMTP legs and the sealed
/// `ciphertext_size` for the APPEND leg (which only sees the sealed body);
/// `ceiling` is expressed in the same units the caller compared against.
pub(crate) fn message_too_large(size_bytes: usize, ceiling: u64) -> RpcError {
    let mut e = RpcError::new(
        fauna_protocol::email::MESSAGE_TOO_LARGE_CODE,
        "error.email.too_large",
    );
    e.details = Some(Box::new(Value::String(format!(
        "message of {size_bytes} bytes exceeds the {ceiling}-byte ceiling"
    ))));
    e
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── Length caps ─────────────────────────────────────────────────
//
// `tstr` lets CBOR carry any length; the HTTP twin doesn't enforce
// length caps today, but the WS-RPC handler picks defensible bounds at
// the boundary to keep one-actor write amplification predictable. Caps
// here are intentionally generous: filter names are user-visible labels
// and the rule/action payloads dominate row size anyway. Conservative
// upper bounds; tuned per the same shape as `bridges_ui_handlers`'s
// feed-subscription caps.
const MAX_NAME_LEN: usize = 256;
const MAX_RULES: usize = 64;
const MAX_TEXT_FIELD_LEN: usize = 2048;
const MAX_REJECT_REASON_LEN: usize = 512;
const MAX_AUTOREPLY_BODY_LEN: usize = 32 * 1024;
/// Cap the encoded rules-blob at the same 32 KiB envelope as the
/// auto-reply body — both are predominantly user-visible strings and
/// dwarf the size of the filter row's other fields.
const MAX_RULES_BYTES: usize = 32 * 1024;

// ── Wire ↔ storage projection ──────────────────────────────────

/// Field-length validation for one wire rule. The rule itself is serialized
/// verbatim into the `rules` BLOB (no type conversion — `EmailFilterRule` is
/// the canonical storage shape); this only rejects oversize string fields at
/// the write boundary to bound per-actor write amplification.
fn validate_wire_rule(r: &WireEmailFilterRule) -> Result<(), RpcError> {
    use WireEmailFilterRule as W;
    let too_long = |field: &str| invalid_params(&format!("rule.{field} too long"));
    match r {
        W::SenderIs { address } => {
            if address.len() > MAX_TEXT_FIELD_LEN {
                return Err(too_long("address"));
            }
        }
        W::SenderDomain { domain } => {
            if domain.len() > MAX_TEXT_FIELD_LEN {
                return Err(too_long("domain"));
            }
        }
        W::SubjectContains { text } | W::BodyContains { text } => {
            if text.len() > MAX_TEXT_FIELD_LEN {
                return Err(too_long("text"));
            }
        }
        W::HeaderExists { name } => {
            if name.len() > MAX_TEXT_FIELD_LEN {
                return Err(too_long("name"));
            }
        }
        W::HeaderContains { name, value } => {
            if name.len() > MAX_TEXT_FIELD_LEN || value.len() > MAX_TEXT_FIELD_LEN {
                return Err(too_long("name/value"));
            }
        }
        // Scaled int — no length to bound.
        W::SpamScoreAtLeast { .. } => {}
        // A condition a newer build added, echoed back by an app that listed
        // it: stored verbatim inside the blob (bounded by `MAX_RULES_BYTES`),
        // and it never matches.
        W::Unknown(_) => {}
    }
    Ok(())
}

/// Field-length validation for a filter action, shaped like the HTTP twin's
/// 400 bad-request surface. Runs before [`action_to_string`] persists the
/// action; the variant set is `fauna_protocol::email::EmailFilterAction`
/// (the canonical wire + storage type — there is no separate "store" mirror).
fn validate_action(a: &WireEmailFilterAction) -> Result<(), RpcError> {
    use WireEmailFilterAction as W;
    match a {
        W::Allow | W::Discard => {}
        W::Reject { reason } => {
            if reason.len() > MAX_REJECT_REASON_LEN {
                return Err(invalid_params("action.reason too long"));
            }
        }
        W::FileInto { mailbox } => {
            if mailbox.len() > MAX_TEXT_FIELD_LEN {
                return Err(invalid_params("action.mailbox too long"));
            }
            // Inbound mail matching the rule is filed here (email-filters.md
            // § Email filter rules). Refuse a mailbox inbound mail must never
            // enter — `Sent` and `Drafts` hold only this account's own writing,
            // the guardian's held mailbox holds only holds — and a name no
            // mailbox can carry. Placement re-screens both with the same two
            // predicates, for a rule stored before this check.
            if let Err(e) = fauna_protocol::email::validate_file_into_mailbox(mailbox) {
                return Err(invalid_params(&format!("action.mailbox {e}")));
            }
            if let Err(reason) = crate::db::bridge_imap::validate_mailbox_name(mailbox) {
                return Err(invalid_params(&format!(
                    "action.mailbox is not a valid mailbox name: {reason}"
                )));
            }
        }
        W::Forward { address, .. } => {
            if address.len() > MAX_TEXT_FIELD_LEN {
                return Err(invalid_params("action.address too long"));
            }
            // The rule's destination gets the RFC 5321 syntactic check at write
            // time (`mail-forwarding.md` § Per-rule "forward to") — the same
            // shared predicate the forward-all knob uses. No hosted-domain
            // list: only forward-all is barred from a domain we host.
            if let Err(e) = fauna_mail::validate_forward_target(address, &[]) {
                return Err(invalid_params(&format!("action.address {e}")));
            }
        }
        W::AutoReply {
            subject,
            body,
            interval_hours: _,
        } => {
            if subject.len() > MAX_TEXT_FIELD_LEN {
                return Err(invalid_params("action.subject too long"));
            }
            if body.len() > MAX_AUTOREPLY_BODY_LEN {
                return Err(invalid_params("action.body too long"));
            }
        }
        W::AddLabel { label } => {
            if label.len() > MAX_TEXT_FIELD_LEN {
                return Err(invalid_params("action.label too long"));
            }
            // A label rides inbound delivery as an IMAP keyword flag
            // (smtp-server.md § Email filter rules). Reject anything that isn't a
            // single RFC 3501 keyword `atom` — a system flag (`\Deleted` →
            // EXPUNGE-eligible, `\Seen` → silently read) or a whitespace-splittable
            // value would otherwise be honoured as several / privileged flags at
            // ingest. Shared validator so a future client dialog enforces the same.
            if let Err(e) = fauna_protocol::email::validate_label(label) {
                return Err(invalid_params(&format!("action.label {e}")));
            }
        }
        // The only unknown action this nest can store is the storage string it
        // handed out ([`action_from_string`]), echoed back by an app saving the
        // filter. A value carried from elsewhere has no storage form here; a
        // string this nest DOES parse would bypass the checks above.
        W::Unknown(CarriedValue(Value::String(stored))) => {
            if stored.len() > MAX_AUTOREPLY_BODY_LEN {
                return Err(invalid_params("action too long"));
            }
            if !matches!(action_from_string(stored), W::Unknown(_)) {
                return Err(invalid_params(
                    "action: a known action must be sent as its own variant",
                ));
            }
        }
        W::Unknown(_) => {
            return Err(invalid_params("action is not one this nest knows"));
        }
    }
    Ok(())
}

/// Serialize an action to the on-disk `email_filters.action` string shape —
/// mirrors the HTTP twin's `action_to_string` exactly so a filter written via
/// WS-RPC reads back through the HTTP route and vice-versa.
///
/// The string is not the whole storage form for `Forward`: its copy mode
/// rides the sibling `forward_redirect` column ([`action_to_storage`]), never
/// the string. A new prefix (`forward-redirect:`) would read as an unknown
/// action on an older nest binary opening a newer database — the rule doing
/// nothing at all after a downgrade — where an unknown column is simply ignored
/// and the rule degrades to `copy` (version-compatibility.md I4).
fn action_to_string(action: &WireEmailFilterAction) -> String {
    use WireEmailFilterAction as W;
    match action {
        W::Allow => "allow".into(),
        W::Discard => "discard".into(),
        W::Reject { reason } => format!("reject:{reason}"),
        W::FileInto { mailbox } => format!("fileinto:{mailbox}"),
        W::Forward { address, .. } => format!("forward:{address}"),
        W::AutoReply {
            subject,
            body,
            interval_hours,
        } => {
            format!("autoreply:{interval_hours}:{subject}\n{body}")
        }
        W::AddLabel { label } => format!("addlabel:{label}"),
        // Written back exactly as it was read. [`validate_action`] admits no
        // other unknown; were one to reach here, the empty string reads back
        // as an unknown action, which does nothing.
        W::Unknown(CarriedValue(Value::String(stored))) => stored.clone(),
        W::Unknown(_) => String::new(),
    }
}

/// Parse the on-disk string shape back into an action — mirrors the HTTP
/// twin's `action_from_string`. A string this nest does not recognise — a
/// newer nest's action, or a malformed row — reads as
/// [`WireEmailFilterAction::Unknown`] carrying the raw string: the filter does
/// nothing, and saving it writes the same string back (`transport.md` § Schema
/// and forward-compat discipline, rule 3 — an unknown filter action does
/// nothing, never `Discard`).
fn action_from_string(s: &str) -> WireEmailFilterAction {
    use WireEmailFilterAction as W;
    let unknown = || W::Unknown(CarriedValue(Value::String(s.to_string())));
    match s {
        "allow" => W::Allow,
        "discard" => W::Discard,
        other => {
            if let Some(reason) = other.strip_prefix("reject:") {
                W::Reject {
                    reason: reason.to_string(),
                }
            } else if let Some(mailbox) = other.strip_prefix("fileinto:") {
                W::FileInto {
                    mailbox: mailbox.to_string(),
                }
            } else if let Some(address) = other.strip_prefix("forward:") {
                // `copy` until `action_from_storage` applies the column.
                W::Forward {
                    address: address.to_string(),
                    redirect: false,
                }
            } else if let Some(rest) = other.strip_prefix("autoreply:") {
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let interval = parts[0].parse().unwrap_or(24);
                    let (subject, body) = parts[1].split_once('\n').unwrap_or((parts[1], ""));
                    W::AutoReply {
                        subject: subject.to_string(),
                        body: body.to_string(),
                        interval_hours: interval,
                    }
                } else {
                    unknown()
                }
            } else if let Some(label) = other.strip_prefix("addlabel:") {
                W::AddLabel {
                    label: label.to_string(),
                }
            } else {
                unknown()
            }
        }
    }
}

/// The complete on-disk projection of an action: the `email_filters.action`
/// string plus the `forward_redirect` column (`true` only for a `redirect`
/// `Forward`; every other action stores `false`). The inverse is
/// [`action_from_storage`]; the two DB-facing handlers and `row_to_wire` go
/// through this pair, never the string half alone.
fn action_to_storage(action: &WireEmailFilterAction) -> (String, bool) {
    let redirect = matches!(
        action,
        WireEmailFilterAction::Forward { redirect: true, .. }
    );
    (action_to_string(action), redirect)
}

/// Inverse of [`action_to_storage`]: parse the string, then apply the column
/// to a `Forward`. The column is ignored for every other action, so a stray
/// `1` on a non-forward row (unrepresentable through the handlers) changes
/// nothing.
fn action_from_storage(s: &str, forward_redirect: bool) -> WireEmailFilterAction {
    let mut action = action_from_string(s);
    if let WireEmailFilterAction::Forward { redirect, .. } = &mut action {
        *redirect = forward_redirect;
    }
    action
}

/// What a succession does with one stored filter rule
/// (`succession-aftermath.md` § Re-key scope; the `email_filters`
/// `Succession::Partial` entry in `db::actor_tables`).
///
/// Ruled by **class, not by an enumeration of the actions someone noticed** —
/// [`filter_succession`] matches the action *exhaustively*, so a new
/// `EmailFilterAction` variant cannot be added without deciding whether it
/// survives a succession. A hand-written `action LIKE …` list is the shape
/// that let `account_aliases` sit un-ruled while it cost a security property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilterSuccession {
    /// Standing authority to make this nest **emit attacker-authored content
    /// outward under the successor's recovered identity** — armable with the
    /// account key a seed thief read, and effective with no user in the loop.
    /// Deleted, exactly like the bunker connections: re-creating one is then
    /// bounded and visible.
    ///
    /// Burning is also the fail-safe direction here, which is what separates
    /// this class from the one below: the nest emits *nothing*, and nothing
    /// about who may reach the successor changes.
    Burn,
    /// The rule's own purpose is legitimate **narrowing** that must survive —
    /// burning it would silently re-admit mail the user had arranged to refuse
    /// (the `inbox_modes` asymmetry inverted) — but it carries an
    /// attacker-authored text field that reaches third parties. The rule moves
    /// with that field replaced by the carried action string.
    MoveDisarmed(String),
    /// Local placement/labelling with no outward emission: the successor's own
    /// configuration, and it moves untouched.
    Move,
}

/// Classify one stored `email_filters.action` string for the succession
/// ceremony and its boot reconcile.
///
/// ⚠ Deliberately classifies the **parsed** action rather than the raw string,
/// because [`action_from_string`] is the same parse the MTA perimeter acts on
/// (both `fauna.email.filters.list` and `fauna.bridges.fetch_recipient_filters`
/// project through [`row_to_wire`]). So a row that cannot be parsed as an
/// autoreply cannot *send* one either, and the classification can never
/// disagree with the behavior it is ruling on.
pub(crate) fn filter_succession(stored_action: &str) -> FilterSuccession {
    use WireEmailFilterAction as W;
    match action_from_string(stored_action) {
        // `Forward` is standing exfiltration authority — a tap on every future
        // message: the MTA dispatches a fired rule's forward after local
        // delivery (`mail-forwarding.md` § Per-rule "forward to").
        W::Forward { .. } => FilterSuccession::Burn,
        // `AutoReply` is standing *outbound-content* authority and, unlike
        // `Forward`, it is **live**: the MTA composes and sends the stashed
        // reply after local delivery, `From:` the recipient's address and
        // DKIM-signed under their domain. Post-ceremony that identity is the
        // successor's, so a moved rule keeps sending the thief's subject+body
        // to exactly the population trying to reach the recovered person, with
        // no end date and nothing prompting anyone to look at the filter list.
        // Nothing irrecoverable dies: re-arming a vacation notice is one
        // re-typed text field.
        W::AutoReply { .. } => FilterSuccession::Burn,
        // An action this nest cannot read cannot be ruled out of the class
        // above, so it takes it: it does nothing here, and it must not move to
        // the successor and come back armed under a newer nest.
        W::Unknown(_) => FilterSuccession::Burn,
        // `Reject`'s reason is thief-authorable text that reaches the sender on
        // a `550`. Weaker than the burn class (bounded and sanitized), but the
        // remedy is not a burn: the rule is how the user refuses a
        // correspondent, so deleting it would *widen* reach. Clearing the
        // reason keeps the refusal and drops the words — safe because a blank
        // reason falls back to a generic message at the perimeter
        // (`sanitizeRejectReason`), and because the rewrite stays a `Reject`
        // rather than degrading to `Discard` the way an over-clever rewrite of
        // a `Forward` would.
        W::Reject { reason } if !reason.is_empty() => {
            FilterSuccession::MoveDisarmed(action_to_string(&W::Reject {
                reason: String::new(),
            }))
        }
        // No outward emission and no attacker-authored text reaching a third
        // party. `Discard` is deliberately here and not in `Burn`: a
        // thief-armed one destroys future mail, but burning it re-admits mail
        // the user chose to drop, its harm is prospective-only, and it is
        // visible in the successor's own filter list — so the list, not the
        // ceremony, is its remedy.
        W::Reject { .. } | W::Allow | W::Discard | W::FileInto { .. } | W::AddLabel { .. } => {
            FilterSuccession::Move
        }
    }
}

/// Project a recipient's stored filter rows to the wire `EmailFilter` shape, in
/// DB order (`priority ASC, id ASC`). Shared by the user-facing
/// `fauna.email.filters.list` and the MTA-perimeter
/// `fauna.bridges.fetch_recipient_filters` (`bridge_routing_handlers.rs`) so the
/// two surfaces return byte-identical filter shapes — one projection, no drift (#3).
pub(crate) async fn email_filters_for_actor(
    db: &crate::db::CacheDb,
    actor_id: &[u8; 32],
) -> anyhow::Result<Vec<WireEmailFilter>> {
    let rows = db.list_email_filters(actor_id).await?;
    Ok(rows.into_iter().map(row_to_wire).collect())
}

fn row_to_wire(row: EmailFilterRow) -> WireEmailFilter {
    // A `rules` blob that does not decode must never match. It reads as one
    // unknown condition carrying the raw bytes — an unknown condition never
    // matches, under `all` or `any` — and never as the empty list, which under
    // `all` matches every message (`transport.md` § Schema and forward-compat
    // discipline, rule 3).
    let rules: Vec<WireEmailFilterRule> = fauna_core::encoding::canonical_decode(&row.rules)
        .unwrap_or_else(|_| {
            vec![WireEmailFilterRule::Unknown(CarriedValue(Value::Bytes(
                row.rules.clone(),
            )))]
        });
    let action = action_from_storage(&row.action, row.forward_redirect);
    WireEmailFilter {
        id: row.id,
        name: row.name,
        rules,
        combination: row.combination,
        action,
        priority: row.priority,
        continue_on_match: row.continue_on_match,
        created_at: row.created_at,
        extra: Default::default(),
    }
}

/// Shared body validation for create + update. Returns the projected
/// `(rules_bytes, action_string, forward_redirect)` triple ready to hand to the DB layer,
/// or an `RpcError` shaped like the HTTP twin's 400 bad-request surface
/// for any of the field-level checks. Mutating both call sites through
/// one helper keeps the create / update shapes byte-identical at the
/// boundary.
fn validate_and_project(
    name: &str,
    rules: Vec<WireEmailFilterRule>,
    combination: &str,
    action: WireEmailFilterAction,
) -> Result<(Vec<u8>, String, bool), RpcError> {
    if name.is_empty() {
        return Err(invalid_params("name is required"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(invalid_params("name too long"));
    }
    if !matches!(combination, "all" | "any") {
        return Err(invalid_params("combination must be \"all\" or \"any\""));
    }
    if rules.len() > MAX_RULES {
        return Err(invalid_params("too many rules"));
    }
    for r in &rules {
        validate_wire_rule(r)?;
    }
    let rules_bytes = fauna_core::encoding::canonical_encode(&rules)
        .map_err(|e| invalid_params(&format!("invalid rules: {e}")))?;
    if rules_bytes.len() > MAX_RULES_BYTES {
        return Err(invalid_params("rules blob too large"));
    }
    validate_action(&action)?;
    let (action_str, forward_redirect) = action_to_storage(&action);
    Ok((rules_bytes, action_str, forward_redirect))
}

/// An unknown rule or action is stored only as the echo of one the filter
/// already holds — an app listing a filter it cannot fully read and saving it
/// back — never as a new one: `held` is the stored filter an update replaces,
/// `None` on create. The same rule the feed's carried rules follow
/// (`transport.md` § Schema and forward-compat discipline, rule 3: no build
/// writes an unknown value except to pass a carried one through).
fn refuse_unheld_unknowns(
    rules: &[WireEmailFilterRule],
    action: &WireEmailFilterAction,
    held: Option<&WireEmailFilter>,
) -> Result<(), RpcError> {
    for r in rules {
        if matches!(r, WireEmailFilterRule::Unknown(_))
            && !held.is_some_and(|h| h.rules.contains(r))
        {
            return Err(invalid_params(
                "rule: a condition this nest does not know is kept only as the filter already holds it",
            ));
        }
    }
    if matches!(action, WireEmailFilterAction::Unknown(_))
        && held.is_none_or(|h| &h.action != action)
    {
        return Err(invalid_params(
            "action: an action this nest does not know is kept only as the filter already holds it",
        ));
    }
    Ok(())
}

// ── fauna.email.filters.list ───────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.filters.list").await?;
            let _req: ListEmailFiltersRequest = decode(&payload).map_err(malformed)?;
            let filters = email_filters_for_actor(&state.db, &actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListEmailFiltersReply {
                filters,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.filters.create ─────────────────────────────────

fn create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.filters.create").await?;
            let req: CreateEmailFilterRequest = decode(&payload).map_err(malformed)?;
            refuse_unheld_unknowns(&req.rules, &req.action, None)?;
            let (rules_bytes, action_str, forward_redirect) =
                validate_and_project(&req.name, req.rules, &req.combination, req.action)?;
            let id = state
                .db
                .create_email_filter(
                    &actor_id,
                    &req.name,
                    &rules_bytes,
                    &req.combination,
                    &action_str,
                    req.priority,
                    req.continue_on_match,
                    forward_redirect,
                )
                .await
                .map_err(internal)?;
            encode_reply(&CreateEmailFilterReply {
                id,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.filters.get ────────────────────────────────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.filters.get").await?;
            let req: GetEmailFilterRequest = decode(&payload).map_err(malformed)?;
            // Mirror the HTTP twin's per-actor isolation: lookup by id,
            // then check the row's owner. Two-step is fine — the DB has
            // a single-id index and the owner check collapses both the
            // wrong-actor and unknown-id paths to the same not_found.
            let row = match state.db.get_email_filter(req.id).await.map_err(internal)? {
                Some(r) if r.owner.as_slice() == actor_id.as_slice() => r,
                _ => return Err(not_found(req.id)),
            };
            encode_reply(&GetEmailFilterReply {
                filter: row_to_wire(row),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.filters.update ─────────────────────────────────

fn update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.filters.update").await?;
            let req: UpdateEmailFilterRequest = decode(&payload).map_err(malformed)?;
            let carries_unknown = matches!(req.action, WireEmailFilterAction::Unknown(_))
                || req
                    .rules
                    .iter()
                    .any(|r| matches!(r, WireEmailFilterRule::Unknown(_)));
            if carries_unknown {
                let held = match state.db.get_email_filter(req.id).await.map_err(internal)? {
                    Some(r) if r.owner.as_slice() == actor_id.as_slice() => row_to_wire(r),
                    _ => return Err(not_found(req.id)),
                };
                refuse_unheld_unknowns(&req.rules, &req.action, Some(&held))?;
            }
            let (rules_bytes, action_str, forward_redirect) =
                validate_and_project(&req.name, req.rules, &req.combination, req.action)?;
            let ok = state
                .db
                .update_email_filter(
                    req.id,
                    &actor_id,
                    &req.name,
                    &rules_bytes,
                    &req.combination,
                    &action_str,
                    req.priority,
                    req.continue_on_match,
                    forward_redirect,
                )
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found(req.id));
            }
            encode_reply(&UpdateEmailFilterReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.filters.delete ─────────────────────────────────

fn delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.filters.delete").await?;
            let req: DeleteEmailFilterRequest = decode(&payload).map_err(malformed)?;
            let deleted = state
                .db
                .delete_email_filter(req.id, &actor_id)
                .await
                .map_err(internal)?;
            if !deleted {
                return Err(not_found(req.id));
            }
            encode_reply(&DeleteEmailFilterReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.send ───────────────────────────────────────────
//
// Outbound submission from a user client. Reuses
// `fauna_mail::routing` for recipient parsing/partition,
// seals in-domain recipients through the shared sealed-ingest core
// (`deliver_in_domain` below), and enqueues out-of-domain recipients
// onto `outbound_mail_queue`.

/// One in-domain recipient the resolver accepted for local delivery: the owning
/// actor plus the per-recipient delivery stamps that must ride inside its sealed
/// copy. `local_part` is kept for the error/refusal messages the sender sees.
pub(crate) struct ResolvedLocalRcpt {
    pub local_part: String,
    pub target: [u8; 32],
    pub stamped_headers: Vec<(String, String)>,
}

/// Resolve one in-domain recipient through the **full fixed-order alias
/// resolver** — the same one the MTA's `RCPT TO` and the
/// `enqueue_outbound_mail` partition use. `Ok(None)` means the resolver refused
/// a local copy (rate-capped / disabled / expired) or matched an admin external
/// forwarder; per `mail-aliases.md` § Per-alias rate-cap (*Second consumer*)
/// such a recipient drops out of local delivery and onto the outbound queue,
/// where the MX perimeter re-decides with the same resolver.
///
/// ⚠ **This used to be `lookup_exact_alias`, and that was a bug** (fixed 2026-08-18): the local part it is handed is UNSTRIPPED
/// (`fauna_mail::routing::partition_recipients` splits on `@` and nothing else)
/// and the lookup matched `kind = 'exact'` only — so a local `+suffix`,
/// wildcard, catch-all or disposable recipient never resolved and the send
/// silently delivered to nobody, while the identical address worked over SMTP
/// submission and inbound MX. Sharing the resolver with those two paths is what
/// makes the three agree, and it brings the resolver's side effects (per-alias
/// rate cap, disposable decrement, `alias_hits`) with it — which is correct: the
/// Go submission path already applies them through the same handler.
///
/// Called ONCE per recipient, ahead of the guardian gate, so the gate sees
/// exactly what delivery will reach and no address is resolved twice.
async fn resolve_in_domain(
    state: &Arc<AppState>,
    domain: &str,
    local_part: &str,
    sender_domain: &str,
) -> Result<Option<ResolvedLocalRcpt>, RpcError> {
    use crate::bridge_routing_handlers::LocalRecipientOutcome;
    match crate::bridge_routing_handlers::resolve_local_recipient(
        state,
        domain,
        local_part,
        sender_domain,
    )
    .await?
    {
        LocalRecipientOutcome::Mailbox {
            actor_id,
            stamped_headers,
            ..
        } => Ok(Some(ResolvedLocalRcpt {
            local_part: local_part.to_string(),
            target: actor_id,
            stamped_headers,
        })),
        LocalRecipientOutcome::Forward { .. } | LocalRecipientOutcome::Reject { .. } => Ok(None),
    }
}

/// Deliver an already-resolved in-domain recipient via the shared
/// [`seal_and_ingest_local`](crate::bridge_routing_handlers::seal_and_ingest_local)
/// core: the message is sealed to the recipient's MSEK-derived pubkey, with the
/// resolver's stamps prepended first, and lands in their `__mail` segment store +
/// `bridge_imap_messages` INBOX, readable via `fauna.email.inbox.fetch`/IMAP —
/// the same path the Go MTA's local-recipient delivery takes. nest seals because
/// `fauna.email.send` carries plaintext `raw_rfc5322` (the same trust model as a
/// Thunderbird→MTA submission). A recipient with no MLS pubkey on file fails for
/// that one recipient (never a silent drop, never the whole send).
async fn deliver_resolved_in_domain(
    state: &Arc<AppState>,
    rcpt: &ResolvedLocalRcpt,
    sender_domain: &str,
    sender_address: &str,
    verified_from: Option<&str>,
    raw_rfc5322: &[u8],
) -> Result<(), RpcError> {
    // The authenticated-sender stamp (`smtp-server.md` § Architectural rules →
    // *The `X-Fauna-*` namespace*): this door vouches only for the `From:` the
    // handle gate verified — `None` on the off-domain bypass, so that copy
    // carries no stamp and every consumer of it reads "unauthenticated". The
    // client-supplied copies were stripped before the gate; this one is ours.
    let mut stamped_headers = rcpt.stamped_headers.clone();
    if let Some(stamp) = verified_from.and_then(fauna_mail::sender_auth::authenticated_sender_stamp)
    {
        stamped_headers.push(stamp);
    }
    crate::bridge_routing_handlers::seal_and_ingest_local(
        state,
        &rcpt.target,
        raw_rfc5322,
        sender_domain,
        fauna_core::data::MailIngress::Sender(sender_address),
        &stamped_headers,
    )
    .await
}

/// The **in-domain twin** of the `RCPT TO` reject (`family-safety.md` § The mail
/// gate): a Fauna user mailing a ward on the same nest never touches SMTP, so
/// there is no per-recipient stage at which to refuse — the *sender* is refused
/// instead, with the same typed error the outbound contact-reach gate uses.
///
/// Run as a **pre-flight over every in-domain recipient, before a single one is
/// delivered.** Unlike SMTP there is no third-party retry storm to worry about
/// (the sender is a live authenticated caller who will see the error and can
/// re-send to the others), so refusing the whole send while naming the refused
/// recipient is the honest outcome. Delivering to the co-recipients and quietly
/// dropping the ward's copy would leave the sender believing the ward received
/// it — a silent partial delivery, which this must never become.
///
/// Takes recipients **already resolved** by the delivery pass rather than
/// resolving them itself (2026-08-18). It used to do its own
/// `lookup_exact_alias`, which was exact-only: a ward reachable at
/// `ward+anything@` was silently un-gated, and once the delivery loop below
/// learned to resolve sub-addresses that would have become a live bypass.
/// Resolving once and gating the result keeps the gate exactly as wide as the
/// delivery it guards — and applies the resolver's side effects (rate cap,
/// disposable decrement, `alias_hits`) once rather than twice, which a second
/// resolve here would have done.
///
/// A recipient whose alias does not resolve never reaches this list, and is
/// handled by the delivery pass's own per-recipient error handling.
async fn refuse_if_guardian_rejects_in_domain(
    state: &Arc<AppState>,
    domain: &str,
    resolved: &[ResolvedLocalRcpt],
    sender_address: &str,
) -> Result<(), RpcError> {
    for rcpt in resolved {
        let verdict = crate::bridge_routing_handlers::guardian_mail_verdict(
            state,
            &rcpt.target,
            fauna_core::data::MailIngress::Sender(sender_address),
        )
        .await?
        .verdict;
        if verdict == fauna_core::data::MailVerdict::Reject {
            return Err(
                crate::bridge_routing_handlers::mail_guardian_approval_required(&format!(
                    "{}@{domain}",
                    rcpt.local_part
                )),
            );
        }
    }
    Ok(())
}

fn send_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.send").await?;
            let req: SendEmailRequest = decode(&payload).map_err(malformed)?;

            // Product-ceiling pre-check — the typed first-party sibling of the
            // SMTP perimeter's `552 5.3.4` (`smtp-server.md` § Message size
            // limits): a raw message over `effective_max_raw_message_bytes` is
            // refused with a typed `message_too_large` so a client renders "too
            // large" rather than a raw transport failure. Authoritative nest-side
            // (the client is untrusted); the inline-ceiling refusal that keeps an
            // over-frame send from severing the WS connection is a distinct
            // client-side pre-check (a clients-area follow-on). Never a hardcoded
            // const — the ceiling-retirement slice re-derives this ceiling and the
            // check tracks it.
            let max_raw = fauna_mail::transport_limits::effective_max_raw_message_bytes(
                state
                    .db
                    .get_spam_policy()
                    .await
                    .map_err(internal)?
                    .effective()
                    .max_message_bytes,
            );
            if req.raw_rfc5322.len() as u64 > max_raw as u64 {
                return Err(message_too_large(req.raw_rfc5322.len(), max_raw as u64));
            }

            // Every door that files bytes the nest did not compose removes the
            // reserved `X-Fauna-*` delivery stamps first (`smtp-server.md`
            // § Architectural rules → *The `X-Fauna-*` namespace*): the in-domain
            // copies and the Sent copy below seal from these bytes, and the
            // genuine threshold stamp is prepended at delivery — a client-supplied
            // one must not ride beneath it into a copy the MDA scorer trusts.
            // Sized above as sent; stripped once here so every reader below (the
            // From count, the parses, the seals, the outbound enqueue) sees one
            // form. `X-Fauna-Forwarded-By` survives, as at every door.
            let raw_rfc5322 = fauna_mail::received_header::strip_fauna_headers(&req.raw_rfc5322);

            // RFC 5322 §3.6: exactly one From field, before the handle gate
            // reads it (`smtp-server.md` § Architectural rules → *Exactly one
            // From field*). The gate below reads the LAST From field through
            // `mail-parser`, while a receiver's DMARC may align against the
            // first — a second field would pass the gate as the sender's own
            // address and leave carrying another.
            let from_fields = fauna_mail::from_field::from_field_count(&raw_rfc5322);
            if from_fields != 1 {
                return Err(invalid_params(&format!(
                    "message must carry exactly one From header field, found {from_fields}"
                )));
            }
            // …and that one field names exactly one mailbox
            // (`mail-multidomain.md` § From: header ownership → *Exactly one
            // mailbox*): two would sign under the first's domain with a
            // foreign one riding along; none (a display name, a domain-less
            // token) leaves nothing to own and nothing for DKIM to key on.
            // The list is the shared-Rust reader the submission door uses,
            // so the two doors see the same address in the same bytes.
            let mut from_mailboxes = fauna_mail::envelope::from_mailboxes(&raw_rfc5322);
            if from_mailboxes.len() != 1 {
                return Err(invalid_params(&format!(
                    "From field must name exactly one mailbox, found {}",
                    from_mailboxes.len()
                )));
            }
            let from_mailbox = from_mailboxes.remove(0);
            // The From: addr-spec every reader below sees — the in-domain
            // sender metadata, the ownership gate, the rate limit's
            // subordinate key and the outbound envelope sender — as
            // `local@domain`, the shape `mail-parser`'s `address()` yields.
            let from_addr = format!("{}@{}", from_mailbox.mailbox, from_mailbox.host);

            // Recipients: merge into a single deduplicated list. The
            // wire shape doesn't carry a separate `to` (the HTTP twin
            // kept one for legacy bridge compat); pass an empty `to`
            // so the merge routine is a pure dedup over `recipients`.
            let all_recipients = fauna_mail::routing::merge_recipients("", &req.recipients);
            if all_recipients.is_empty() {
                return Err(invalid_params("no recipients"));
            }

            // The message's own RFC 5322 Message-ID. Parsed once here, used
            // twice below: the guardian mail gate's sent-Message-ID seed and
            // the outbound queue (both only when the send has a remote
            // recipient).
            let msgid = mail_parser::MessageParser::default()
                .parse(&raw_rfc5322)
                .as_ref()
                .and_then(|p| p.message_id())
                .unwrap_or("")
                .to_string();

            // Guardian mail gate — outbound auto-seed, path A of two
            // (`family-safety.md` § Wire & data shape). This is the *native*
            // client's path (Conversations sends `fauna.email.send`),
            // authenticated as the ward itself; an external MUA takes path B via
            // SMTP submission and is seeded in `enqueue_outbound_mail_handler`.
            //
            // Two facts, two uses. The recipient ADDRESSES become known senders,
            // so replies to the child's own mail always flow back in — seed the
            // FULL recipient set, before the in-domain/remote partition below,
            // since same-nest recipients never enter the outbound queue. The
            // message's MESSAGE-ID becomes the correlation a remote MTA's bounce
            // of *this* message must name — and consume — to be delivered rather
            // than held; that seed needs the partition first, because only a
            // message with a remote recipient can ever be bounced, and the
            // remote count sizes its correlation budget (§ The mail gate).
            //
            // Both are no-ops for an unsupervised sender, and best-effort: a seed
            // failure must never fail the user's send.
            for rcpt in &all_recipients {
                if let Err(e) = state
                    .db
                    .add_mail_allowlist_entry(&actor_id, rcpt, "outbound")
                    .await
                {
                    tracing::warn!("guardian mail allowlist seed failed: {e}");
                }
            }
            // Split against the runtime PRIMARY mail domain (the `local_domains`
            // table). The legacy boot-time `state.email.domain` this used to fall
            // back to is **removed**; keying on it (always `None` on the Go-MTA
            // build) had left in-domain (Fauna→Fauna) delivery dead in the shipped
            // binary. See `bridge_routing_handlers::primary_mail_domain`.
            let primary_domain =
                crate::bridge_routing_handlers::primary_mail_domain(&state).await?;
            let (local_parts, mut remote_addrs) = fauna_mail::routing::partition_recipients(
                &all_recipients,
                primary_domain.as_deref(),
            )
            .map_err(|addr| invalid_params(&format!("invalid address: {addr}")))?;
            if let Err(e) = state
                .db
                .add_sent_msgid(&actor_id, &msgid, remote_addrs.len())
                .await
            {
                tracing::warn!("guardian sent-msgid seed failed: {e}");
            }

            // The From: address's two halves, for the readers below (the
            // at-rest sender-domain metadata and the ownership gate). Both are
            // non-empty by construction: `from_mailboxes` above admits only a
            // mailbox carrying a local part AND a domain, so there is no
            // `@`-less shape left that could slip past a split-on-`@` gate.
            let (from_local, from_domain) =
                (from_mailbox.mailbox.as_str(), from_mailbox.host.as_str());
            // The From: domain, reused for both the in-domain delivery metadata
            // and the sender's own Sent-copy metadata below. Never empty
            // (`persist_decoded_inbound_mail` rejects an empty `sender_domain`).
            let sender_domain = from_domain.to_string();

            // Sender-ownership verification — the app door's arm of the one
            // rule both sender doors enforce (`mail-multidomain.md` § *From:
            // header ownership*; this door's statement: `mail-app-surface.md`
            // § *Sender-handle verification*). The deployment claims authority
            // over EVERY active local domain, not just the primary: a handle
            // is "addressable and login-reachable as `bob@<domain>` for every
            // active `local_domains` entry" (`mail-multidomain.md` § *The
            // model: one handle, addressable on every active domain*), so
            // `<handle>@<any active domain>` IS that actor's own address, and
            // so is any alias the alias resolver attributes to the actor.
            // Comparing against the primary alone once left every additional
            // domain a free impersonation surface — and because this same
            // From: address becomes the outbound envelope sender
            // (`original_sender` below) and DKIM signs on the From: domain,
            // the forgery went out DMARC-aligned. Off-domain From: addresses
            // still bypass (the deployment isn't authoritative for them).
            let deployment_claims_from_domain = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .iter()
                .any(|d| d.domain_name.eq_ignore_ascii_case(from_domain));
            if deployment_claims_from_domain {
                // `get_handle` returns `Ok(None)` only when there is no `users`
                // row at all; a REGISTERED actor without a handle stores the
                // empty string (`create_user` writes `''`, and
                // `fauna.admin.users.clear_handle` sets it back to `''`). So an
                // empty handle is folded into the no-handle arm — left in the
                // mismatch arm it produced the nonsense refusal "from address
                // does not match your handle ()", and the honest no-handle
                // message was unreachable for every actor that could actually
                // get this far.
                let handle = state
                    .db
                    .get_handle(&actor_id)
                    .await
                    .map_err(|e| internal(format!("get_handle: {e}")))?
                    .filter(|h| !h.is_empty());
                let is_own_handle = handle
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(from_local));
                if !is_own_handle {
                    // Not the handle: an alias the actor owns is theirs to send
                    // from too (the MUA-facing door accepts it as `MAIL FROM`
                    // and `From:` alike — an app must not be held to less).
                    // The SAME in-process resolver the MTA reaches over
                    // `fauna.bridges.resolve_recipient`, and the same
                    // reading of its answer: only a deliverable mailbox owned
                    // by THIS actor authorises; Forward (an admin external
                    // forwarder — no sending mailbox), Reject, or another
                    // owner are all "not yours".
                    let owned = matches!(
                        crate::bridge_routing_handlers::resolve_local_recipient_unstamped(
                            &state,
                            from_domain,
                            from_local,
                            primary_domain.as_deref().unwrap_or(from_domain),
                        )
                        .await?,
                        crate::bridge_routing_handlers::LocalRecipientOutcome::Mailbox {
                            actor_id: owner,
                            ..
                        } if owner == actor_id
                    );
                    if !owned {
                        return Err(match handle.as_deref() {
                            Some(handle) => permission_denied(&format!(
                                "from address does not match your handle ({handle}) or an alias you own"
                            )),
                            None => permission_denied("you must set a handle before sending email"),
                        });
                    }
                }
            }
            // The address this door verified, for the in-domain copies'
            // authenticated-sender stamp: the `From:` iff the handle gate above
            // fired and passed; the off-domain bypass verified nothing.
            let verified_from = deployment_claims_from_domain.then_some(from_addr.as_str());

            // Outbound rate limit. Only fires when there is remote delivery to
            // do — pure in-domain sends never leave the deployment, so they
            // don't consume an outbound-reputation allowance.
            //
            // TWO keys, and the ORDER of the two matters less than the fact
            // that the first one exists:
            //
            //  1. Per **actor** — the authenticated caller. This is the
            //     ceiling that binds. Before the 2026-08-23 change the only key
            //     here was `original_sender`, i.e. the caller's own `From:`
            //     header, and an off-domain `From:` deliberately bypasses the
            //     handle gate above ("the deployment isn't authoritative for
            //     them", `mail-app-surface.md` § Sender-handle verification).
            //     So the caller could put any string there and a fresh string
            //     each message minted a fresh counter each message: the cap
            //     was defeated by editing a header, and the account it exists
            //     to bound was never named in the query. The actor id is the
            //     one value on this path the caller cannot choose.
            //
            //  2. Per **sender address** — the original `original_sender`
            //     count, KEPT as a subordinate cap rather than deleted. It is
            //     not redundant: (1) bounds one account, (2) bounds one
            //     address across accounts, and those come apart exactly where
            //     the handle gate does not reach — N distinct actors may each
            //     spend their own hourly allowance forging the *same*
            //     off-domain `From:`, which (1) alone permits and (2) catches.
            if !remote_addrs.is_empty() {
                let by_actor = state
                    .db
                    .count_outbound_by_actor_window(&actor_id, SEND_RATE_WINDOW_SECS)
                    .await
                    .map_err(internal)?;
                let by_sender = state
                    .db
                    .count_outbound_by_sender_window(&from_addr, SEND_RATE_WINDOW_SECS)
                    .await
                    .map_err(internal)?;
                if by_actor >= SEND_RATE_LIMIT_PER_HOUR || by_sender >= SEND_RATE_LIMIT_PER_HOUR {
                    let mut e =
                        RpcError::new("fauna.email.rate_limited", "error.email.rate_limited");
                    e.details = Some(Box::new(Value::String(format!(
                        "outbound rate limit exceeded ({SEND_RATE_LIMIT_PER_HOUR}/hr)",
                    ))));
                    return Err(e);
                }

                // The per-actor recipients/day quota — the SAME nest-side
                // counter authenticated raw-SMTP submission consumes
                // (`smtp-server.md` § Architectural rules), so a user cannot
                // dual-stream between this RPC and port 465/587 to escape it.
                // `mail-mass-mailing.md` § How the per-list cap separates from
                // per-actor states this as the rule already: "Regular
                // one-to-one mail (`fauna.email.send` / authenticated raw-SMTP
                // submission) consumes the per-actor counters" — until the
                // 2026-08-23 change this half of that sentence was not true of
                // the RPC.
                //
                // Charged on the REMOTE recipients only, matching the ceiling
                // above and the counter's own subject (what leaves the
                // deployment). `.effective()` resolves an unset admin override
                // to the wire-catalog default, the same overlay
                // `check_submission_quota_handler` reads.
                let effective = state
                    .db
                    .get_submission_policy()
                    .await
                    .map_err(internal)?
                    .effective();
                let day_bucket = crate::db::now_epoch_secs() / 86_400;
                let outcome = state
                    .db
                    .try_consume_submission_quota(
                        &actor_id,
                        day_bucket,
                        remote_addrs.len() as u32,
                        effective.max_per_day,
                    )
                    .await
                    .map_err(internal)?;
                if let crate::db::bridge_routing::SubmissionQuotaOutcome::OverQuota { remaining } =
                    outcome
                {
                    let mut e =
                        RpcError::new("fauna.email.rate_limited", "error.email.rate_limited");
                    e.details = Some(Box::new(Value::String(format!(
                        "daily submission quota exceeded ({} recipients/day, {remaining} remaining)",
                        effective.max_per_day,
                    ))));
                    return Err(e);
                }
            }

            let mut local_delivered: u32 = 0;
            let mut remote_queued: u32 = 0;
            let mut remote_errors: Vec<String> = Vec::new();

            if !local_parts.is_empty() {
                // In-domain (Fauna→Fauna) delivery: seal the sender's
                // plaintext to each local recipient's MSEK-derived pubkey and
                // route it through the SAME sealed-ingest core the Go MTA's
                // local-recipient path uses (`__mail/<actor>` segment store +
                // `bridge_imap_messages` INBOX), so in-domain mail is visible
                // to `fauna.email.inbox.fetch`/IMAP. Resolution is the
                // production `account_aliases` exact-match resolver — NOT the
                // deprecated `deliver_local`/legacy-inbox/`email_aliases`
                // path. (mail-app-surface.md § First-party client send owns
                // this handler; smtp-server.md § Recipient handling on
                // submission owns the bridge's RCPT-time twin.)
                //
                // `local_parts` is non-empty only when a primary mail domain is
                // registered (partition_recipients routes everything remote
                // otherwise), so the domain is always present here. The seal,
                // index-hint tokenization, and ingest all live in the shared
                // `seal_and_ingest_local` core (priority #2).
                let domain = primary_domain.clone().unwrap_or_default();
                // Resolve every in-domain recipient FIRST, through the full
                // fixed-order resolver, so the guardian gate below
                // sees exactly the recipients delivery will reach and each
                // address is resolved — side effects included — exactly once.
                let mut resolved: Vec<ResolvedLocalRcpt> = Vec::new();
                for part in &local_parts {
                    match resolve_in_domain(&state, &domain, part, &sender_domain).await {
                        Ok(Some(r)) => resolved.push(r),
                        // Any resolver `Reject` — unknown address, disabled,
                        // expired, invalid sub-address, over its rate cap — or
                        // an admin external forwarder: onto the outbound queue,
                        // where our own MX perimeter re-decides with this same
                        // resolver and emits the bounce. Deliberately uniform
                        // across all five reject reasons and shared with
                        // `submit_outbound`'s in-domain partition; the two move
                        // together or not at all (mail-aliases.md § Per-alias
                        // rate-cap -> *Second consumer, deliberately left
                        // uniform*).
                        Ok(None) => remote_addrs.push(format!("{part}@{domain}")),
                        Err(e) => {
                            tracing::warn!(
                                recipient = %format!("{part}@{domain}"),
                                error = %e.code,
                                "in-domain recipient did not resolve"
                            );
                            remote_errors.push(format!("local: {part}: {}", e.code));
                        }
                    }
                }
                // Guardian mail gate, `reject` arm — refuse the whole send
                // before delivering to anyone (see the fn doc).
                refuse_if_guardian_rejects_in_domain(&state, &domain, &resolved, &from_addr)
                    .await?;
                for rcpt in &resolved {
                    match deliver_resolved_in_domain(
                        &state,
                        rcpt,
                        &sender_domain,
                        &from_addr,
                        verified_from,
                        &raw_rfc5322,
                    )
                    .await
                    {
                        Ok(()) => local_delivered += 1,
                        Err(e) => {
                            tracing::warn!(
                                recipient = %format!("{}@{domain}", rcpt.local_part),
                                error = %e.code,
                                "in-domain local delivery failed"
                            );
                            remote_errors.push(format!("local: {}: {}", rcpt.local_part, e.code));
                        }
                    }
                }
            }

            if !remote_addrs.is_empty() {
                let rcpt_refs: Vec<&str> = remote_addrs.iter().map(String::as_str).collect();
                match state
                    .db
                    .enqueue_outbound(crate::db::outbound::NewOutbound {
                        original_msgid: &msgid,
                        original_sender: &from_addr,
                        recipients: &rcpt_refs,
                        raw_message: &raw_rfc5322,
                        inbound_verdicts: crate::db::outbound::InboundVerdictsSnapshot {
                            spf: String::new(),
                            dmarc: String::new(),
                            dmarc_policy: String::new(),
                        },
                        is_forwarded: false,
                        forward_actor_id: None,
                        forward_rule_id: None,
                        forward_copy_mode: None,
                        // The authenticated caller, so the per-hour ceiling
                        // above can count this row against the account that
                        // actually sent it rather than the `From:` string it
                        // chose.
                        submit_actor_id: Some(&actor_id),
                    })
                    .await
                {
                    Ok(ids) => remote_queued = ids.len() as u32,
                    Err(e) => {
                        tracing::error!("enqueue_outbound error: {e}");
                        remote_errors.push(e.to_string());
                    }
                }
            }

            // Nudge the MTA bridge to drain promptly instead of waiting for
            // its next fetch_outbound_due poll (the outbound twin of the
            // inbound mail.received push). Best-effort; the poll is the
            // backstop. Only on a genuinely-non-empty remote enqueue.
            if remote_queued > 0 {
                crate::bridge_routing_handlers::notify_bridges_outbound_ready(&state).await;
            }

            // Durable server-side Sent copy. Seal the sender-supplied plaintext
            // to the SENDER's own MSEK-derived read key and store it in their
            // `Sent` mailbox via the shared own-submission ingest path, so mail
            // composed in a Fauna app survives a client restart and appears on
            // every device — it reloads via `fauna.email.sent.fetch` (the
            // in-memory conversation store re-fetches from uid 0 each launch).
            // This mirrors the server-side Sent copy an external-MUA submission
            // already leaves (smtp-server.md § Inbound client receive). The
            // native app also shows a local echo on send; the Sent-feed
            // re-ingest dedups against it by Message-ID
            // (`fauna_conversations::ConversationsManager::ingest_inbound`), so the
            // in-session view shows one copy and a restart reloads exactly one.
            //
            // Best-effort: the message has already been delivered/queued above, so
            // a Sent-copy failure (e.g. a sender with no MLS pubkey on file) is
            // logged, never a send failure — failing here could misleadingly
            // trigger a client retry and double-send to remote MX.
            if let Err(e) = crate::bridge_routing_handlers::seal_and_store_sent_copy(
                &state,
                &actor_id,
                &raw_rfc5322,
                &sender_domain,
            )
            .await
            {
                tracing::warn!(
                    error = %e.code,
                    "storing server-side Sent copy for fauna.email.send failed"
                );
            }

            encode_reply(&SendEmailReply {
                local_delivered,
                remote_queued,
                remote_errors,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.{inbox,sent}.fetch ──────────────────────────────
//
// A User-class, **caller-scoped** read of one of the calling actor's own
// standard mailboxes. Two kinds share this body:
//
//   * `fauna.email.inbox.fetch` — the inbound twin of `fauna.email.send`
//     (the client mail-receive feed, Slice 1).
//   * `fauna.email.sent.fetch` — the **Sent** sibling: a message the user
//     sent from an external MUA (e.g. macOS Mail via SMTP submission) leaves
//     a server-side `Sent` copy sealed to the sender's own MSEK-derived
//     recipient key, so the native app can surface its outbound mail in
//     the unified conversations view.
//
// Both read the **new Go-mail-bridge path** only: placement from
// `bridge_imap_messages` (`query_bridge_imap_messages`) + bodies from the
// `__mail/<actor>` segment store (`read_envelopes_bulk`). NOT the deprecated
// `inbox`/`email_aliases` stack.
//
// Caller-scoping is by construction: there is no `actor_id` request field,
// so the reading actor is always the authenticated caller. Modelled on
// `fetch_message_ciphertext_handler` (the canonical new-path reader) and
// `list_messages_handler`'s `limit+1` page-detection sentinel; the
// returned `sealed_envelope` is exactly the canonical `MailRecordEnvelope`
// bytes `read_envelopes_bulk` returns (the client opens it client-side —
// the nest holds no opening key). Unlike `list_messages_handler` this does
// NOT emit bootstrap placement records: that is MDA-side placement-journal
// maintenance, irrelevant to a read.

/// Page-size cap for the mailbox-fetch feeds. Mail bodies are large, so a
/// page is far smaller than `channel.fetch`'s 500. A request `limit` of 0
/// selects `MAIL_FETCH_DEFAULT_LIMIT`; anything above the cap is clamped down.
const MAIL_FETCH_DEFAULT_LIMIT: u32 = 50;
const MAIL_FETCH_MAX_LIMIT: u32 = 50;

/// Build a caller-scoped read handler for a single standard `mailbox`
/// (`"INBOX"` / `"Sent"`), gated on `permission`. The body is mailbox-agnostic:
/// `InboxFetchRequest` / `InboxFetchReply` / `InboxMessage` are the **generic
/// mail-page wire types** now serving both INBOX and Sent (a rename to
/// `MailFetch*` is a deliberate future cleanup, out of scope here). The only
/// per-kind differences are the literal mailbox name and the permission string.
fn mailbox_fetch_handler(mailbox: &'static str, permission: &'static str) -> RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, permission).await?;
            let req: InboxFetchRequest = decode(&payload).map_err(malformed)?;

            // Refuse on a pure-backup destination (no local plaintext-framed
            // segments to read) — same gate the BridgeMda IMAP read path uses.
            crate::bridge_routing_handlers::refuse_if_pure_backup(
                &state,
                "mail",
                &actor_id,
                crate::bridge_routing_handlers::pure_backup_destination,
            )
            .await?;

            // Idempotent — seed the caller's own standard mailboxes so a
            // never-received-mail actor reads an empty mailbox rather than
            // erroring. No bootstrap placement records (read path).
            state
                .db
                .ensure_bridge_imap_mailboxes(&actor_id)
                .await
                .map_err(internal)?;

            // The mailbox HIGHESTMODSEQ — the client's `flag_changes` baseline
            // (`mail-app-surface.md` § Read state). Read BEFORE the rows, so a
            // flag write racing this page carries a higher modseq and is
            // delivered again by the delta rather than missed.
            let highest_modseq = state
                .db
                .max_highestmodseq_for_actor(&actor_id, Some(mailbox))
                .await
                .map_err(internal)?
                .max(0) as u64;

            // Clamp the page size: 0 → default, oversize → cap.
            let limit = match req.limit {
                0 => MAIL_FETCH_DEFAULT_LIMIT,
                n => n.min(MAIL_FETCH_MAX_LIMIT),
            };

            // Fetch one extra row to detect whether more pages remain
            // (the `list_messages_handler` sentinel trick). `after_uid == 0`
            // means "from the beginning" (no UID floor).
            let after = (req.after_uid != 0).then_some(req.after_uid);
            let mut rows = state
                .db
                .query_bridge_imap_messages(&actor_id, mailbox, None, after, None, Some(limit + 1))
                .await
                .map_err(internal)?;
            let mut more = if rows.len() > limit as usize {
                rows.truncate(limit as usize);
                true
            } else {
                false
            };

            // Bulk-read the sealed segment envelopes for this page, in
            // row order. `read_envelopes_bulk` returns the verbatim
            // canonical `MailRecordEnvelope` bytes the client opens.
            let id_slices: Vec<&[u8]> = rows.iter().map(|r| &r.message_id[..]).collect();
            let envelopes = crate::segments::mail::read_envelopes_bulk(
                &state.mail_segments,
                &state.db,
                &actor_id,
                &id_slices,
            )
            .await
            .map_err(internal)?;

            // Zip placement rows + envelopes. A `None` envelope is a
            // segment_records mirror divergence (the body row vanished
            // under a placement row) — skip it with a warn, mirroring
            // `read_envelopes_bulk`'s own divergence handling, rather than
            // shipping a placement row with no body.
            //
            // Frame budget: the reply must fit one 2 MiB WS-RPC frame, and the
            // perimeter admits messages whose stored envelope alone exceeds it.
            // Two mechanisms keep every page inside the frame (smtp-server.md
            // § Message size limits, the client-feed reference leg): a single
            // over-budget envelope crosses by REFERENCE (its bytes are staged on
            // the byte plane and the message carries a small `body_ref` instead of
            // the inline envelope), and a page is closed early with `more = true`
            // once the included messages' wire cost fills the budget — the client
            // resumes from the last included uid.
            let frame_budget =
                fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize;
            let mut page_bytes: usize = 0;
            let mut messages: Vec<InboxMessage> = Vec::with_capacity(rows.len());
            for (row, env) in rows.into_iter().zip(envelopes) {
                match env {
                    Some(sealed_envelope) => {
                        // A v3 continuation HEAD carries no inline body, so the
                        // client cannot open it. Materialize it here — concatenate
                        // its parts and re-wrap as an inline envelope
                        // `open_inbound_record` reads normally (the feed's
                        // "concatenate parts before the split",
                        // message-segment-store.md § Continuation records). An
                        // inline (v1/v2) record ships verbatim, unchanged. A join
                        // failure (a part still relaying / crash mid-write) skips
                        // the message with a warn, like a mirror divergence —
                        // never fails the whole page.
                        let shippable =
                            match fauna_mail::segments::peek_format_version(&sealed_envelope) {
                                Ok(fauna_mail::segments::MAIL_ENVELOPE_FORMAT_VERSION_V3) => {
                                    match crate::segments::mail::read_sealed_body_with_floor(
                                        &state.mail_segments,
                                        &state.db,
                                        &actor_id,
                                        &row.message_id,
                                    )
                                    .await
                                    {
                                        Ok(Some((body, hint, _floor))) => {
                                            match fauna_mail::segments::MailRecordEnvelope::new(
                                                body, hint,
                                            )
                                            .encode()
                                            {
                                                Ok(bytes) => bytes,
                                                Err(e) => {
                                                    tracing::warn!(
                                                        actor = ?actor_id, uid = row.uid,
                                                        "mailbox.fetch: re-encode of a joined \
                                                         continuation body failed — skipping: {e}"
                                                    );
                                                    continue;
                                                }
                                            }
                                        }
                                        Ok(None) => continue,
                                        Err(e) => {
                                            tracing::warn!(
                                                actor = ?actor_id, uid = row.uid,
                                                "mailbox.fetch: joining a continuation head failed \
                                                 (part still relaying / mid-write) — skipping \
                                                 (readable over IMAP): {e}"
                                            );
                                            continue;
                                        }
                                    }
                                }
                                Ok(_) => sealed_envelope,
                                Err(e) => {
                                    tracing::warn!(
                                        actor = ?actor_id, uid = row.uid,
                                        "mailbox.fetch: unparseable stored envelope version — \
                                         skipping: {e}"
                                    );
                                    continue;
                                }
                            };
                        // A stored envelope that alone exceeds the frame budget can
                        // never ride inline in any page, so it crosses by REFERENCE
                        // (the client-feed leg; the twin of
                        // `fetch_message_ciphertext`'s inline-or-`body_ref` split,
                        // smtp-server.md § Message size limits). Stage the whole
                        // outer envelope on the byte plane and ship a small
                        // `body_ref`; the client GETs the chunks over the open
                        // download route, joins them, and opens the result exactly
                        // as an inline `sealed_envelope`.
                        //
                        // A body at or under the frame always rides inline, encoded
                        // byte-identically to the pre-reference shape.
                        let over_frame = shippable.len() > frame_budget;
                        let (sealed_envelope, body_ref, wire_cost) = if over_frame {
                            match crate::mail_body_plane::stage_sealed_body(&state, &shippable)
                                .await
                            {
                                Ok(r) => {
                                    // On the wire the message is now just the
                                    // reference (ordered 32-byte hashes + total).
                                    // Charge THAT against the page budget — not the
                                    // multi-MB envelope — so a page still packs
                                    // several referenced messages.
                                    let cost = r.chunk_hashes.len() * 34 + 96;
                                    (Vec::new(), Some(r), cost)
                                }
                                Err(e) => {
                                    // Staging failed (byte plane down / transient
                                    // I/O). Degrade to skipping THIS message only
                                    // (still readable over IMAP), never fail the whole
                                    // page: one big message must not take the mailbox
                                    // hostage. Content-addressed staging retries
                                    // idempotently on the next poll.
                                    tracing::warn!(
                                        actor = ?actor_id,
                                        mailbox = %mailbox,
                                        uid = row.uid,
                                        envelope_bytes = shippable.len(),
                                        "mailbox.fetch: staging an over-frame envelope on the byte \
                                         plane failed — skipping this message (still readable over \
                                         IMAP), page continues: {e:?}"
                                    );
                                    continue;
                                }
                            }
                        } else {
                            let n = shippable.len();
                            (shippable, None, n)
                        };
                        // The first message always fits: an inline envelope here is
                        // ≤ the budget (over-budget ones became references above) and
                        // a reference's cost is tiny, so `page_bytes` (0 on the first
                        // message) never trips this on entry.
                        if page_bytes + wire_cost > frame_budget {
                            more = true;
                            break;
                        }
                        page_bytes += wire_cost;
                        messages.push(InboxMessage {
                            uid: row.uid,
                            message_id: row.message_id.to_vec(),
                            internal_date: row.internal_date,
                            // The record's seal instant (epoch seconds) — the
                            // client's content-sealing-epochs classification
                            // basis; ms→s, the unknown `0` passing through as
                            // `0`. Exact mirror of the MDA FETCH path
                            // (bridge_imap_handlers).
                            stored_at: row.stored_at.max(0) / 1000,
                            sealed_envelope,
                            body_ref,
                            // Split the space-separated IMAP flag/keyword string so
                            // the on-device scorer can read the `$FaunaSpamScored`
                            // watermark (mail-spam.md § Re-file timing).
                            flags: row.flags.split_whitespace().map(String::from).collect(),
                            extra: Default::default(),
                        });
                    }
                    None => {
                        tracing::warn!(
                            actor = ?actor_id,
                            mailbox = %mailbox,
                            message_id = ?row.message_id,
                            uid = row.uid,
                            "mailbox.fetch: placement row has no segment body (mirror divergence) — skipping"
                        );
                    }
                }
            }

            encode_reply(&InboxFetchReply {
                messages,
                more,
                highest_modseq,
                extra: Default::default(),
            })
        })
    })
}

fn inbox_fetch_handler() -> RpcHandler {
    mailbox_fetch_handler("INBOX", "fauna.email.inbox.fetch")
}

fn sent_fetch_handler() -> RpcHandler {
    mailbox_fetch_handler("Sent", "fauna.email.sent.fetch")
}

// ── fauna.email.apply_spam_disposition ─────────────────────────────
//
// The on-device spam scorer's outcome for a `User`-class Fauna app,
// applied to the caller's OWN INBOX (`mail-spam.md` § Wire shapes,
// § Re-file timing). Least-privilege: watermark `scored_uids` with
// `$FaunaSpamScored`, then move `junk_uids` (⊆ scored_uids) INBOX→Junk —
// nothing else (no arbitrary flag / mailbox). The nest executes it blindly
// (`content-scoring.md` § Stages: it sees the placement change, never the
// score). The `User` twin of the MDA's `scoreSelectedInbox` StoreFlags+Move
// pair, over the identical `db.apply_store_flags` / `db.apply_move` machinery.
fn apply_spam_disposition_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.apply_spam_disposition").await?;
            let req: ApplySpamDispositionRequest = decode(&payload).map_err(malformed)?;

            // Least-privilege guard: every junk UID must be one the scorer
            // scored (it only ever moves messages it scored). A stray UID is a
            // malformed request, not a silent skip.
            let scored: std::collections::BTreeSet<u32> = req.scored_uids.iter().copied().collect();
            if let Some(stray) = req.junk_uids.iter().find(|u| !scored.contains(u)) {
                return Err(malformed(format!(
                    "junk_uids must be a subset of scored_uids (stray uid {stray})"
                )));
            }

            // Refuse on a pure-backup destination (no local mailbox to mutate)
            // — same gate the read path uses.
            crate::bridge_routing_handlers::refuse_if_pure_backup(
                &state,
                "mail",
                &actor_id,
                crate::bridge_routing_handlers::pure_backup_destination,
            )
            .await?;

            // Idempotent — seed the caller's own standard mailboxes so a
            // never-received-mail actor is a clean no-op rather than an error.
            state
                .db
                .ensure_bridge_imap_mailboxes(&actor_id)
                .await
                .map_err(internal)?;

            // (1) Watermark every scored UID in the caller's INBOX with the
            // shared `$FaunaSpamScored` keyword — BEFORE the move, so moved
            // rows carry it (RFC 9051 §6.4.7: keywords carry on COPY/MOVE) and
            // a later "not spam" move-back stays scored and is not re-Junked
            // (`mail-spam.md` § Re-file timing). The watermark is an
            // internal/invisible-migratable keyword, so — unlike a user-visible
            // flag change — it appends no placement-journal record (an IMAP MUA
            // never acts on it).
            let watermark_flags = [SPAM_SCORED_KEYWORD.to_string()];
            let watermarked = if req.scored_uids.is_empty() {
                0
            } else {
                let outcome = state
                    .db
                    .apply_store_flags(
                        &actor_id,
                        "INBOX",
                        &req.scored_uids,
                        StoreFlagsDbOp::Add,
                        &watermark_flags,
                        None,
                    )
                    .await
                    .map_err(internal)?;
                outcome.updated.len() as u32
            };

            // (2) Move the spam subset INBOX→Junk. No quota pre-check: a
            // same-actor INBOX→Junk reshuffle adds no net storage, and spam
            // must always be demotable regardless of the actor's quota.
            let moved_to_junk = if req.junk_uids.is_empty() {
                0
            } else {
                let outcome = state
                    .db
                    .apply_move(&actor_id, "INBOX", &req.junk_uids, "Junk")
                    .await
                    .map_err(internal)?;

                // Placement-journal append + IDLE push, for parity with the
                // BridgeMda `move` path, so a concurrent IMAP MUA (QRESYNC /
                // IDLE) sees the same INBOX→Junk placement change.
                if !outcome.moved.is_empty() {
                    let (src_uid_set, dst_uid_set): (Vec<u32>, Vec<u32>) =
                        outcome.moved.iter().copied().unzip();
                    let record = MailPlacementRecord::Move {
                        src_mailbox: "INBOX".to_string(),
                        src_uid_set,
                        dst_mailbox: "Junk".to_string(),
                        dst_uid_set,
                        modseq_src: outcome.source_highestmodseq as u64,
                        modseq_dst: outcome.dest_highestmodseq as u64,
                        deleted_at: outcome.moved_at,
                    };
                    state
                        .mail_placement
                        .append_event(&actor_id, &record)
                        .await
                        .map_err(internal)?;

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
                            &actor_id,
                            "INBOX",
                            event(MoveSide::Source),
                        );
                        emit_mailbox_state_event(
                            &state,
                            &actor_id,
                            "Junk",
                            event(MoveSide::Destination),
                        );
                    }
                }
                outcome.moved.len() as u32
            };

            encode_reply(&ApplySpamDispositionReply {
                watermarked,
                moved_to_junk,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.email.inbox.mark_seen / fauna.email.inbox.flag_changes ───
//
// A Fauna app's mail read state IS the IMAP `\Seen` flag
// (`mail-app-surface.md` § Read state). `mark_seen` is the second named-effect
// `User` flag write beside `apply_spam_disposition`: it can only ADD `\Seen` to
// the caller's own INBOX, over the same `db.apply_store_flags` +
// `record_flag_writes` path the BridgeMda `store_flags` takes — so modseq
// bumps, the `StoreFlags` placement record is journaled, an IDLE-ing mail
// client hears it, and the actor's other devices get `fauna.mail.flags_changed`.

const SEEN_FLAG: &str = "\\Seen";

fn mark_seen_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.inbox.mark_seen").await?;
            let req: MarkSeenRequest = decode(&payload).map_err(malformed)?;
            crate::bridge_routing_handlers::refuse_if_pure_backup(
                &state,
                "mail",
                &actor_id,
                crate::bridge_routing_handlers::pure_backup_destination,
            )
            .await?;
            if req.uids.is_empty() {
                return encode_reply(&MarkSeenReply::default());
            }
            // Unknown/expunged UIDs are skipped by the STORE; an already-seen
            // row is reported unchanged and neither written nor journaled.
            let outcome = state
                .db
                .apply_store_flags(
                    &actor_id,
                    "INBOX",
                    &req.uids,
                    StoreFlagsDbOp::Add,
                    &[SEEN_FLAG.to_string()],
                    None,
                )
                .await
                .map_err(internal)?;
            let updated = crate::bridge_imap_handlers::record_flag_writes(
                &state,
                &actor_id,
                "INBOX",
                &outcome.updated,
            )
            .await?;
            encode_reply(&MarkSeenReply {
                updated,
                extra: Default::default(),
            })
        })
    })
}

/// Page-size bounds for `flag_changes`: a row is a uid, a short flag list and
/// a modseq, so a page is far larger than the body-carrying fetch feeds'.
const FLAG_CHANGES_DEFAULT_LIMIT: u32 = 500;
const FLAG_CHANGES_MAX_LIMIT: u32 = 1000;

fn flag_changes_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.email.inbox.flag_changes").await?;
            let req: FlagChangesRequest = decode(&payload).map_err(malformed)?;
            crate::bridge_routing_handlers::refuse_if_pure_backup(
                &state,
                "mail",
                &actor_id,
                crate::bridge_routing_handlers::pure_backup_destination,
            )
            .await?;
            // Seed the standard mailboxes so a never-mailed actor reads an
            // empty delta over a real HIGHESTMODSEQ rather than a zero.
            state
                .db
                .ensure_bridge_imap_mailboxes(&actor_id)
                .await
                .map_err(internal)?;
            let limit = match req.limit {
                0 => FLAG_CHANGES_DEFAULT_LIMIT,
                n => n.min(FLAG_CHANGES_MAX_LIMIT),
            };
            let since = i64::try_from(req.since_modseq).unwrap_or(i64::MAX);
            let (rows, more, highest) = state
                .db
                .list_bridge_imap_flag_changes(&actor_id, "INBOX", since, req.after_uid, limit)
                .await
                .map_err(internal)?;
            let changes = rows
                .into_iter()
                .map(|(uid, flags, modseq)| FlagChange {
                    uid,
                    flags: flags.split_whitespace().map(String::from).collect(),
                    modseq: modseq.max(0) as u64,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&FlagChangesReply {
                changes,
                highest_modseq: highest.max(0) as u64,
                more,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_email_handlers(b: &mut RpcRouterBuilder) {
    let read = || Duration::from_secs(5);
    b.add(
        "fauna.email.filters.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.email.filters.create",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: create_handler(),
        },
    );
    b.add(
        "fauna.email.filters.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: get_handler(),
        },
    );
    b.add(
        "fauna.email.filters.update",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: update_handler(),
        },
    );
    b.add(
        "fauna.email.filters.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: delete_handler(),
        },
    );
    // `fauna.email.send` forbids replay at 30 s — see
    // `KindRegistry::register_email_kinds` for the rationale; same
    // shape as `fauna.bridges.link`, the OAuth-flow precedent.
    b.add(
        "fauna.email.send",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: send_handler(),
        },
    );
    // `fauna.email.inbox.fetch` — caller-scoped INBOX read (the client
    // mail-receive feed). Idempotent read; 60 s deadline (bodies can be
    // large) — see `KindRegistry::register_email_kinds`.
    b.add(
        "fauna.email.inbox.fetch",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: inbox_fetch_handler(),
        },
    );
    // `fauna.email.sent.fetch` — caller-scoped Sent read (the outbound
    // copy of mail submitted from an external MUA). Same meta shape as
    // inbox.fetch: idempotent read, 60 s deadline (bodies can be large).
    b.add(
        "fauna.email.sent.fetch",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: sent_fetch_handler(),
        },
    );
    // `fauna.email.apply_spam_disposition` — the on-device scorer's outcome
    // (watermark + INBOX→Junk) for the caller's own INBOX. Idempotent on
    // replay (re-watermark is a set-union no-op; a re-move finds the UIDs
    // already gone from INBOX), so `forbid_replay=false`; 30 s write deadline.
    b.add(
        "fauna.email.apply_spam_disposition",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: apply_spam_disposition_handler(),
        },
    );
    // `fauna.email.inbox.mark_seen` — add `\Seen` to the caller's own INBOX
    // UIDs (`mail-app-surface.md` § Read state). Idempotent on replay (a
    // re-add changes no row and bumps no modseq); 30 s write deadline.
    b.add(
        "fauna.email.inbox.mark_seen",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: mark_seen_handler(),
        },
    );
    // `fauna.email.inbox.flag_changes` — the caller's INBOX flag delta past a
    // modseq cursor. A small pure read.
    b.add(
        "fauna.email.inbox.flag_changes",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: flag_changes_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the action→on-disk→action path is a fixed point for every
    /// `EmailFilterAction` variant — the same invariant the HTTP twin relies on
    /// for read-after-write correctness. Catches any divergence between
    /// `action_to_storage` / `action_from_storage` (the string half plus the
    /// `forward_redirect` column, which is the only thing that tells a
    /// `redirect` Forward from a `copy` one).
    #[test]
    fn action_round_trips_through_storage_string() {
        let variants = vec![
            WireEmailFilterAction::Allow,
            WireEmailFilterAction::Discard,
            WireEmailFilterAction::Reject {
                reason: "spam".into(),
            },
            WireEmailFilterAction::FileInto {
                mailbox: "Archive".into(),
            },
            WireEmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect: false,
            },
            WireEmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect: true,
            },
            WireEmailFilterAction::AutoReply {
                subject: "Out".into(),
                body: "Back\nMonday".into(),
                interval_hours: 12,
            },
            WireEmailFilterAction::AddLabel {
                label: "important".into(),
            },
        ];
        for a in variants {
            validate_action(&a).expect("valid");
            let (s, redirect) = action_to_storage(&a);
            let parsed = action_from_storage(&s, redirect);
            assert_eq!(a, parsed, "round-trip mismatch via {s:?}/{redirect}");
        }
    }

    /// The copy mode lives in the column, never the string: both modes store
    /// the same `forward:<address>` (so an older binary reads either as
    /// `copy`), and only the column distinguishes them. A `1` on a
    /// non-forward row is inert.
    #[test]
    fn forward_copy_mode_is_carried_by_the_column_not_the_string() {
        let copy = WireEmailFilterAction::Forward {
            address: "bob@example.com".into(),
            redirect: false,
        };
        let redirect = WireEmailFilterAction::Forward {
            address: "bob@example.com".into(),
            redirect: true,
        };
        assert_eq!(
            action_to_storage(&copy),
            ("forward:bob@example.com".into(), false)
        );
        assert_eq!(
            action_to_storage(&redirect),
            ("forward:bob@example.com".into(), true)
        );
        assert_eq!(action_from_storage("forward:bob@example.com", false), copy);
        assert_eq!(
            action_from_storage("forward:bob@example.com", true),
            redirect
        );
        assert_eq!(
            action_from_storage("fileinto:Archive", true),
            WireEmailFilterAction::FileInto {
                mailbox: "Archive".into(),
            }
        );
        assert!(!action_to_storage(&WireEmailFilterAction::Discard).1);
    }

    /// A forwarding rule's destination is syntax-checked at write time
    /// (`mail-forwarding.md` § Per-rule "forward to"); a well-formed external
    /// address passes.
    #[test]
    fn forward_action_refuses_a_malformed_destination() {
        for bad in [
            "",
            "no-at-sign",
            "a@b@c.example",
            "bob@dotless",
            "a b@example.net",
        ] {
            let a = WireEmailFilterAction::Forward {
                address: bad.into(),
                redirect: false,
            };
            assert!(validate_action(&a).is_err(), "{bad:?} must be refused");
        }
        let ok = WireEmailFilterAction::Forward {
            address: "bob+x@mail.example.net".into(),
            redirect: true,
        };
        assert!(validate_action(&ok).is_ok());
    }

    /// Pin every action's succession disposition
    /// (`succession-aftermath.md` § Re-key scope). The exhaustive match in
    /// [`filter_succession`] is what makes a *new* variant impossible to ship
    /// un-ruled; this pins that the ruling each existing variant carries is the
    /// one the registry prose claims.
    ///
    /// The `Reject` pair is the interesting one: the disposition turns on
    /// whether there is thief-authorable text to drop, not on the action tag.
    #[test]
    fn every_filter_action_carries_its_succession_disposition() {
        use WireEmailFilterAction as W;
        let cases = vec![
            // Outward emission under the successor's recovered identity.
            (
                W::Forward {
                    address: "thief@evil.example".into(),
                    redirect: true,
                },
                FilterSuccession::Burn,
            ),
            (
                W::AutoReply {
                    subject: "I have moved".into(),
                    body: "Write me at thief@evil.example".into(),
                    interval_hours: 24,
                },
                FilterSuccession::Burn,
            ),
            // Narrowing that must survive, carrying text that must not.
            (
                W::Reject {
                    reason: "go away".into(),
                },
                FilterSuccession::MoveDisarmed("reject:".into()),
            ),
            // Already disarmed — nothing to rewrite, so it is a plain move.
            (
                W::Reject {
                    reason: String::new(),
                },
                FilterSuccession::Move,
            ),
            // Local placement / labelling.
            (W::Allow, FilterSuccession::Move),
            (W::Discard, FilterSuccession::Move),
            (
                W::FileInto {
                    mailbox: "Archive".into(),
                },
                FilterSuccession::Move,
            ),
            (
                W::AddLabel {
                    label: "important".into(),
                },
                FilterSuccession::Move,
            ),
        ];
        for (action, want) in cases {
            let stored = action_to_string(&action);
            assert_eq!(
                filter_succession(&stored),
                want,
                "wrong succession disposition for {stored:?}"
            );
        }
    }

    /// A stored action string this nest does not recognise — a newer nest wrote
    /// it, or the row is malformed — reads as a filter that does nothing, never
    /// as `Discard`, and is written back unchanged when the filter is saved
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: an
    /// unknown filter action does nothing).
    #[test]
    fn an_unknown_stored_action_does_nothing_and_is_written_back_unchanged() {
        for stored in ["quarantine:Spam", "autoreply:no-interval", "", "Allow"] {
            let action = action_from_storage(stored, false);
            assert_eq!(
                action,
                WireEmailFilterAction::Unknown(CarriedValue(Value::String(stored.into()))),
                "{stored:?} must read as unknown, not as a live action"
            );
            validate_action(&action).expect("a carried storage string is valid");
            assert_eq!(action_to_storage(&action), (stored.to_string(), false));
        }
    }

    /// An unknown action an app sends is accepted only as the storage string
    /// this nest handed out: a value carried from somewhere else has no
    /// storage form here, and a string this nest DOES parse would skip the
    /// checks its known action gets (a forward destination, a mailbox, a
    /// label).
    #[test]
    fn an_unknown_action_is_refused_unless_it_is_a_carried_storage_string() {
        use std::collections::BTreeMap;
        let wire_shape = WireEmailFilterAction::Unknown(CarriedValue(Value::Map(BTreeMap::from(
            [("Snooze".to_string(), Value::Map(BTreeMap::new()))],
        ))));
        assert!(validate_action(&wire_shape).is_err());
        for known in [
            "discard",
            "forward:thief@evil.example",
            "fileinto:Sent",
            "addlabel:\\Deleted",
        ] {
            let smuggled =
                WireEmailFilterAction::Unknown(CarriedValue(Value::String(known.into())));
            assert!(
                validate_action(&smuggled).is_err(),
                "{known:?} must not pass as an unknown action"
            );
        }
    }

    /// An unknown rule or action is written only as the echo of the one the
    /// stored filter holds: a create carrying one is refused, an update may
    /// keep exactly what the filter holds and nothing else.
    #[test]
    fn an_unknown_rule_or_action_is_stored_only_as_an_echo() {
        let unknown_rule = WireEmailFilterRule::Unknown(CarriedValue(Value::String("r".into())));
        let other_rule = WireEmailFilterRule::Unknown(CarriedValue(Value::String("x".into())));
        let unknown_action =
            WireEmailFilterAction::Unknown(CarriedValue(Value::String("quarantine:Spam".into())));
        let held = row_to_wire(EmailFilterRow {
            id: 1,
            owner: vec![0; 32],
            name: "n".into(),
            rules: fauna_core::encoding::canonical_encode(&vec![unknown_rule.clone()]).unwrap(),
            combination: "all".into(),
            action: "quarantine:Spam".into(),
            priority: 0,
            continue_on_match: false,
            forward_redirect: false,
            created_at: 0,
        });
        // Create: nothing is held.
        assert!(
            refuse_unheld_unknowns(
                std::slice::from_ref(&unknown_rule),
                &WireEmailFilterAction::Allow,
                None
            )
            .is_err()
        );
        assert!(refuse_unheld_unknowns(&[], &unknown_action, None).is_err());
        // Update: the held values pass, anything else does not.
        assert!(
            refuse_unheld_unknowns(
                std::slice::from_ref(&unknown_rule),
                &unknown_action,
                Some(&held)
            )
            .is_ok()
        );
        assert!(refuse_unheld_unknowns(&[other_rule], &unknown_action, Some(&held)).is_err());
        let other_action =
            WireEmailFilterAction::Unknown(CarriedValue(Value::String("snooze:4".into())));
        assert!(refuse_unheld_unknowns(&[], &other_action, Some(&held)).is_err());
    }

    /// The succession ceremony cannot rule an action it cannot read out of
    /// the outward-emission class, so an unknown one takes the burn class: it
    /// does nothing on this nest, and it must not come back armed on a newer
    /// one.
    #[test]
    fn an_unknown_stored_action_burns_at_succession() {
        assert_eq!(filter_succession("quarantine:Spam"), FilterSuccession::Burn);
    }

    /// A `rules` blob that does not decode never matches: it reads as one
    /// unknown condition (which never matches under `all` or `any`), never as
    /// the empty list that matches every message under `all`. This projection
    /// feeds both the user's list and the mail bridge's
    /// `fetch_recipient_filters`.
    #[test]
    fn an_undecodable_rules_blob_never_reads_as_an_empty_rule_list() {
        let row = EmailFilterRow {
            id: 1,
            owner: vec![0; 32],
            name: "corrupt".into(),
            rules: vec![0xff, 0x00, 0x13],
            combination: "all".into(),
            action: "discard".into(),
            priority: 0,
            continue_on_match: false,
            forward_redirect: false,
            created_at: 0,
        };
        let wire = row_to_wire(row);
        assert_eq!(wire.rules.len(), 1);
        assert!(matches!(wire.rules[0], WireEmailFilterRule::Unknown(_)));
    }

    /// A disarmed `Reject` must still parse back as a `Reject`, not decay out
    /// of it. This is the whole reason the burn class is deleted rather than
    /// rewritten: a rewrite that lands outside the parseable shape reads as an
    /// unknown action, which does nothing — silently converting a refusal into
    /// delivery.
    #[test]
    fn a_disarmed_reject_is_still_a_reject() {
        let armed = action_to_string(&WireEmailFilterAction::Reject {
            reason: "thief text".into(),
        });
        let FilterSuccession::MoveDisarmed(disarmed) = filter_succession(&armed) else {
            panic!("an armed Reject must disarm");
        };
        assert_eq!(
            action_from_string(&disarmed),
            WireEmailFilterAction::Reject {
                reason: String::new()
            },
            "the disarmed action decayed out of the Reject shape — a rewrite \
             that leaves the parseable shape becomes silent mail destruction"
        );
    }

    /// Verify that the rule→dag-cbor→rule path is a fixed point for every
    /// variant. `EmailFilterRule` is both the wire type and the on-disk storage
    /// shape (one type, no mirror), so this guards the canonical encode/decode
    /// round-trip the `rules` BLOB relies on.
    #[test]
    fn rule_round_trips_through_storage_blob() {
        // `EmailFilterRule` is the canonical storage shape: it serializes into
        // the `rules` BLOB and decodes back verbatim (no store-type mirror).
        let variants = vec![
            WireEmailFilterRule::SenderIs {
                address: "a@b.c".into(),
            },
            WireEmailFilterRule::SenderDomain {
                domain: "b.c".into(),
            },
            WireEmailFilterRule::SubjectContains { text: "hi".into() },
            WireEmailFilterRule::BodyContains { text: "yo".into() },
            WireEmailFilterRule::HeaderExists {
                name: "X-Foo".into(),
            },
            WireEmailFilterRule::HeaderContains {
                name: "X-Foo".into(),
                value: "bar".into(),
            },
            WireEmailFilterRule::SpamScoreAtLeast { milli: 7_500 },
        ];
        for r in variants {
            let bytes =
                fauna_core::encoding::canonical_encode(&vec![r.clone()]).expect("canonical encode");
            let parsed: Vec<WireEmailFilterRule> =
                fauna_core::encoding::canonical_decode(&bytes).expect("decode");
            assert_eq!(parsed, vec![r], "round-trip mismatch");
        }
    }

    /// Layer-6 (Domain E) discriminating red: the at-rest `email_filters.rules`
    /// BLOB produced by `validate_and_project` MUST be canonical dag-cbor
    /// (serialization.md:29 — every persisted byte through one canonical
    /// encoder), not BARE. `fauna_protocol::email::EmailFilterRule` is the sole
    /// rule type (producer + consumer); the Go MTA perimeter re-decodes the
    /// same bytes into `fauna_mail::filter::FilterCondition` by variant NAME
    /// (dag-cbor keys enums by name, not serde_bare's positional index). Pre-flip
    /// serde_bare bytes fail strict `canonical_decode` with `NotCanonical`.
    #[test]
    fn email_filter_rules_at_rest_is_canonical_dagcbor() {
        let rules = vec![
            WireEmailFilterRule::SenderDomain {
                domain: "example.com".into(),
            },
            WireEmailFilterRule::SubjectContains {
                text: "invoice".into(),
            },
        ];
        let (bytes, _action, _redirect) =
            validate_and_project("f", rules.clone(), "all", WireEmailFilterAction::Allow)
                .expect("project rules");
        let proj: Vec<WireEmailFilterRule> = fauna_core::encoding::canonical_decode(&bytes)
            .expect("at-rest rules must be canonical dag-cbor");
        assert_eq!(proj, rules);
    }

    #[test]
    fn validate_rejects_empty_name() {
        let err = validate_and_project("", Vec::new(), "all", WireEmailFilterAction::Allow);
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_unknown_combination() {
        let err = validate_and_project(
            "n",
            Vec::new(),
            "either", // not all/any
            WireEmailFilterAction::Allow,
        );
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_oversize_name() {
        let big = "x".repeat(MAX_NAME_LEN + 1);
        let err = validate_and_project(&big, Vec::new(), "all", WireEmailFilterAction::Allow);
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_too_many_rules() {
        let too_many: Vec<WireEmailFilterRule> = (0..(MAX_RULES + 1))
            .map(|i| WireEmailFilterRule::SubjectContains {
                text: format!("t{i}"),
            })
            .collect();
        let err = validate_and_project("n", too_many, "all", WireEmailFilterAction::Allow);
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_oversize_rule_text() {
        let big = "x".repeat(MAX_TEXT_FIELD_LEN + 1);
        let err = validate_and_project(
            "n",
            vec![WireEmailFilterRule::SubjectContains { text: big }],
            "all",
            WireEmailFilterAction::Allow,
        );
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_addlabel_system_flag() {
        // EF-1: an `AddLabel` whose label is an IMAP system flag (`\Deleted`)
        // is rejected at create time — it would otherwise ride inbound delivery
        // as a `\Deleted` keyword and make matching mail EXPUNGE-eligible.
        let err = validate_and_project(
            "n",
            Vec::new(),
            "all",
            WireEmailFilterAction::AddLabel {
                label: "\\Deleted".into(),
            },
        );
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_rejects_addlabel_whitespace() {
        let err = validate_and_project(
            "n",
            Vec::new(),
            "all",
            WireEmailFilterAction::AddLabel {
                label: "two words".into(),
            },
        );
        assert_eq!(err.unwrap_err().code, "fauna.email.invalid_params");
    }

    #[test]
    fn validate_accepts_addlabel_plain_keyword() {
        // A plain keyword passes — the common case stays valid.
        let ok = validate_and_project(
            "n",
            Vec::new(),
            "all",
            WireEmailFilterAction::AddLabel {
                label: "Newsletter".into(),
            },
        );
        assert!(ok.is_ok(), "a plain keyword label must validate");
    }

    #[test]
    fn validate_rejects_file_into_a_refused_or_malformed_mailbox() {
        // `Sent` and `Drafts` hold only this account's own writing, and the
        // guardian's held mailbox holds only holds (`email-filters.md` § Email
        // filter rules). A malformed name is one no mailbox can carry. Create and
        // update share this check.
        for mailbox in [
            "Sent",
            "Drafts",
            crate::db::bridge_imap::GUARDIAN_HELD_MAILBOX,
            "",
            "Reports//2026",
        ] {
            let err = validate_and_project(
                "n",
                Vec::new(),
                "all",
                WireEmailFilterAction::FileInto {
                    mailbox: mailbox.into(),
                },
            );
            assert_eq!(
                err.unwrap_err().code,
                "fauna.email.invalid_params",
                "FileInto {mailbox:?} must be refused"
            );
        }
    }

    #[test]
    fn validate_accepts_file_into_the_mailboxes_inbound_mail_belongs_in() {
        // The refusal is narrow: the other standard mailboxes and any
        // well-formed custom folder stay targets (the tier_3 delivery test files
        // into `Junk`).
        for mailbox in [
            "INBOX",
            "Archive",
            "Junk",
            "Trash",
            "Reports",
            "Receipts/2026",
        ] {
            let ok = validate_and_project(
                "n",
                Vec::new(),
                "all",
                WireEmailFilterAction::FileInto {
                    mailbox: mailbox.into(),
                },
            );
            assert!(ok.is_ok(), "FileInto {mailbox:?} must validate");
        }
    }

    // ── In-domain delivery through the APP send door ──────────
    //
    // `fauna.email.send` is the door `libs/fauna-client-email` — and therefore
    // tui's Conversations mail rail — sends on. Its in-domain half must reach
    // the same recipients the SMTP submission path and inbound MX reach, or the
    // same address behaves differently depending on which client sent to it.

    async fn send_fixture(domain: &str) -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        db.add_mail_domain(domain, true, "testing", "self_signed", None, None)
            .await
            .unwrap();
        Arc::new(AppState::for_test(db))
    }

    /// Send `raw` from `sender` to `recipients` through the real handler and
    /// return the reply's local-delivery count.
    async fn send_via_handler(
        state: &Arc<AppState>,
        sender: [u8; 32],
        recipients: Vec<String>,
        raw: &[u8],
    ) -> u32 {
        let req = fauna_protocol::email::SendEmailRequest {
            recipients,
            raw_rfc5322: raw.to_vec(),
            extra: Default::default(),
        };
        let payload = bytes::Bytes::from(fauna_protocol::encode_canonical(&req).unwrap().to_vec());
        let bytes = send_handler()(state.clone(), sender, payload)
            .await
            .expect("send handler ok");
        let reply: fauna_protocol::email::SendEmailReply =
            fauna_protocol::decode_strict(&bytes).expect("decodes");
        reply.local_delivered
    }

    /// A registered sender whose handle matches the `From:` local part — the
    /// send door verifies the two agree on every call.
    async fn seed_sender(state: &Arc<AppState>) -> [u8; 32] {
        let sender = [7u8; 32];
        state.db.create_user(&sender, "free", "t").await.unwrap();
        state.db.set_handle(&sender, "sender").await.unwrap();
        sender
    }

    async fn seed_local_recipient(state: &Arc<AppState>, domain: &str, pattern: &str) -> [u8; 32] {
        let actor = [3u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &actor,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
            .db
            .put_exact_alias(domain, pattern, "exact", &actor)
            .await
            .unwrap();
        actor
    }

    #[tokio::test]
    async fn send_delivers_to_an_in_domain_exact_recipient() {
        // The case that has always worked — kept as the control, so the
        // sub-address test below cannot pass by the send failing for some
        // unrelated reason.
        let state = send_fixture("example.com").await;
        let recipient = seed_local_recipient(&state, "example.com", "test").await;
        let sender = seed_sender(&state).await;

        let delivered = send_via_handler(
            &state,
            sender,
            vec!["test@example.com".into()],
            b"From: sender@example.com\r\nTo: test@example.com\r\nSubject: hi\r\n\r\nbody\r\n",
        )
        .await;

        assert_eq!(
            delivered, 1,
            "an exact in-domain recipient must be delivered"
        );
        let inbox = state
            .db
            .query_bridge_imap_messages(&recipient, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1);
    }

    #[tokio::test]
    async fn send_refuses_a_supervised_ward_reached_at_a_subaddress() {
        // The guardian gate must be exactly as wide as the delivery it guards.
        // Its pre-flight used to run its OWN exact-only lookup, which was
        // harmless only while delivery was exact-only too; teaching delivery to
        // resolve sub-addresses without teaching the gate would have turned
        // `ward+anything@` into a live bypass of a family-safety refusal
        // (`family-safety.md` § The mail gate).
        let state = send_fixture("example.com").await;
        let guardian = [4u8; 32];
        let ward = [5u8; 32];
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
                "reject",
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
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &ward,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
            .db
            .put_exact_alias("example.com", "kid", "exact", &ward)
            .await
            .unwrap();
        let sender = seed_sender(&state).await;

        let req = fauna_protocol::email::SendEmailRequest {
            recipients: vec!["kid+games@example.com".into()],
            raw_rfc5322:
                b"From: sender@example.com\r\nTo: kid+games@example.com\r\nSubject: hi\r\n\r\nbody\r\n"
                    .to_vec(),
            extra: Default::default(),
        };
        let payload = bytes::Bytes::from(fauna_protocol::encode_canonical(&req).unwrap().to_vec());
        let err = send_handler()(state.clone(), sender, payload)
            .await
            .expect_err("a rejected ward must refuse the whole send, sub-address included");

        assert!(
            err.details
                .as_ref()
                .map(|d| format!("{d:?}").contains("kid+games@example.com"))
                .unwrap_or(false),
            "the refusal must name the recipient it refused; got {err:?}"
        );
        // And nothing was delivered — the refusal is a pre-flight, never a
        // partial delivery the sender would have to guess at.
        let inbox = state
            .db
            .query_bridge_imap_messages(&ward, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert!(inbox.is_empty(), "a refused send delivers to nobody");
    }

    #[tokio::test]
    async fn send_stamps_the_delivered_in_domain_copy() {
        // The stamps must ride INSIDE the sealed body, as they do on both Go
        // delivery paths — otherwise the recipient's filter rules and the MDA's
        // threshold read see nothing on locally-sent mail. The sealed record
        // cannot be opened here, so the observable is its SIZE: a sub-addressed
        // recipient carries `X-Fauna-Address-Suffix` (plus the threshold stamp
        // both deliveries get), so its record must be strictly larger than the
        // exact recipient's for the same message.
        let raw: &[u8] =
            b"From: sender@example.com\r\nTo: test@example.com\r\nSubject: hi\r\n\r\nbody\r\n";

        async fn sealed_len(local_part: &str, raw: &[u8]) -> usize {
            let state = send_fixture("example.com").await;
            let recipient = seed_local_recipient(&state, "example.com", "test").await;
            let sender = seed_sender(&state).await;
            let delivered = send_via_handler(
                &state,
                sender,
                vec![format!("{local_part}@example.com")],
                raw,
            )
            .await;
            assert_eq!(delivered, 1, "{local_part} must be delivered");
            let records = crate::segments::mail::read_after_seq(
                &state.mail_segments,
                &state.db,
                &recipient,
                0,
                10,
            )
            .await
            .unwrap();
            assert_eq!(records.len(), 1, "one record delivered");
            let (envelope, _floor) = crate::segments::mail::read_record_with_floor(
                &state.mail_segments,
                &state.db,
                &recipient,
                &records[0].1,
            )
            .await
            .unwrap()
            .expect("the delivered record is readable");
            envelope.encrypted_body.len()
        }

        let exact = sealed_len("test", raw).await;
        let subaddress = sealed_len("test+work", raw).await;
        assert!(
            subaddress > exact,
            "the sub-addressed copy must carry an extra X-Fauna-Address-Suffix stamp \
             inside its seal (exact={exact}, subaddress={subaddress}) — equal sizes mean \
             the resolver's stamps never reached the sealed body"
        );
    }

    #[tokio::test]
    async fn send_delivers_to_an_in_domain_subaddress_recipient() {
        // Row 174: `partition_recipients` hands the send door the UNSTRIPPED
        // local part (`test+work`), and the door resolves it with an
        // exact-pattern lookup, so a sub-address of the sender's own nest never
        // resolves and the message is silently not delivered — the miss is
        // swallowed into `remote_errors` while `local_delivered` stays 0. The
        // identical address resolves fine over SMTP submission and inbound MX
        // (`enqueue_outbound_mail_locally_delivers_in_domain_subaddress` is the
        // same case one door over, and it passes).
        let state = send_fixture("example.com").await;
        let recipient = seed_local_recipient(&state, "example.com", "test").await;
        let sender = seed_sender(&state).await;

        let delivered = send_via_handler(
            &state,
            sender,
            vec!["test+work@example.com".into()],
            b"From: sender@example.com\r\nTo: test+work@example.com\r\nSubject: hi\r\n\r\nbody\r\n",
        )
        .await;

        assert_eq!(
            delivered, 1,
            "a sub-addressed in-domain recipient must be delivered — default-on \
             subaddressing resolves `test+work` back to `test`'s mailbox"
        );
        let inbox = state
            .db
            .query_bridge_imap_messages(&recipient, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "the sub-addressed recipient must receive it"
        );
    }
}
