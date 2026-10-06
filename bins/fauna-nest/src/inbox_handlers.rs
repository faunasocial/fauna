//! WS-RPC handlers for the **fauna-native inbox** delivery queue —
//! `fauna.inbox.fetch` + `fauna.inbox.ack`.
//!
//! The inbox is the durable per-actor store-and-forward queue for
//! fauna-native social / federation payloads (contact-requests, knocks,
//! group invites/messages, MLS Welcomes, security notices, cross-nest
//! DMs). Delivery fires the best-effort `PushEvent::InboxItem` push
//! (`routes::deliver_to_inbox`); these kinds are the **durable backstop**
//! a client drains on connect / after a missed push.
//!
//! These replaced the HTTP `GET /api/v1/inbox/{actor_id}` drain — the
//! `routes::get_inbox` handler, **deleted 2026-06-09** in the
//! WS-RPC-everywhere rip — and fix its latent data-loss bug: that handler
//! marked items delivered the instant they were *read* (it called
//! `mark_delivered` on every poll), so a client that crashed before
//! applying them dropped them. Here `fetch`
//! is a pure peek of undelivered items (no status change) and the client
//! explicitly `ack`s the ids it has durably applied — at-least-once
//! delivery with the client in control of consume.
//!
//! Both kinds are **caller-scoped by construction**: the handler reads the
//! connection's own `actor_id` (no target param), so a caller can only
//! ever drain its own queue. `ack` additionally scopes its `UPDATE` to the
//! caller (`CacheDb::ack_inbox`), so a guessed link id can't ack another
//! actor's item. Permission gate `User` (Admin inherits) via
//! `bridge_method_allowlist`.
//!
//! Kind metadata mirrors `kind.rs::register_inbox_kinds`: `fetch` and `ack` are
//! `forbid_replay=false` @5 s (small control frames, idempotent ops), while
//! `send` is `forbid_replay=true` @30 s — it is **not** idempotent and cannot
//! be, because its `content_id` mixes a timestamp and a monotonic nonce (see
//! the registration below).

use std::time::Duration;

use bytes::Bytes;
use fauna_protocol::{
    RpcError, decode_strict as decode,
    inbox::{
        InboxAckReply, InboxAckRequest, InboxFetchReply, InboxFetchRequest, InboxItem,
        InboxSendReply, InboxSendRequest,
    },
};

use crate::federation_pool::originate_inbox_deliver;
use crate::routes::{
    ArrivalOrigin, InboxDeliveryOutcome, InboxRejection, InboxRejectionDisposition,
    deliver_inbox_payload_core, inbox_payload_sender, parse_actor_id,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Default page size when the request leaves `limit` at 0.
const INBOX_FETCH_DEFAULT_LIMIT: u32 = 100;
/// Hard cap on a single page (control frames are small, but bound it).
const INBOX_FETCH_MAX_LIMIT: u32 = 500;

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("inbox", reason)
}

fn forbidden(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::forbidden_ns("inbox", reason)
}

