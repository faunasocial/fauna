//! WS-RPC handlers for the user-facing connection-management surface — the
//! knocks inbox (`fauna.knocks.{list,accept,block,unblock,dismiss}`), the
//! contact roster (`fauna.contacts.{list,confirm}`), and the inbox-acceptance
//! policy (`fauna.inbox.mode.{get,set}`). Part of the WS-RPC-everywhere
//! migration (tracked internally); `knocks.unblock` added later (the
//! `profile-block-button` toggle substrate).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (User-only arms for these kinds); the same gate the `fauna.posts.*`,
//! `fauna.feed.*`, and `fauna.notifications.*` user-facing kinds use,
//! partitioned by `CallerClass`.
//!
//! Each handler decodes its wire request, then either calls a `CacheDb`
//! method directly (`poll_knocks` / `list_contacts_full` /
//! `promote_to_confirmed` / `unblock_contact` / `get_inbox_mode` /
//! `set_inbox_mode` — the DB method is the shared core for the single-call
//! paths) or one of the multi-DB-call cores below (`accept_contact_core` /
//! `block_contact_core` / `dismiss_knock_core`, which carry the dual-table
//! logic), and maps the result onto the cluster reply /
//! `RpcError` shapes.
//! The connection `actor_id` replaces the HTTP `{actor_id}` path param +
//! bearer-match (exactly the posts/feed/notifications treatment). The HTTP
//! twins (`knock_routes.rs`) + the `paths::{knocks,contacts}` /
//! `inbox::mode_for_actor` constants were deleted in T4 — these kinds are the
//! sole surface now.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::{
    RpcError,
    contacts::{
        ContactConfirmReply, ContactItem, ContactListReply, ContactListRequest, ContactStatusReply,
        InboxModeGetReply, InboxModeGetRequest, InboxModeSetReply, InboxModeSetRequest,
        KnockActionReply, KnockActionRequest, KnockItem, KnockListReply, KnockListRequest,
    },
    decode_strict as decode, encode_canonical,
};

use crate::api_error::ApiError;
use crate::routes::AppState;
use crate::routes::parse_32_bytes;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::malformed;

fn knocks_internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("knocks", err)
}

fn contacts_internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("contacts", err)
}

fn invalid_peer_id(cluster: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns(cluster, "invalid peer_id hex")
}

/// The inbox-mode handler rejected the requested mode (the HTTP twin
/// returned `400` on a value outside `{open, allow_knock, contacts_only,
/// closed}`). Mirrors the posts cluster's `invalid_params` convention.
fn invalid_inbox_mode(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_params_ns("inbox", err)
}

fn encode_reply<T: serde::Serialize>(
    reply: &T,
    internal: fn(String) -> RpcError,
) -> Result<Bytes, RpcError> {
    encode_canonical(reply)
        .map(|v| Bytes::from(v.to_vec()))
        .map_err(|e| internal(format!("encode reply: {e}")))
}

async fn require_permission(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    kind: &str,
    cluster: &str,
) -> Result<(), RpcError> {
    // The DB-lookup-error branch used to map to `permission_denied` (with the
    // raw error text leaking into the wire `reason` field) instead of an
    // `internal`-classified error like every other permission-gate site —
    // fixed here as part of the unification, not preserved.
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, |e| {
        crate::rpc_errors::internal_ns(cluster, e)
    })
    .await?;
    Ok(())
}

/// Parse the hex peer_id carried by the action requests (the posts pattern —
/// `channel_routes::parse_32_bytes`).
fn parse_peer_id(hex_str: &str, cluster: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_peer_id(cluster))
}

// ── Multi-DB-call cores ────────────────────────────────────────
//
// The accept/block/dismiss writes touch more than one table (the Bayesian
// auto-train that block once also fired is gone — a block is not a spam
// verdict, `mail-spam.md` § Implicit signals are forbidden). These cores
// carry that logic; the handlers below are thin shells over them. They returned
// `Result<(), ApiError>` when they were also called by the (now-deleted) HTTP
// twins in `knock_routes.rs`; the `ApiError` shape is preserved so the
// handlers map `e.message` onto `fauna.knocks.internal`.

