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
        ListPrincipalsReply, ListPrincipalsRequest, PrincipalInfo, RevokePrincipalReply,
        RevokePrincipalRequest,
    },
};
use serde_bytes::ByteBuf;

use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::rpc_errors::{encode_reply, internal, malformed};
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
            let principals = state
                .db
                .list_third_party_principals(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|p| PrincipalInfo {
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
        "fauna.principals.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: revoke_handler(),
        },
    );
}