/// Map a local-delivery [`InboxRejection`] onto a client-facing `RpcError` — a
/// pure namespace/code-family adapter over [`InboxRejection::disposition`],
/// the single shared statement of which bucket each variant falls into
/// (mirrored by `federation_handlers::map_inbox_rejection`). This leg answers
/// `Forbidden` with the **inbox**-namespaced code, but `MalformedClass` with
/// the shared, un-namespaced `fauna.protocol.malformed` — the client's generic
/// "your payload was bad" code, not an inbox-specific one; `Internal` is the
/// shared `fauna.protocol.internal`, identical on both legs.
fn rejection_to_error(r: InboxRejection) -> RpcError {
    use InboxRejectionDisposition::{Capacity, Forbidden, Internal, MalformedClass};
    match r.disposition() {
        Forbidden(m) => forbidden(m),
        MalformedClass(m) => malformed(m),
        Capacity => crate::rpc_errors::rate_limited(),
        Internal(m) => internal(m),
    }
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// `fauna.inbox.fetch` — return the caller's undelivered inbox items
/// without changing their status (a peek-drain).
fn fetch_handler() -> RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.inbox.fetch").await?;
            let req: InboxFetchRequest = decode(&payload).map_err(malformed)?;
            let limit = match req.limit {
                0 => INBOX_FETCH_DEFAULT_LIMIT,
                n => n.min(INBOX_FETCH_MAX_LIMIT),
            } as usize;

            // Undelivered rows, oldest first (link-id order), from the caller's
            // skip cursor. No status change — skipping is not delivery, so a
            // stepped-over row is returned again by the next cursor-less fetch.
            let mut rows = state
                .db
                .poll_inbox_after(&actor_id, req.after_id)
                .await
                .map_err(internal)?;
            let more = rows.len() > limit;
            rows.truncate(limit);

            let mut items: Vec<InboxItem> = Vec::with_capacity(rows.len());
            for (id, payload, blob_hash) in &rows {
                // Resolve blob-backed payloads to their full bytes, exactly
                // as the (deleted) HTTP `get_inbox` did (mode-aware payload store).
                let resolved = if let Some(ps) = &state.payload_store {
                    ps.resolve_payload(payload, blob_hash.as_deref())
                        .await
                        .unwrap_or_else(|_| payload.clone())
                } else {
                    payload.clone()
                };
                items.push(InboxItem {
                    id: *id,
                    payload: resolved,
                    extra: Default::default(),
                });
            }

            encode_reply(&InboxFetchReply {
                items,
                more,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.inbox.ack` — mark the caller's applied items delivered so a
/// re-fetch no longer returns them.
fn ack_handler() -> RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.inbox.ack").await?;
            let req: InboxAckRequest = decode(&payload).map_err(malformed)?;
            let acked = state
                .db
                .ack_inbox(&actor_id, &req.ids)
                .await
                .map_err(internal)? as u32;
            encode_reply(&InboxAckReply {
                acked,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.inbox.send` — the client→home-nest bearer leg of fauna-native social
/// inbox delivery (the outbound counterpart of `fetch`/`ack`). The authed caller
/// hands its **home** nest a signed `(ContactRequest, Post)` tuple + the
/// recipient (and the recipient's home-nest URL when cross-nest); the home nest
/// does local-deliver-or-originate. Replaces the unauthenticated
/// `POST /api/v1/inbox/{actor}` HTTP twin — clients reach remote actors *through*
/// their home nest under Spec Y2, never by POSTing the remote nest directly.
/// Mirrors the `fauna.events.remote_rsvp` → home-nest-originates precedent
/// (`federation.md` § Federation residue surface).
fn send_handler() -> RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.inbox.send").await?;
            let req: InboxSendRequest = decode(&payload).map_err(malformed)?;
            let recipient = parse_actor_id(&req.recipient_actor_id)
                .ok_or_else(|| malformed("invalid recipient_actor_id hex"))?;

            // Sender-binding: an authed actor may only ever send *as itself*.
            // The unauthenticated HTTP twin and the nest↔nest federation leg
            // cannot enforce this (no authenticated caller identity); the authed
            // per-actor WS can. Done here, NOT in the transport-agnostic core.
            let sender = inbox_payload_sender(&req.payload_bytes).map_err(malformed)?;
            if sender != actor_id {
                return Err(permission_denied(
                    "caller is not the payload sender (cr.sender != caller)",
                ));
            }

            // Family-safety outbound reach gate (family-safety.md § Guardian
            // policy pillar 1): under contact_approval, a supervised sender
            // may only initiate to accepted/confirmed contacts — the guardian
            // pre-approves new parties via `fauna.family.contact.add`.
            if let Some(policy) = state
                .db
                .get_guardian_policy(&actor_id)
                .await
                .map_err(|e| internal(format!("guardian policy: {e}")))?
                && policy.contact_approval
            {
                let status = state
                    .db
                    .get_contact_status(&actor_id, &recipient)
                    .await
                    .map_err(|e| internal(format!("contact status: {e}")))?;
                if !matches!(status.as_deref(), Some("accepted") | Some("confirmed")) {
                    return Err(crate::rpc_errors::guardian_approval_required_ns(
                        "inbox",
                        "this account can only message approved contacts — ask your guardian",
                    ));
                }
            }

            let body = Bytes::from(req.payload_bytes);
            let inbox_id = match req.recipient_nest_url.as_deref().filter(|u| !u.is_empty()) {
                // Cross-nest: relay to the recipient's home nest over the
                // `fauna.federation.inbox.deliver` channel (idempotent — inbox
                // dedup). A peer policy rejection surfaces as a dial error →
                // `internal`, matching the welcome.deliver precedent
                // (`conversations_handlers`) — the federation reply carries only
                // an opaque code, not the InboxMode bucket.
                Some(peer_url) => originate_inbox_deliver(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &req.recipient_actor_id,
                    body.as_ref(),
                )
                .await
                .map_err(|e| internal(format!("cross-nest inbox delivery: {e}")))?,
                // Same nest: deliver locally — full signature verification +
                // `InboxMode` routing, identical to the retiring HTTP twin.
                None => {
                    match deliver_inbox_payload_core(
                        &state,
                        &recipient,
                        &body,
                        ArrivalOrigin::Local,
                    )
                    .await
                    {
                        InboxDeliveryOutcome::Delivered(row_id) => Some(row_id),
                        InboxDeliveryOutcome::KnockStored => None,
                        InboxDeliveryOutcome::Rejected(r) => return Err(rejection_to_error(r)),
                    }
                }
            };
            encode_reply(&InboxSendReply {
                inbox_id,
                extra: Default::default(),
            })
        })
    })
}

