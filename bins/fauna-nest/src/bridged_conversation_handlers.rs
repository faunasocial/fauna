//! `fauna.bridges.conversation.*` — the bridged-conversation family, Phase G
//! (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue → Phase G
//! owns the kinds and their caller classes; `docs/goal/ui/conversations.md`
//! § Where logic lives → *The `Bridged` adapter* owns every payload's
//! semantics; the floor roster is `docs/goal/behavior/conversation-rooms.md`
//! § Bridged rooms').
//!
//! **The bridge's six** are `ThirdParty`-only: their actor-router entries carry
//! the wire contract and refuse at the central gate, always, and each is served
//! by a principal handler here, reached through `principal_handlers`' gate
//! (ceiling, then the `fauna:conversations:bridge` arm). A principal whose
//! consented document declares no `bridge` block, or attested no key to seal
//! outbound items to, holds no conversation door. **The user's four** are
//! `User`-class and caller-scoped.
//!
//! **A room outlives its bridge** (`apps/bridges.md` § Phase G → *When the
//! bridge stops serving*): with no live principal declaring its bridge id a
//! room lists `disconnected`, reads as ever, and refuses `send` with the typed
//! `bridge_disconnected`; a principal that comes to declare the id adopts it
//! at consent (`db::bridged_conversations`).
//!
//! **The family gate** (`docs/goal/behavior/family-safety.md` § The bridge-DM
//! gate → *Enforcement points*): a deposit composes the verdict keyed
//! `(bridge_id, sender)` — the sender the authenticated bridge asserts for its
//! own network — before anything is stored, and a `block`ed peer's deposit is
//! refused; a send to a `block`ed peer is refused and every other send seeds
//! the `allow` row. Hold-ness is computed at read ([`crate::bridge_dm_gate`]).

use std::time::Duration;

use fauna_protocol::bridged_conversations::{
    BRIDGED_PAGE_DEFAULT, BRIDGED_PAGE_MAX, BridgedAck, BridgedMessageInfo, BridgedRoomInfo,
    ConversationChangedPush, DepositReply, DepositRequest, InboxFetchReply, InboxFetchRequest,
    KIND_DEPOSIT, KIND_INBOX_FETCH, KIND_OUTBOX_ACK, KIND_OUTBOX_FETCH, KIND_RECEIPT,
    KIND_ROOM_MEMBERS, KIND_ROOM_UPSERT, KIND_ROOMS_LIST, KIND_ROOMS_OPEN, KIND_SEND,
    MAX_BRIDGED_FAR_ID_BYTES, MAX_BRIDGED_PARTICIPANTS, MAX_BRIDGED_SEALED_BYTES, OutboxAckReply,
    OutboxAckRequest, OutboxFetchReply, OutboxFetchRequest, OutboxItem, RECEIPT_DELIVERED,
    RECEIPT_READ, ReceiptRequest, RoomMembersRequest, RoomUpsertReply, RoomUpsertRequest,
    RoomsListReply, RoomsListRequest, RoomsOpenReply, RoomsOpenRequest, SendReply, SendRequest,
};
use fauna_protocol::kind_manifest::{BridgeBlock, BridgeCapabilityValue};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::bridge_dm_gate::{GateContext, room_guardian_state};
use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::db::bridged_conversations::{
    BridgeSeat, BridgedRefused, BridgedRoomRow, Deposited, RoomShape,
};
use crate::principal_handlers::{PrincipalCaller, PrincipalHandler};
use crate::routes::AppState;
use crate::rpc_errors::{
    coded_ns, encode_reply, guardian_approval_required_ns, internal, invalid_params_ns, malformed,
    not_found_ns, permission_denied_ns,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The error family every refusal here answers in.
const NS: &str = "bridges";

/// One consented conversation bridge on an account's roster — what a room is
/// born on and what a principal call acts as.
struct ServingBridge {
    principal_id: Vec<u8>,
    label: String,
    block: BridgeBlock,
    x25519: [u8; 32],
}

/// The account's consented bridges that can carry a conversation: a declared
/// `bridge` block and an attested key to seal outbound items to — and the
/// first-party in-process legs that serve the account
/// ([`crate::bridge_legs::serving`]: the Nostr, Bluesky and ActivityPub DM
/// legs, each where the account has that network linked).
async fn serving_bridges(
    state: &AppState,
    account: &[u8; 32],
) -> Result<Vec<ServingBridge>, RpcError> {
    let mut bridges: Vec<ServingBridge> = state
        .db
        .list_third_party_principals(account)
        .await
        .map_err(internal)?
        .into_iter()
        .filter_map(|p| {
            let block = p.declared_bridge?;
            let x25519 = <[u8; 32]>::try_from(p.holder_x25519?.as_slice()).ok()?;
            Some(ServingBridge {
                label: p.label.unwrap_or_else(|| block.id.clone()),
                principal_id: p.principal_id,
                block,
                x25519,
            })
        })
        .collect();
    bridges.extend(
        crate::bridge_legs::serving(&state.db, account)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|leg| ServingBridge {
                principal_id: leg.principal_id,
                label: leg.label,
                block: leg.block,
                x25519: leg.x25519,
            }),
    );
    Ok(bridges)
}

