//! WS-RPC handlers for the third-party principal roster (`fauna.principals.*`),
//! per `docs/goal/architecture/third-party.md` § The principal model.
//!
//! USER class and self-scoped: the actor is the authenticated connection's,
//! never a request field, so an account lists and ends only its own
//! principals. The row itself is minted by the consent, not here —
//! `/oauth/token`'s code redemption writes it in the grant's own transaction
//! (`db::third_party_principals`).

use fauna_protocol::{
    decode_strict as decode,
    principals::{
        ListPrincipalOpsReply, ListPrincipalOpsRequest, ListPrincipalsReply, ListPrincipalsRequest,
        PRINCIPAL_OP_DETAIL_MAX, PRINCIPAL_OPS_PAGE, PRINCIPAL_OPS_RECORD_BATCH, PrincipalInfo,
        RecordPrincipalOpsReply, RecordPrincipalOpsRequest, RecordedPrincipalOp, RecordedWindow,
        RevokePrincipalReply, RevokePrincipalRequest,
    },
};
use serde_bytes::ByteBuf;

use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::rpc_errors::{encode_reply, internal, invalid_params_ns, malformed, not_found_ns};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// `fauna.principals.list` — the roster read the connected-apps page composes
/// from. Deliberately not gated on the external-apps kill-switch: a disabled
/// account must still SEE and end what it once approved (the switch suspends
/// the plane; it never hides the audit surface), exactly as `list_grants`.
fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.principals.list").await?;
            let _req: ListPrincipalsRequest = decode(&payload).map_err(malformed)?;
            let now = u64::try_from(fauna_core::data::Timestamp::now_secs()).unwrap_or(0);
            let mut oracle = state
                .db
                .principal_oracle_summaries(&actor_id, now)
                .await
                .map_err(internal)?;
            let principals = state
                .db
                .list_third_party_principals(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|p| PrincipalInfo {
                    oracle: oracle.remove(&p.principal_id),
                    principal_id: p.principal_id,
                    client_id: p.client_id,
                    holder_x25519: p.holder_x25519.map(ByteBuf::from),
                    execution_form: p.execution_form,
                    declared_kinds: p.declared_kinds,
                    granted_scopes: p.granted_scopes,
                    created_at: p.created_at,
                    last_used_at: p.last_used_at,
                    label: p.label,
                    publisher_key: p.publisher_key.map(ByteBuf::from),
                    writer_ed25519: p.writer_ed25519.map(ByteBuf::from),
                    live_grants: p.live_grants,
                    bridge: p.declared_bridge,
                    service_auth: p.declared_service_auth,
                    activitypub_peers: p.declared_activitypub_peers,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListPrincipalsReply {
                principals,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.principals.ops` — one principal's oracle record, newest first, at
/// most [`PRINCIPAL_OPS_PAGE`] rows: the per-operation half of the audit split
/// (`key-material-hierarchy.md` § Audience: deployment infrastructure → *The
/// oracle*), which only the custodian can observe — so this is the one nest
/// read for operations, as the bunker roster's counters are for its apps. Not
/// gated on the kill-switch, as the roster read is not.
fn ops_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.principals.ops").await?;
            let req: ListPrincipalOpsRequest = decode(&payload).map_err(malformed)?;
            let ops = state
                .db
                .list_principal_oracle_ops(&actor_id, &req.principal_id, PRINCIPAL_OPS_PAGE)
                .await
                .map_err(internal)?;
            encode_reply(&ListPrincipalOpsReply {
                ops,
                extra: Default::default(),
            })
        })
    })
}

/// How far ahead of the nest's clock a recorded operation's `at` may sit —
/// device clocks drift; a row from the future beyond this is refused.
const RECORD_CLOCK_SKEW_SECS: i64 = 300;

/// Why a submitted ceiling position is no custodian's window, or `None`: it
/// cannot open in the future beyond the skew, nor hold more than the ceiling
/// admits.
fn window_defect(window: &RecordedWindow, now: i64) -> Option<String> {
    let start = i64::try_from(window.window_start).unwrap_or(i64::MAX);
    if start > now.saturating_add(RECORD_CLOCK_SKEW_SECS) {
        return Some("window_start is not a time the window could have opened".into());
    }
    if window.count > fauna_core::identity_op::OPS_PER_WINDOW {
        return Some(format!(
            "a window admits at most {} operations",
            fauna_core::identity_op::OPS_PER_WINDOW
        ));
    }
    None
}

/// Why a submitted row is not a device custodian's record, or `None`.
fn record_row_defect(op: &RecordedPrincipalOp, now: i64) -> Option<String> {
    if op.op_id.len() != 16 {
        return Some("op_id must be 16 bytes".into());
    }
    if op.detail.is_empty() || op.detail.len() > PRINCIPAL_OP_DETAIL_MAX {
        return Some(format!(
            "detail must be 1..={PRINCIPAL_OP_DETAIL_MAX} bytes"
        ));
    }
    if op.at <= 0 || op.at > now.saturating_add(RECORD_CLOCK_SKEW_SECS) {
        return Some("at is not a time the operation could have run".into());
    }
    match fauna_core::identity_op::IdentityOpClass::parse(&op.class) {
        Ok(class) if class.custodian().is_device() => None,
        // A nest or bridge custodian records its own operations where it
        // performs them; an app submitting one would forge that record.
        Ok(_) => Some(format!("{} is not a device custodian's class", op.class)),
        Err(e) => Some(format!("{}: {e}", op.class)),
    }
}

/// `fauna.principals.ops.record` — a **device** custodian's submission of the
/// operations it performed for one of the account's own principals
/// (`conversations-oracle.md` § The per-operation record): the signed-in app
/// hosting the conversations engine performs on the device, so its record
/// reaches the account's other apps here, and `fauna.principals.ops` merges it
/// as a second custodian's. Self-scoped: a principal that is not the caller's
/// own is `not_found`, nothing written. Only a device class is accepted — the
/// nest's and the bridges' custodians record where they perform. A resubmitted
/// `op_id` keeps its one row, so the custodian's retry converges.
fn record_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            const NS: &str = "fauna.principals.ops.record";
            require_permission(&state, &actor_id, NS).await?;
            let req: RecordPrincipalOpsRequest = decode(&payload).map_err(malformed)?;
            if req.ops.len() > PRINCIPAL_OPS_RECORD_BATCH {
                return Err(invalid_params_ns(
                    NS,
                    format!("at most {PRINCIPAL_OPS_RECORD_BATCH} operations per request"),
                ));
            }
            let now = fauna_core::data::Timestamp::now_secs();
            if let Some(defect) = req.ops.iter().find_map(|op| record_row_defect(op, now)) {
                return Err(invalid_params_ns(NS, defect));
            }
            if let Some(defect) = req.window.as_ref().and_then(|w| window_defect(w, now)) {
                return Err(invalid_params_ns(NS, defect));
            }
            let Some(recorded) = state
                .db
                .record_device_principal_ops(
                    &actor_id,
                    &req.principal_id,
                    &req.ops,
                    req.window.as_ref(),
                )
                .await
                .map_err(internal)?
            else {
                return Err(not_found_ns(NS, "no such principal on this account"));
            };
            encode_reply(&RecordPrincipalOpsReply {
                recorded,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.principals.revoke` — **the one verb** (rule 4): the roster row, every
/// grant family, every capability grant held by the principal's key, in one
/// transaction.
///
/// An unknown id answers `revoked: false`, not an error: the end state the
/// caller asked for — no such principal — holds either way, and a retry after a
/// lost reply must converge rather than fail.
fn revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.principals.revoke").await?;
            let req: RevokePrincipalRequest = decode(&payload).map_err(malformed)?;
            let ended = state
                .db
                .revoke_third_party_principal(&actor_id, &req.principal_id)
                .await
                .map_err(internal)?;
            // Rows first, then the sweep: the transaction above has committed,
            // so a principal session racing this revoke either registered in
            // time to be swept here or re-reads a row that is already gone
            // (`principal_session::register_principal_connection`). Every
            // dispatch on a live session re-reads the row too — the sweep is
            // for the socket itself, which would otherwise idle on, a Push
            // recipient once the event door exists (`transport-connection.md`
            // § *The principal session* → *Revocation — three doors*, (a)).
            state.ws.disconnect_principal(&actor_id, &req.principal_id);
            let Some(ended) = ended else {
                return encode_reply(&RevokePrincipalReply {
                    revoked: false,
                    ..Default::default()
                });
            };
            // After the commit, never between the endings: the same one nudge
            // per actor every grant ending sends, so a PDS bridge's cached
            // session state does not lag the revocation until its next poll.
            if ended.grants_ended > 0 {
                crate::bridge_atproto_handlers::notify_atproto_sessions_changed(
                    &state, &actor_id, None,
                )
                .await;
            }
            // The owner's own capability revoke wakes the delegation runner
            // because a revoke may take the nest off a delegated kind; ending
            // grants by holder is the same act, so it owes the same wake.
            if ended.capability_grants_ended > 0 {
                state.delegation_runner_wake.notify_one();
            }
            // The bridged rooms it served: their undrained outbox is gone and
            // the Sent rows say so — the same nudge every room change sends,
            // so the thread re-reads (`bridged_conversation_handlers`).
            for room_id in &ended.outbox_rooms {
                state.ws.notify_push(
                    &actor_id,
                    fauna_protocol::PushEvent::BridgeConversationChanged(
                        fauna_protocol::bridged_conversations::ConversationChangedPush {
                            room_id: room_id.clone(),
                            extra: Default::default(),
                        },
                    ),
                );
            }
            encode_reply(&RevokePrincipalReply {
                revoked: true,
                grants_ended: ended.grants_ended,
                capability_grants_ended: ended.capability_grants_ended,
                extra: Default::default(),
            })
        })
    })
}

pub fn register_principals_handlers(b: &mut RpcRouterBuilder) {
    let fetch = std::time::Duration::from_secs(5);
    b.add(
        "fauna.principals.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.principals.ops",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: ops_handler(),
        },
    );
    b.add(
        "fauna.principals.ops.record",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: record_handler(),
        },
    );
    b.add(
        "fauna.principals.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: revoke_handler(),
        },
    );
}