/// Refuse a ward's own attempt to exercise a contact-lifecycle authority that
/// `contact_approval` has moved to their guardian (`family-safety.md` § Guardian
/// policy pillar 1, § Reach approvals).
///
/// Three ward-facing kinds share it. `knocks.accept` is the obvious one — the
/// ward must not accept their own new contacts. The other two are subtler and
/// grant no *fresh* reach, but each erodes the guardian's authority over the
/// queue that is the whole enforcement surface:
///
/// - `knocks.unblock` clears a `blocked` edge — including one the guardian just
///   created via `approvals.decide { approve: false }`, silently undoing the
///   guardian's denial.
/// - `knocks.dismiss` deletes a pending knock — and the guardian's approvals
///   queue *is* the wards' pending-knock set, so a ward could remove an incoming
///   contact attempt before the guardian ever saw it. The guardian's visibility
///   of *attempted* contact is not the ward's to revoke.
///
/// The guardian retains both powers through `fauna.family.approvals.decide`.
/// Reads stay open on every surface: supervision restricts authority, not
/// transparency (§ Don't make supervision silent).
async fn deny_if_contact_approval(
    state: &Arc<AppState>,
    supervised: &[u8; 32],
) -> Result<(), RpcError> {
    if let Some(policy) = state
        .db
        .get_guardian_policy(supervised)
        .await
        .map_err(|e| knocks_internal(format!("guardian policy: {e}")))?
        && policy.contact_approval
    {
        return Err(crate::rpc_errors::guardian_approval_required_ns(
            "knocks",
            "this account's new contacts require guardian approval",
        ));
    }
    Ok(())
}

/// Accept a knock from `peer_id`: mark the contact `accepted`, release the
/// arrival the knock was holding, then best-effort dismiss the now-handled knock
/// (a `dismiss_knock` failure is logged but does not fail the accept — the
/// contact relationship is the source of truth).
pub(crate) async fn accept_contact_core(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    peer_id: &[u8; 32],
) -> Result<(), ApiError> {
    if let Err(e) = state.db.accept_contact(actor_id, peer_id).await {
        tracing::error!("accept_contact error: {e}");
        return Err(ApiError::internal("storage error"));
    }

    // Read the held arrival BEFORE dismissing the knock that holds it. Accepting
    // a sender delivers the very payload that knocked — the group invite, the
    // first DM, the contact request's post — instead of dropping it and making
    // the sender re-send. For a supervised account this is what makes the
    // guardian's `approvals.decide { approve: true }` actually hand the ward the
    // arrival they approved (`family-safety.md` § Reach approvals).
    let held = match state.db.pending_knock_payload(actor_id, peer_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("pending_knock_payload before accept: {e}");
            None
        }
    };

    if let Err(e) = state.db.dismiss_knock(actor_id, peer_id).await {
        tracing::warn!("dismiss_knock after accept: {e}");
    }

    if let Some(body) = held.filter(|b| !b.is_empty()) {
        crate::routes::deliver_held_knock_payload(state, actor_id, peer_id, Bytes::from(body))
            .await;
    }

    Ok(())
}

/// Block a knock sender `peer_id`: mark the contact `blocked`, best-effort
/// dismiss the knock and delete its doorbell notification. It trains nothing:
/// a block is not a spam verdict (`mail-spam.md` § Implicit signals are
/// forbidden), and the universal spam seal removed the auto-train that once
/// rode here.
pub(crate) async fn block_contact_core(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    peer_id: &[u8; 32],
) -> Result<(), ApiError> {
    if let Err(e) = state.db.block_contact(actor_id, peer_id).await {
        tracing::error!("block_contact error: {e}");
        return Err(ApiError::internal("storage error"));
    }

    if let Err(e) = state.db.dismiss_knock(actor_id, peer_id).await {
        tracing::warn!("dismiss_knock after block: {e}");
    }
    // The knock's doorbell goes with it (`behavior/notifications.md`
    // § Retention, rule 3): a blocked stranger's text has no business
    // outliving the block.
    if let Err(e) = state.db.delete_knock_notification(actor_id, peer_id).await {
        tracing::warn!("delete_knock_notification after block: {e}");
    }

    Ok(())
}