/// The far room id `address` names on `bridge`: the far network's own
/// spelling, except on a first-party leg that keys its rooms on a canonical
/// form (the Nostr leg's hex, whichever spelling names the key — the key its
/// ingest and the gate use).
fn far_room_for(bridge: &ServingBridge, address: &str) -> String {
    crate::bridge_legs::far_room_id(&bridge.principal_id, address)
        .unwrap_or_else(|| address.to_string())
}

/// The calling principal as a conversation bridge, or the refusal.
async fn caller_bridge(
    state: &AppState,
    caller: &PrincipalCaller,
) -> Result<ServingBridge, RpcError> {
    serving_bridges(state, &caller.account)
        .await?
        .into_iter()
        .find(|b| b.principal_id == caller.principal_id)
        .ok_or_else(|| {
            permission_denied_ns(
                NS,
                "this app's document declares no conversation bridge, or it attested no key",
            )
        })
}

fn far_id(field: &str, value: &str) -> Result<(), RpcError> {
    if value.is_empty() || value.len() > MAX_BRIDGED_FAR_ID_BYTES {
        return Err(invalid_params_ns(
            NS,
            format!("{field} must be 1..={MAX_BRIDGED_FAR_ID_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn participants_ok(participants: &[String]) -> Result<(), RpcError> {
    if participants.len() > MAX_BRIDGED_PARTICIPANTS {
        return Err(invalid_params_ns(
            NS,
            format!("at most {MAX_BRIDGED_PARTICIPANTS} participants"),
        ));
    }
    participants
        .iter()
        .try_for_each(|p| far_id("participant", p))
}

fn sealed_ok(field: &str, bytes: &[u8]) -> Result<(), RpcError> {
    if bytes.is_empty() || bytes.len() > MAX_BRIDGED_SEALED_BYTES {
        return Err(invalid_params_ns(
            NS,
            format!("{field} must be 1..={MAX_BRIDGED_SEALED_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn page(limit: u32) -> u32 {
    match limit {
        0 => BRIDGED_PAGE_DEFAULT,
        n => n.min(BRIDGED_PAGE_MAX),
    }
}

/// Map the store's typed refusals; anything else is a fault.
fn store_error(e: anyhow::Error) -> RpcError {
    match e.downcast_ref::<BridgedRefused>() {
        Some(BridgedRefused::NoSuchRoom) => not_found_ns(NS, "no such bridged room"),
        Some(BridgedRefused::Full) => coded_ns(
            NS,
            "conversation_store_full",
            "this bridge's conversation store is full",
        ),
        None => internal(e),
    }
}

/// The nudge to the account's own clients.
pub(crate) fn notify_changed(state: &AppState, account: &[u8; 32], room_id: &[u8]) {
    state.ws.notify_push(
        account,
        fauna_protocol::PushEvent::BridgeConversationChanged(ConversationChangedPush {
            room_id: room_id.to_vec(),
            extra: Default::default(),
        }),
    );
}

fn room_info(
    row: BridgedRoomRow,
    bridge: Option<&ServingBridge>,
    gate: Option<&GateContext>,
) -> BridgedRoomInfo {
    let guardian_state = room_guardian_state(gate, &row.bridge_id, &row.participants);
    BridgedRoomInfo {
        room_id: row.room_id,
        bridge_label: bridge.map_or_else(|| row.bridge_id.clone(), |b| b.label.clone()),
        glyph: bridge.map_or_else(|| "bridge".to_string(), |b| b.block.glyph.clone()),
        bridge_id: row.bridge_id,
        far_room_id: row.far_room_id,
        label: row.label,
        participants: row.participants,
        self_address: row.self_address,
        // A vector this nest validated and wrote itself.
        capabilities: serde_json::from_str(&row.capabilities).unwrap_or_default(),
        // The serving bridge's key now, over the room's snapshot of it: a room
        // the Nostr leg's schema-118 move carried holds an empty snapshot until
        // the leg's next deposit.
        bridge_x25519: bridge.map_or(row.bridge_x25519, |b| b.x25519.to_vec()),
        last_at: row.last_at,
        guardian_state,
        disconnected: bridge.is_none(),
        extra: Default::default(),
    }
}

/// The typed refusal for a room no live principal serves.
fn bridge_disconnected() -> RpcError {
    coded_ns(
        NS,
        "bridge_disconnected",
        "no connected app serves this room's bridge; reconnect it to send",
    )
}

// ── the bridge's six (principal handlers) ──────────────────────────────

/// `conversation.room.upsert`.
pub(crate) fn principal_room_upsert_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: RoomUpsertRequest = decode(&payload).map_err(malformed)?;
            far_id("far_room_id", &req.far_room_id)?;
            participants_ok(&req.participants)?;
            let bridge = caller_bridge(&state, &caller).await?;
            let seat = BridgeSeat {
                principal_id: &bridge.principal_id,
                bridge: &bridge.block,
                bridge_x25519: &bridge.x25519,
            };
            let shape = RoomShape {
                label: req.label,
                participants: req.participants,
                self_address: req.self_address,
            };
            let (room, created) = state
                .db
                .upsert_bridged_room(&caller.account, &seat, &req.far_room_id, &shape)
                .await
                .map_err(store_error)?;
            notify_changed(&state, &caller.account, &room.room_id);
            encode_reply(&RoomUpsertReply {
                room_id: room.room_id,
                created,
                extra: Default::default(),
            })
        })
    })
}

/// `conversation.room.members` — refused where the declared vector says
/// membership change does not reach the bridge.
pub(crate) fn principal_room_members_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: RoomMembersRequest = decode(&payload).map_err(malformed)?;
            far_id("far_room_id", &req.far_room_id)?;
            participants_ok(&req.participants)?;
            let bridge = caller_bridge(&state, &caller).await?;
            if bridge.block.capabilities.get("supports_membership_change")
                != Some(&BridgeCapabilityValue::Flag(true))
            {
                return Err(permission_denied_ns(
                    NS,
                    "this bridge declares that membership change does not reach it",
                ));
            }
            if !state
                .db
                .set_bridged_room_participants(
                    &caller.account,
                    &bridge.principal_id,
                    &req.far_room_id,
                    &req.participants,
                )
                .await
                .map_err(internal)?
            {
                return Err(not_found_ns(NS, "no such bridged room"));
            }
            if let Some(room) = state
                .db
                .get_bridged_room_by_far_id(&caller.account, &bridge.principal_id, &req.far_room_id)
                .await
                .map_err(internal)?
            {
                notify_changed(&state, &caller.account, &room.room_id);
            }
            encode_reply(&BridgedAck::default())
        })
    })
}