/// Register the fauna-native inbox handlers (`fetch`/`ack` drain + `send`).
pub fn register_inbox_handlers(b: &mut RpcRouterBuilder) {
    let quick = || Duration::from_secs(5);
    b.add(
        "fauna.inbox.fetch",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: fetch_handler(),
        },
    );
    b.add(
        "fauna.inbox.ack",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: ack_handler(),
        },
    );
    // The cross-nest send leg gets the longer 30 s federation-dial deadline
    // (matches `kind.rs::register_inbox_kinds` + the sibling
    // `fauna.events.{remote_rsvp,invite}` cross-nest kinds).
    b.add(
        "fauna.inbox.send",
        RpcKindMeta {
            // NOT idempotent, and unable to become so: the local leg's
            // `push_inbox_with_quota` derives its `content_id` from
            // `inbox_content_id(recipient, now_millis, nonce, payload)`, which
            // mixes in BOTH a timestamp and a monotonic `INBOX_NONCE` — so the
            // key is unique *by construction* on every call and no column can
            // ever dedup it. A replay therefore inserts a second delivery row
            // AND adds `size` to the recipient's `inbox_bytes_used` a second
            // time (`db/inbox.rs`), permanently inflating a quota the user
            // never spent. The previous `false` cited "the federation leg is
            // idempotent (idempotency cache)" — that ground is FALSE: the cache
            // lives on `RpcConnection` and `request_auto_retry` waits for the
            // reconnect, so it cannot catch a retry (`transport.md`
            // § Idempotency and reconnect-with-resume). "At-least-once
            // semantics" describes a non-idempotent handler, which that section
            // requires be `true`. Standing evidence:
            // `db::tests::sending_twice_double_charges_the_recipients_quota`.
            // Mirror any change in `KindRegistry::register_inbox_kinds`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: send_handler(),
        },
    );
}

#[cfg(test)]
mod rejection_to_error_tests {
    use super::{InboxRejection, rejection_to_error};

    /// Pins the wire code `rejection_to_error` emits for every
    /// [`InboxRejection`] variant — the client leg's half of the shared
    /// bucketing in `InboxRejection::disposition`. `federation_handlers`
    /// carries the sibling pin (`map_inbox_rejection_tests`) for the same
    /// variants on the federation leg; the two must keep disagreeing only on
    /// namespace/code-family, never on which variant lands in which family.
    #[test]
    fn every_variant_keeps_its_wire_code() {
        let cases: Vec<(InboxRejection, &str)> = vec![
            (
                InboxRejection::BadPayload("x".into()),
                "fauna.protocol.malformed",
            ),
            (InboxRejection::Blocked, "fauna.inbox.forbidden"),
            (InboxRejection::InboxClosed, "fauna.inbox.forbidden"),
            (InboxRejection::ContactsOnly, "fauna.inbox.forbidden"),
            (InboxRejection::KnockPending, "fauna.protocol.malformed"),
            (InboxRejection::UnknownMode, "fauna.inbox.forbidden"),
            (
                InboxRejection::QuotaForbidden("x".into()),
                "fauna.inbox.forbidden",
            ),
            (InboxRejection::RecipientUnknown, "fauna.inbox.forbidden"),
            (
                InboxRejection::QuotaTooLarge("x".into()),
                "fauna.protocol.malformed",
            ),
            (
                InboxRejection::KnockTooLarge("x".into()),
                "fauna.protocol.malformed",
            ),
            (
                InboxRejection::KnockQueueFull,
                "fauna.protocol.rate_limited",
            ),
            (InboxRejection::Storage, "fauna.protocol.internal"),
        ];
        for (rejection, want_code) in cases {
            let debug = format!("{rejection:?}");
            let got = rejection_to_error(rejection);
            assert_eq!(got.code, want_code, "{debug} wire code changed");
        }
    }
}