/// Dismiss a knock from `peer_id`: delete the knock and its notification row
/// (the doorbell lives as long as the knock — `behavior/notifications.md`
/// § Retention, rule 3; `accept_contact_core` is the one path that keeps it),
/// then best-effort delete the contact (so the peer can knock again). A
/// `delete_contact` or doorbell failure is logged but does not fail the
/// dismiss.
async fn dismiss_knock_core(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    peer_id: &[u8; 32],
) -> Result<(), ApiError> {
    if let Err(e) = state.db.dismiss_knock(actor_id, peer_id).await {
        tracing::error!("dismiss_knock error: {e}");
        return Err(ApiError::internal("storage error"));
    }
    if let Err(e) = state.db.delete_knock_notification(actor_id, peer_id).await {
        tracing::warn!("delete_knock_notification after dismiss: {e}");
    }

    if let Err(e) = state.db.delete_contact(actor_id, peer_id).await {
        tracing::warn!("delete_contact after dismiss: {e}");
    }

    Ok(())
}

// ── fauna.knocks.list ──────────────────────────────────────────

fn knocks_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.knocks.list", "knocks").await?;
            // Decode for the `extra` forward-compat envelope (the list
            // request carries no params besides the connection actor).
            let _req: KnockListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .poll_knocks(&actor_id)
                .await
                .map_err(knocks_internal)?;

            let knocks: Vec<KnockItem> = rows
                .into_iter()
                .map(|k| KnockItem {
                    id: k.id,
                    // hex of the raw [u8; 32] — the HTTP twin's `sender`.
                    sender: hex::encode(k.sender_id),
                    // lossy-UTF-8 of the raw node bytes — the twin's
                    // `sender_node`.
                    sender_node: String::from_utf8_lossy(&k.sender_node).into_owned(),
                    summary: k.summary,
                    created_at: k.created_at,
                    extra: std::collections::BTreeMap::new(),
                })
                .collect();

            encode_reply(
                &KnockListReply {
                    knocks,
                    extra: std::collections::BTreeMap::new(),
                },
                knocks_internal,
            )
        })
    })
}

// ── fauna.knocks.accept ────────────────────────────────────────

fn knocks_accept_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.knocks.accept", "knocks").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "knocks")?;

            // Family-safety contact-approval gate (family-safety.md § Guardian
            // policy pillar 1): under contact_approval, acceptance authority
            // for the ward's new contact edges moves to the guardian — the
            // knock stays pending here and is decided via
            // `fauna.family.approvals.decide`.
            deny_if_contact_approval(&state, &actor_id).await?;

            accept_contact_core(&state, &actor_id, &peer_id)
                .await
                .map_err(|e| knocks_internal(e.message))?;

            encode_reply(
                &KnockActionReply {
                    extra: std::collections::BTreeMap::new(),
                },
                knocks_internal,
            )
        })
    })
}

// ── fauna.knocks.block ─────────────────────────────────────────

fn knocks_block_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.knocks.block", "knocks").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "knocks")?;

            block_contact_core(&state, &actor_id, &peer_id)
                .await
                .map_err(|e| knocks_internal(e.message))?;

            encode_reply(
                &KnockActionReply {
                    extra: std::collections::BTreeMap::new(),
                },
                knocks_internal,
            )
        })
    })
}

// ── fauna.knocks.unblock ───────────────────────────────────────