/// `conversation.deposit` — the family gate, then the store.
pub(crate) fn principal_deposit_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: DepositRequest = decode(&payload).map_err(malformed)?;
            far_id("far_room_id", &req.far_room_id)?;
            far_id("far_message_id", &req.far_message_id)?;
            far_id("sender", &req.sender)?;
            sealed_ok("sealed_content", &req.sealed_content)?;
            let bridge = caller_bridge(&state, &caller).await?;
            // The gate keys on the sender the authenticated bridge asserts for
            // its own network; a `block`ed peer's new message never lands.
            if crate::bridge_dm_gate::peer_verdict(
                &state,
                &caller.account,
                &bridge.block.id,
                &req.sender,
            )
            .await?
                == fauna_core::data::DmVerdict::Blocked
            {
                return Err(guardian_approval_required_ns(
                    NS,
                    "this account's guardian has blocked messages from this sender",
                ));
            }
            let (room_id, deposited) = state
                .db
                .deposit_bridged_message(
                    &caller.account,
                    &bridge.principal_id,
                    &req.far_room_id,
                    &req.far_message_id,
                    &req.sender,
                    &req.sealed_content,
                    req.created_at,
                )
                .await
                .map_err(store_error)?;
            let (id, duplicate) = match deposited {
                Deposited::Stored(id) => {
                    notify_changed(&state, &caller.account, &room_id);
                    (id, false)
                }
                Deposited::Duplicate(id) => (id, true),
            };
            encode_reply(&DepositReply {
                id,
                duplicate,
                extra: Default::default(),
            })
        })
    })
}

/// `conversation.outbox.fetch`.
pub(crate) fn principal_outbox_fetch_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: OutboxFetchRequest = decode(&payload).map_err(malformed)?;
            let bridge = caller_bridge(&state, &caller).await?;
            let items = state
                .db
                .fetch_bridged_outbox(
                    &caller.account,
                    &bridge.principal_id,
                    req.after_id,
                    page(req.limit),
                )
                .await
                .map_err(internal)?
                .into_iter()
                .map(|o| OutboxItem {
                    id: o.id,
                    far_room_id: o.far_room_id,
                    ciphertext: o.ciphertext,
                    queued_at: o.queued_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&OutboxFetchReply {
                items,
                extra: Default::default(),
            })
        })
    })
}

/// `conversation.outbox.ack`.
pub(crate) fn principal_outbox_ack_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: OutboxAckRequest = decode(&payload).map_err(malformed)?;
            if req.ids.len() > BRIDGED_PAGE_MAX as usize {
                return Err(invalid_params_ns(
                    NS,
                    format!("at most {BRIDGED_PAGE_MAX} ids per ack"),
                ));
            }
            let bridge = caller_bridge(&state, &caller).await?;
            let acked = state
                .db
                .ack_bridged_outbox(&caller.account, &bridge.principal_id, &req.ids)
                .await
                .map_err(internal)?;
            encode_reply(&OutboxAckReply {
                acked,
                extra: Default::default(),
            })
        })
    })
}

/// `conversation.receipt` — where the declared delivery is `Async`.
pub(crate) fn principal_receipt_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: ReceiptRequest = decode(&payload).map_err(malformed)?;
            if req.state != RECEIPT_DELIVERED && req.state != RECEIPT_READ {
                return Err(invalid_params_ns(
                    NS,
                    format!("state must be {RECEIPT_DELIVERED} or {RECEIPT_READ}"),
                ));
            }
            let bridge = caller_bridge(&state, &caller).await?;
            if bridge.block.capabilities.get("delivery_mode")
                != Some(&BridgeCapabilityValue::Mode("Async".into()))
            {
                return Err(permission_denied_ns(
                    NS,
                    "receipts are for a bridge whose declared delivery is Async",
                ));
            }
            if let Some(room_id) = state
                .db
                .record_bridged_receipt(&caller.account, &bridge.principal_id, req.id, &req.state)
                .await
                .map_err(internal)?
            {
                notify_changed(&state, &caller.account, &room_id);
            }
            encode_reply(&BridgedAck::default())
        })
    })
}

// ── the user's four ────────────────────────────────────────────────────

fn rooms_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_ROOMS_LIST).await?;
            let _req: RoomsListRequest = decode(&payload).map_err(malformed)?;
            let bridges = serving_bridges(&state, &actor_id).await?;
            let gate = crate::bridge_dm_gate::gate_context(&state, &actor_id).await?;
            let rooms = state
                .db
                .list_bridged_rooms(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|row| {
                    let bridge = bridges
                        .iter()
                        .find(|b| b.principal_id == row.bridge_principal_id);
                    room_info(row, bridge, gate.as_ref())
                })
                .collect();
            encode_reply(&RoomsListReply {
                rooms,
                extra: Default::default(),
            })
        })
    })
}