fn knocks_unblock_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.knocks.unblock", "knocks").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "knocks")?;

            // A ward may not undo a block their guardian decided.
            deny_if_contact_approval(&state, &actor_id).await?;

            // Single DB call — `unblock_contact` IS the shared core (a guarded
            // `DELETE` that clears the edge only when it is `blocked`; see
            // `db::contacts`). No knock to dismiss and no spam-train inverse,
            // unlike `block` — so this is a direct-call path like
            // `contacts.confirm`, not a multi-DB-call core.
            state
                .db
                .unblock_contact(&actor_id, &peer_id)
                .await
                .map_err(knocks_internal)?;

            encode_reply(
                &KnockActionReply {
                    extra: std::collections::BTreeMap::new(),
                },
                knocks_internal,
            )
        })
    })
}

// ── fauna.knocks.dismiss ───────────────────────────────────────

fn knocks_dismiss_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.knocks.dismiss", "knocks").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "knocks")?;

            // A ward may not remove an incoming knock from the guardian's
            // approvals queue before the guardian sees it.
            deny_if_contact_approval(&state, &actor_id).await?;

            dismiss_knock_core(&state, &actor_id, &peer_id)
                .await
                .map_err(|e| knocks_internal(e.message))?;

            encode_reply(
                &KnockActionReply {
                    extra: std::collections::BTreeMap::new(),
                },
                knocks_internal,
            )
        })
    })
}

// ── fauna.contacts.list ────────────────────────────────────────

fn contacts_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.contacts.list", "contacts").await?;
            let _req: ContactListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_contacts_full(&actor_id)
                .await
                .map_err(contacts_internal)?;

            // Enrich each row with the peer's public handle + domain, joined
            // nest-side from the peer's Profile (`docs/goal/ui/contacts.md`
            // § State & data shape). `ContactRow.handle` is `Some(_)` iff the
            // peer is a local user (has a `users` row); `None` ⇒ a federated
            // peer (the nest holds no cached federated Profile). The domain is
            // this nest's handle domain — the same source `resolve_handle_core`
            // reports for a handle — and is meaningful only for a local peer.
            // `handle_domain()` (the live identity domain — the primary `mail_domains`
            // projection set at claim) is the same source `resolve_handle_core`
            // reports, so a domainless-then-claimed box labels a local peer with the
            // claimed domain, not the stale `--handle-domain` seed (which left it
            // `None`). mail-multidomain.md § Multi-domain handles.
            let nest_domain: Option<String> = Some(state.handle_domain());
            let contacts: Vec<ContactItem> = rows
                .into_iter()
                .map(|c| {
                    let is_local = c.handle.is_some();
                    // An empty handle on a local user is "no usable handle".
                    let handle = c.handle.filter(|h| !h.is_empty());
                    let domain = if is_local { nest_domain.clone() } else { None };
                    ContactItem {
                        // hex of the raw peer bytes — the HTTP twin's `peer_id`.
                        peer_id: hex::encode(&c.peer_id),
                        status: c.status,
                        accepted_at: c.accepted_at,
                        created_at: c.created_at,
                        handle,
                        domain,
                        extra: std::collections::BTreeMap::new(),
                    }
                })
                .collect();

            encode_reply(
                &ContactListReply {
                    contacts,
                    extra: std::collections::BTreeMap::new(),
                },
                contacts_internal,
            )
        })
    })
}

// ── fauna.contacts.confirm ─────────────────────────────────────

fn contacts_confirm_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.contacts.confirm", "contacts").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "contacts")?;

            // Single DB call — the method IS the shared core (the HTTP twin
            // calls `promote_to_confirmed` directly too).
            state
                .db
                .promote_to_confirmed(&actor_id, &peer_id)
                .await
                .map_err(contacts_internal)?;

            encode_reply(
                &ContactConfirmReply {
                    extra: std::collections::BTreeMap::new(),
                },
                contacts_internal,
            )
        })
    })
}

// ── fauna.contacts.status ──────────────────────────────────────