fn rooms_open_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_ROOMS_OPEN).await?;
            let req: RoomsOpenRequest = decode(&payload).map_err(malformed)?;
            far_id("address", &req.address)?;
            let bridges = serving_bridges(&state, &actor_id).await?;
            // The one place a declared grammar is matched.
            let mut admitting = bridges.iter().filter(|b| {
                req.bridge_id.as_deref().is_none_or(|id| id == b.block.id)
                    && b.block.address_matches(&req.address)
            });
            let (Some(bridge), None) = (admitting.next(), admitting.next()) else {
                return Err(coded_ns(
                    NS,
                    "address_refused",
                    "no single connected bridge accepts this address",
                ));
            };
            let seat = BridgeSeat {
                principal_id: &bridge.principal_id,
                bridge: &bridge.block,
                bridge_x25519: &bridge.x25519,
            };
            let far_room_id = far_room_for(bridge, &req.address);
            let shape = RoomShape {
                participants: vec![far_room_id.clone()],
                ..Default::default()
            };
            let (row, created) = state
                .db
                .upsert_bridged_room(&actor_id, &seat, &far_room_id, &shape)
                .await
                .map_err(store_error)?;
            if created {
                notify_changed(&state, &actor_id, &row.room_id);
            }
            let gate = crate::bridge_dm_gate::gate_context(&state, &actor_id).await?;
            encode_reply(&RoomsOpenReply {
                room: room_info(row, Some(bridge), gate.as_ref()),
                extra: Default::default(),
            })
        })
    })
}

fn inbox_fetch_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_INBOX_FETCH).await?;
            let req: InboxFetchRequest = decode(&payload).map_err(malformed)?;
            let guardian_state = match &req.room_id {
                Some(room_id) => {
                    let room = state
                        .db
                        .get_bridged_room(&actor_id, room_id)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| not_found_ns(NS, "no such bridged room"))?;
                    let gate = crate::bridge_dm_gate::gate_context(&state, &actor_id).await?;
                    room_guardian_state(gate.as_ref(), &room.bridge_id, &room.participants)
                }
                None => None,
            };
            let messages = state
                .db
                .fetch_bridged_inbox(
                    &actor_id,
                    req.room_id.as_deref().map(|r| r.as_slice()),
                    req.after_id,
                    page(req.limit),
                )
                .await
                .map_err(internal)?
                .into_iter()
                .map(|m| BridgedMessageInfo {
                    id: m.id,
                    room_id: m.room_id,
                    bridge_id: m.bridge_id,
                    outbound: m.outbound,
                    sender: m.sender,
                    sealed_content: m.sealed_content,
                    created_at: m.created_at,
                    received_at: m.received_at,
                    receipt: m.receipt,
                    undelivered: m.undelivered,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&InboxFetchReply {
                messages,
                guardian_state,
                extra: Default::default(),
            })
        })
    })
}

fn send_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_SEND).await?;
            let req: SendRequest = decode(&payload).map_err(malformed)?;
            sealed_ok("sealed_for_bridge", &req.sealed_for_bridge)?;
            sealed_ok("sealed_for_self", &req.sealed_for_self)?;
            let room = state
                .db
                .get_bridged_room(&actor_id, &req.room_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found_ns(NS, "no such bridged room"))?;
            // A room whose bridge no live principal serves is read-only: the
            // item would be sealed to a key nobody holds and queued for a
            // fetch that never comes.
            if !serving_bridges(&state, &actor_id)
                .await?
                .iter()
                .any(|b| b.principal_id == room.bridge_principal_id)
            {
                return Err(bridge_disconnected());
            }
            // The gate's outbound rule: only an explicit guardian `block`
            // refuses (the knob is not consulted) — placed before the queue,
            // because a sent message cannot be recalled.
            for peer in &room.participants {
                if crate::bridge_dm_gate::outbound_blocked(&state, &actor_id, &room.bridge_id, peer)
                    .await?
                {
                    return Err(guardian_approval_required_ns(
                        NS,
                        "this account's guardian has blocked messages with this person",
                    ));
                }
            }
            // An in-process leg knows now whether it could deliver: a
            // message that can never leave is refused by name, not stored as
            // Sent and dropped at the drain.
            crate::bridge_legs::send_precheck(&state, &room.bridge_principal_id, &actor_id).await?;
            let id = state
                .db
                .send_bridged_message(
                    &actor_id,
                    &room,
                    &req.sealed_for_bridge,
                    &req.sealed_for_self,
                )
                .await
                .map_err(store_error)?;
            // The ward chose the correspondent, so replies flow: seed `allow`
            // (a guardianship-guarded no-op that never overwrites a `block`).
            for peer in &room.participants {
                state
                    .db
                    .seed_dm_peer_allow(&actor_id, &room.bridge_id, peer)
                    .await
                    .map_err(internal)?;
            }
            notify_changed(&state, &actor_id, &room.room_id);
            // An in-process leg drains its own outbox; nudge it now rather
            // than at its worker's next tick.
            crate::bridge_legs::nudge_drain(&state, &room.bridge_principal_id);
            encode_reply(&SendReply {
                id,
                self_address: room.self_address.unwrap_or_default(),
                extra: Default::default(),
            })
        })
    })
}

/// The actor-router half of a `ThirdParty`-only kind: the wire contract lives
/// in the actor router (one kind, one wire contract), but no actor class holds
/// it, so this runs the central gate and is refused there, always — the
/// `fauna.nostr.bunker.bind` shape.
fn third_party_only(kind: &'static str) -> RpcHandler {
    Box::new(move |state, actor_id, _payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, kind).await?;
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Every principal handler this module serves, for `principal_handlers`'
/// table.
pub(crate) fn principal_handlers() -> [(&'static str, PrincipalHandler); 6] {
    [
        (KIND_DEPOSIT, principal_deposit_handler()),
        (KIND_OUTBOX_FETCH, principal_outbox_fetch_handler()),
        (KIND_OUTBOX_ACK, principal_outbox_ack_handler()),
        (KIND_ROOM_UPSERT, principal_room_upsert_handler()),
        (KIND_ROOM_MEMBERS, principal_room_members_handler()),
        (KIND_RECEIPT, principal_receipt_handler()),
    ]
}

/// Register the ten kinds. All light (5 s). Replay: the bridge's writes are
/// idempotent (deposit on its far id, upsert and members by value, ack and
/// receipt by id) and replay-safe; the user's `send` mints a row per call, so
/// it forbids replay.
pub fn register_bridged_conversation_handlers(b: &mut RpcRouterBuilder) {
    let light = || Duration::from_secs(5);
    for kind in [
        KIND_DEPOSIT,
        KIND_OUTBOX_FETCH,
        KIND_OUTBOX_ACK,
        KIND_ROOM_UPSERT,
        KIND_ROOM_MEMBERS,
        KIND_RECEIPT,
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: light(),
                handler: third_party_only(kind),
            },
        );
    }
    for (kind, forbid_replay, handler) in [
        (KIND_ROOMS_LIST, false, rooms_list_handler()),
        (KIND_ROOMS_OPEN, false, rooms_open_handler()),
        (KIND_INBOX_FETCH, false, inbox_fetch_handler()),
        (KIND_SEND, true, send_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay,
                default_deadline: light(),
                handler,
            },
        );
    }
}