/// Single-actor contact-status lookup — the caller's relationship to one
/// `peer_id`, the `fauna.contacts.list` roster read narrowed to one peer. The
/// folder recipient contact-gate reads this per un-acked folder Welcome to
/// decide auto/knock/suppress (`docs/goal/ui/folders.md` § Sharing) — O(1)
/// on the drain path vs. fetching the whole roster. Caller-scoped by the
/// connection `actor_id`; `status` is `None` when no contact edge exists (a
/// stranger), which the client's `contact_arrival_disposition` treats as a knock.
fn contacts_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.contacts.status", "contacts").await?;
            let req: KnockActionRequest = decode(&payload).map_err(malformed)?;
            let peer_id = parse_peer_id(&req.peer_id, "contacts")?;

            let status = state
                .db
                .get_contact_status(&actor_id, &peer_id)
                .await
                .map_err(contacts_internal)?;

            encode_reply(
                &ContactStatusReply {
                    status,
                    extra: std::collections::BTreeMap::new(),
                },
                contacts_internal,
            )
        })
    })
}

// ── fauna.inbox.mode.get ───────────────────────────────────────

fn inbox_mode_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.inbox.mode.get", "inbox").await?;
            let _req: InboxModeGetRequest = decode(&payload).map_err(malformed)?;

            // The DB method already defaults to "allow_knock" on a missing
            // row (it does not error); the HTTP twin additionally mapped any
            // error to the default. Mirror that here.
            let mode = state
                .db
                .get_inbox_mode(&actor_id)
                .await
                .unwrap_or_else(|_| "allow_knock".to_string());

            encode_reply(
                &InboxModeGetReply {
                    mode,
                    extra: std::collections::BTreeMap::new(),
                },
                invalid_inbox_mode,
            )
        })
    })
}

// ── fauna.inbox.mode.set ───────────────────────────────────────

fn inbox_mode_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.inbox.mode.set", "inbox").await?;
            let req: InboxModeSetRequest = decode(&payload).map_err(malformed)?;

            // The DB method validates the mode against the allowed set and
            // errors on a bad value — the HTTP twin mapped that to a `400`.
            // On WS-RPC it is an invalid-params error.
            state
                .db
                .set_inbox_mode(&actor_id, &req.mode)
                .await
                .map_err(invalid_inbox_mode)?;

            encode_reply(
                &InboxModeSetReply {
                    extra: std::collections::BTreeMap::new(),
                },
                invalid_inbox_mode,
            )
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_contacts_handlers(b: &mut RpcRouterBuilder) {
    // Per-kind replay semantics + rationale: see
    // `KindRegistry::register_contacts_kinds`. All ten kinds are
    // `forbid_replay = false` @5 s — the four reads
    // (knocks.list/contacts.list/contacts.status/inbox.mode.get) are pure, the
    // six writes (knocks.{accept,block,unblock,dismiss}/contacts.confirm/
    // inbox.mode.set) are idempotent and converge on re-issue (no
    // score-increment hazard like posts.interact).
    b.add(
        "fauna.knocks.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: knocks_list_handler(),
        },
    );
    b.add(
        "fauna.knocks.accept",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: knocks_accept_handler(),
        },
    );
    b.add(
        "fauna.knocks.block",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: knocks_block_handler(),
        },
    );
    b.add(
        "fauna.knocks.unblock",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: knocks_unblock_handler(),
        },
    );
    b.add(
        "fauna.knocks.dismiss",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: knocks_dismiss_handler(),
        },
    );
    b.add(
        "fauna.contacts.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: contacts_list_handler(),
        },
    );
    b.add(
        "fauna.contacts.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: contacts_status_handler(),
        },
    );
    b.add(
        "fauna.contacts.confirm",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: contacts_confirm_handler(),
        },
    );
    b.add(
        "fauna.inbox.mode.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: inbox_mode_get_handler(),
        },
    );
    b.add(
        "fauna.inbox.mode.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: inbox_mode_set_handler(),
        },
    );
}