/// The user's four kinds as an in-crate test calls them — what a first-party
/// leg's tests read their deposits back through, so they assert the family's
/// own surface rather than its tables. Compiled only with a leg whose tests
/// call it.
#[cfg(all(test, any(feature = "bluesky", feature = "activitypub")))]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;

    fn enc<T: serde::Serialize>(v: &T) -> bytes::Bytes {
        fauna_protocol::encode_canonical(v).unwrap()
    }

    /// `conversation.rooms.list`.
    pub(crate) async fn rooms(state: &Arc<AppState>, account: [u8; 32]) -> Vec<BridgedRoomInfo> {
        let reply = rooms_list_handler()(state.clone(), account, enc(&RoomsListRequest::default()))
            .await
            .unwrap();
        decode::<RoomsListReply>(&reply).unwrap().rooms
    }

    /// `conversation.inbox.fetch` for one room.
    pub(crate) async fn inbox(
        state: &Arc<AppState>,
        account: [u8; 32],
        room_id: &[u8],
    ) -> Vec<BridgedMessageInfo> {
        let req = InboxFetchRequest {
            room_id: Some(room_id.to_vec().into()),
            ..Default::default()
        };
        let reply = inbox_fetch_handler()(state.clone(), account, enc(&req))
            .await
            .unwrap();
        decode::<InboxFetchReply>(&reply).unwrap().messages
    }

    /// `conversation.rooms.open` on `bridge_id`.
    pub(crate) async fn open(
        state: &Arc<AppState>,
        account: [u8; 32],
        bridge_id: &str,
        address: &str,
    ) -> Result<BridgedRoomInfo, RpcError> {
        let req = RoomsOpenRequest {
            bridge_id: Some(bridge_id.to_string()),
            address: address.to_string(),
            extra: Default::default(),
        };
        let reply = rooms_open_handler()(state.clone(), account, enc(&req)).await?;
        Ok(decode::<RoomsOpenReply>(&reply).unwrap().room)
    }

    /// `conversation.send` of `text`, sealed to the room's bridge key.
    pub(crate) async fn send(
        state: &Arc<AppState>,
        account: [u8; 32],
        room: &BridgedRoomInfo,
        text: &str,
    ) -> Result<i64, RpcError> {
        let key = <[u8; 32]>::try_from(room.bridge_x25519.as_slice()).expect("32-byte bridge key");
        let sealed_for_bridge = fauna_mls::wrapped_blob::seal_to_recipient(text.as_bytes(), &key)
            .expect("seal for the bridge")
            .to_canonical_bytes()
            .expect("canonical");
        let req = SendRequest {
            room_id: room.room_id.clone(),
            sealed_for_bridge,
            sealed_for_self: b"to-self".to_vec(),
            extra: Default::default(),
        };
        let reply = send_handler()(state.clone(), account, enc(&req)).await?;
        Ok(decode::<SendReply>(&reply).unwrap().id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use bytes::Bytes;
    use fauna_protocol::encode_canonical;

    use crate::db::CacheDb;
    use crate::db::third_party_principals::{
        AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
    };

    const ACCOUNT: [u8; 32] = [0xA1; 32];
    const CLIENT: &str = "https://app.example/client.json";
    const OTHER: &str = "https://other.example/client.json";
    const NEVER: i64 = i64::MAX;

    fn manifest() -> PrincipalManifest {
        PrincipalManifest {
            publisher_key: [0x9B; 32],
            declared_kinds: vec![],
            declared_service_auth: vec![],
            events_uri: None,
            bridge: Some(BridgeBlock {
                id: "matrix".into(),
                glyph: "bridge".into(),
                address_grammar: "^@[^:]+:.+$".into(),
                capabilities: [(
                    "delivery_mode".to_string(),
                    BridgeCapabilityValue::Mode("Async".into()),
                )]
                .into_iter()
                .collect(),
                extra: Default::default(),
            }),
        }
    }

    async fn consent(db: &CacheDb, family: &[u8], client: &str, holder: [u8; 32]) -> Vec<u8> {
        db.record_atproto_oauth_grant(
            &ACCOUNT,
            family,
            client,
            Some("Example Bridge"),
            "fauna:conversations:bridge",
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(holder),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: Some(manifest()),
            },
        )
        .await
        .unwrap();
        db.list_third_party_principals(&ACCOUNT)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.client_id == client)
            .unwrap()
            .principal_id
    }

    fn enc<T: serde::Serialize>(v: &T) -> Bytes {
        encode_canonical(v).unwrap()
    }

    async fn list(state: &Arc<AppState>) -> Vec<BridgedRoomInfo> {
        let reply = rooms_list_handler()(state.clone(), ACCOUNT, enc(&RoomsListRequest::default()))
            .await
            .unwrap();
        decode::<RoomsListReply>(&reply).unwrap().rooms
    }

    async fn inbox(state: &Arc<AppState>, room_id: &[u8]) -> Vec<BridgedMessageInfo> {
        let req = InboxFetchRequest {
            room_id: Some(room_id.to_vec().into()),
            ..Default::default()
        };
        let reply = inbox_fetch_handler()(state.clone(), ACCOUNT, enc(&req))
            .await
            .unwrap();
        decode::<InboxFetchReply>(&reply).unwrap().messages
    }

    async fn send(state: &Arc<AppState>, room_id: &[u8]) -> Result<i64, RpcError> {
        let req = SendRequest {
            room_id: room_id.to_vec(),
            sealed_for_bridge: b"to-bridge".to_vec(),
            sealed_for_self: b"to-self".to_vec(),
            extra: Default::default(),
        };
        let reply = send_handler()(state.clone(), ACCOUNT, enc(&req)).await?;
        Ok(decode::<SendReply>(&reply).unwrap().id)
    }

    async fn open(state: &Arc<AppState>) -> Result<BridgedRoomInfo, RpcError> {
        let req = RoomsOpenRequest {
            bridge_id: None,
            address: "@bob:example.org".into(),
            extra: Default::default(),
        };
        let reply = rooms_open_handler()(state.clone(), ACCOUNT, enc(&req)).await?;
        Ok(decode::<RoomsOpenReply>(&reply).unwrap().room)
    }

    /// `apps/bridges.md` § Phase G → *When the bridge stops serving*, as the
    /// user's four kinds see it: after the revoke the room lists
    /// `disconnected`, reads as ever with its stranded Sent row marked,
    /// refuses `send` by name and `rooms.open` as no bridge's — and a
    /// principal declaring the id reconnects it, key and all.
    #[tokio::test]
    async fn a_revoked_bridges_room_is_read_only_until_a_principal_adopts_it() {
        const KEY_1: [u8; 32] = [1; 32];
        const KEY_2: [u8; 32] = [2; 32];
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        // A build with a bridge feature serves that network's in-process leg
        // beside the roster's bridges, and asks its tables whether it serves
        // this account — tables a booted nest of that flavor always holds.
        crate::bridge_legs::test_support::init_leg_tables(&db).await;
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        let p1 = consent(&db, b"family-1", CLIENT, KEY_1).await;
        let state = Arc::new(AppState::for_test(db.clone()));

        let room = open(&state).await.unwrap();
        assert!(!room.disconnected);
        assert_eq!(room.bridge_x25519, KEY_1);
        let stranded = send(&state, &room.room_id).await.unwrap();

        db.revoke_third_party_principal(&ACCOUNT, &p1)
            .await
            .unwrap()
            .unwrap();

        let rooms = list(&state).await;
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].disconnected);
        assert_eq!(rooms[0].bridge_label, "matrix");
        assert_eq!(rooms[0].glyph, "bridge");
        let rows = inbox(&state, &room.room_id).await;
        assert_eq!(
            rows.iter()
                .map(|m| (m.id, m.outbound, m.undelivered))
                .collect::<Vec<_>>(),
            vec![(stranded, true, true)]
        );
        let err = send(&state, &room.room_id).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.bridge_disconnected");
        let err = open(&state).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.address_refused");

        // Another document the user chose for the same network adopts it.
        consent(&db, b"family-2", OTHER, KEY_2).await;
        let rooms = list(&state).await;
        assert!(!rooms[0].disconnected);
        assert_eq!(rooms[0].bridge_x25519, KEY_2);
        assert_eq!(rooms[0].room_id, room.room_id);
        let delivered = send(&state, &room.room_id).await.unwrap();
        let rows = inbox(&state, &room.room_id).await;
        assert_eq!(
            rows.iter()
                .map(|m| (m.id, m.undelivered))
                .collect::<Vec<_>>(),
            vec![(stranded, true), (delivered, false)]
        );
        // And `rooms.open` finds the same room on the new bridge.
        assert_eq!(open(&state).await.unwrap().room_id, room.room_id);
    }
}
