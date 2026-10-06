//! `fauna.bridges.conversation.*` — the bridged-conversation family, Phase G
//! (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue → Phase G
//! owns the kinds' existence and caller classes;
//! `docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, ruling 2, owns every payload's semantics).
//!
//! Two halves over one sealed mailbox. The **bridge's six** (`ThirdParty`,
//! scope `fauna:conversations:bridge`) deposit inbound ciphertext sealed to the
//! user's recipient key, drain the outbox the user's app sealed to the bridge's
//! X25519 key, and report a room's shape, its far membership and receipts. The
//! **user's four** (`User`, caller-scoped — none takes a target actor) list and
//! open rooms, read one room's rows in both directions, and send. The nest
//! opens nothing in either direction.
//!
//! A bridge names a room by its own `far_room_id`; the nest mints `room_id`
//! (the `rooms` row's id) at the room's birth and the user's side names rooms
//! by it. A message row's `id` is nest-assigned in arrival order, so it is
//! both the inbox cursor and the order of the nest's `received_at` — never the
//! far side's claimed `created_at`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;
use crate::kind_manifest::BridgeCapabilityValue;

/// The bridge's deposit of one inbound message.
pub const KIND_DEPOSIT: &str = "fauna.bridges.conversation.deposit";
/// The bridge's drain of queued outbound items.
pub const KIND_OUTBOX_FETCH: &str = "fauna.bridges.conversation.outbox.fetch";
/// The bridge's acknowledgement of delivered outbound items.
pub const KIND_OUTBOX_ACK: &str = "fauna.bridges.conversation.outbox.ack";
/// The bridge's create-or-update of a room.
pub const KIND_ROOM_UPSERT: &str = "fauna.bridges.conversation.room.upsert";
/// The bridge's report of a room's far membership.
pub const KIND_ROOM_MEMBERS: &str = "fauna.bridges.conversation.room.members";
/// The bridge's delivery / read receipt for an outbound item.
pub const KIND_RECEIPT: &str = "fauna.bridges.conversation.receipt";
/// The user's list of their bridged rooms.
pub const KIND_ROOMS_LIST: &str = "fauna.bridges.conversation.rooms.list";
/// The user's find-or-mint of a room for a far address.
pub const KIND_ROOMS_OPEN: &str = "fauna.bridges.conversation.rooms.open";
/// The user's read of bridged rows.
pub const KIND_INBOX_FETCH: &str = "fauna.bridges.conversation.inbox.fetch";
/// The user's send.
pub const KIND_SEND: &str = "fauna.bridges.conversation.send";
/// The push nudging the account's own clients after a deposit, a room change
/// or a receipt. Carries no content.
pub const PUSH_CONVERSATION_CHANGED: &str = "fauna.bridges.push.conversation_changed";

/// The largest sealed message body a deposit or a send may carry, in bytes
/// (each of `sealed_for_bridge` / `sealed_for_self` on a send).
pub const MAX_BRIDGED_SEALED_BYTES: usize = 256 * 1024;
/// The most participants one room report may carry.
pub const MAX_BRIDGED_PARTICIPANTS: usize = 256;
/// The longest far-network identifier (room id, message id, address) the
/// family accepts, in bytes.
pub const MAX_BRIDGED_FAR_ID_BYTES: usize = 512;
/// The default and the largest page of `inbox.fetch` / `outbox.fetch`.
pub const BRIDGED_PAGE_DEFAULT: u32 = 100;
/// See [`BRIDGED_PAGE_DEFAULT`].
pub const BRIDGED_PAGE_MAX: u32 = 500;

/// A receipt's state — `delivered`, then `read`.
pub const RECEIPT_DELIVERED: &str = "delivered";
/// See [`RECEIPT_DELIVERED`].
pub const RECEIPT_READ: &str = "read";

// ── the bridge's six ───────────────────────────────────────────────────

/// `conversation.deposit` — one inbound message, sealed to the user, into a
/// room this principal serves (born by an earlier `room.upsert` or the user's
/// `rooms.open`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DepositRequest {
    /// The room, in the bridge's own spelling.
    pub far_room_id: String,
    /// The message's far-network id — the idempotence key within the room.
    pub far_message_id: String,
    /// Who sent it, as the authenticated bridge asserts for its own network —
    /// the identity the family gate's verdict keys on.
    pub sender: String,
    /// The message, HPKE-sealed by the bridge to the user's recipient key.
    #[serde(with = "serde_bytes")]
    pub sealed_content: Vec<u8>,
    /// The far side's claimed send time, unix ms — carried, never ordered on.
    #[serde(default)]
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The deposit's answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DepositReply {
    /// The stored row's id.
    pub id: i64,
    /// `true` when `(room, far_message_id)` was already stored — the earlier
    /// row's id is answered and nothing is written.
    #[serde(default)]
    pub duplicate: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.outbox.fetch` — queued outbound items for the rooms this
/// principal serves, oldest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutboxFetchRequest {
    /// Items with `id > after_id`.
    #[serde(default)]
    pub after_id: i64,
    /// Page size; `0` = [`BRIDGED_PAGE_DEFAULT`], capped at [`BRIDGED_PAGE_MAX`].
    #[serde(default)]
    pub limit: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One queued outbound item.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutboxItem {
    /// The item's id — the Sent row's, which `outbox.ack` and `receipt` name.
    pub id: i64,
    pub far_room_id: String,
    /// The message, sealed by the user's app to this bridge's X25519 key.
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
    pub queued_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The outbox page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutboxFetchReply {
    pub items: Vec<OutboxItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.outbox.ack` — the bridge delivered these items; the nest
/// deletes them. Ids this principal does not serve are ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutboxAckRequest {
    pub ids: Vec<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// How many items the ack removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutboxAckReply {
    pub acked: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.room.upsert` — create or update a room's bridge-side shape.
/// The capability vector is never sent: it is the manifest's, snapshotted at
/// the room's birth from the principal's roster row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomUpsertRequest {
    pub far_room_id: String,
    /// The far network's display name for the room, if it has one.
    #[serde(default)]
    pub label: Option<String>,
    /// The far participants, in the far network's spelling, the account's own
    /// address excluded. A create seats them; an update leaves them to
    /// `room.members`.
    #[serde(default)]
    pub participants: Vec<String>,
    /// The account's own address on the far network — the sender a Sent copy
    /// carries.
    #[serde(default)]
    pub self_address: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The room the upsert landed on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomUpsertReply {
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    /// `true` when this call minted the room.
    #[serde(default)]
    pub created: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.room.members` — the far network's membership for a room,
/// replacing the far seats only (the user's and the bridge's are never
/// written by a report). Refused where the declared vector says membership
/// change does not reach the bridge.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomMembersRequest {
    pub far_room_id: String,
    pub participants: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An empty acknowledgement.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgedAck {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.receipt` — a delivery or read receipt for an outbound item,
/// where the declared `delivery_mode` is `Async`. A receipt never moves a row
/// backwards (`read` after `delivered`, never the reverse).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReceiptRequest {
    /// The outbound item's id (the Sent row's).
    pub id: i64,
    /// [`RECEIPT_DELIVERED`] or [`RECEIPT_READ`].
    pub state: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── the user's four ────────────────────────────────────────────────────

/// One bridged room as the user's app reads it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgedRoomInfo {
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    /// The manifest's `bridge.id`.
    pub bridge_id: String,
    /// The bridge's display label — the resolved name the consent card showed.
    pub bridge_label: String,
    /// The bridge's declared glyph id.
    pub glyph: String,
    pub far_room_id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// The far participants, the account's own address excluded.
    pub participants: Vec<String>,
    /// The account's own far address, once the bridge has reported it.
    #[serde(default)]
    pub self_address: Option<String>,
    /// The declared capability vector — the `ThreadCapabilities` record minus
    /// `encryption`, deserializable into that record once `encryption` is set.
    pub capabilities: BTreeMap<String, BridgeCapabilityValue>,
    /// The bridge principal's X25519 key — what `sealed_for_bridge` is sealed
    /// to.
    #[serde(with = "serde_bytes")]
    pub bridge_x25519: Vec<u8>,
    /// The room's newest row's `received_at` (its birth when empty), unix ms.
    pub last_at: i64,
    /// The family gate's marker for the room's peer: `"held"` / `"blocked"`,
    /// absent for deliver (every unsupervised account). Computed at read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_state: Option<String>,
    /// `true` when no live principal of the account serves this room's
    /// bridge — the principal was revoked, or re-consented without the bridge
    /// block: the room is readable, never writable, until a principal
    /// declaring the bridge id adopts it (`apps/bridges.md` § Phase G → *When
    /// the bridge stops serving*). Absent on the wire means connected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disconnected: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.rooms.list` — the caller's bridged rooms across every bridge
/// serving the account, newest activity first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The caller's rooms.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomsListReply {
    pub rooms: Vec<BridgedRoomInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.rooms.open` — find or mint the 1:1 room for a far address.
/// Idempotent; the address is matched against the bridge's declared
/// `address_grammar` (the one place it is matched).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomsOpenRequest {
    /// The bridge to open on. Absent: the nest picks the one consented bridge
    /// whose grammar admits `address` — none, or more than one, refuses.
    #[serde(default)]
    pub bridge_id: Option<String>,
    /// The peer, in the far network's own spelling.
    pub address: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The room opened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoomsOpenReply {
    pub room: BridgedRoomInfo,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.inbox.fetch` — rows in both directions, sealed to the user,
/// in arrival order.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InboxFetchRequest {
    /// One room; absent = every room of the caller's (the shared driver's
    /// whole-inbox poll).
    #[serde(default)]
    pub room_id: Option<ByteBuf>,
    /// Rows with `id > after_id`.
    #[serde(default)]
    pub after_id: i64,
    /// Page size; `0` = [`BRIDGED_PAGE_DEFAULT`], capped at [`BRIDGED_PAGE_MAX`].
    #[serde(default)]
    pub limit: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One stored row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgedMessageInfo {
    /// Nest-assigned in arrival order — the cursor, and the id `send` answered
    /// for a Sent row.
    pub id: i64,
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    pub bridge_id: String,
    /// `true` for the caller's own Sent copy.
    pub outbound: bool,
    /// The far sender (the account's own far address on a Sent copy, when
    /// known).
    pub sender: String,
    /// Sealed to the user's own recipient key.
    #[serde(with = "serde_bytes")]
    pub sealed_content: Vec<u8>,
    /// The far side's claimed time (a Sent copy: the nest's), unix ms.
    pub created_at: i64,
    /// The nest's clock at storage, unix ms — what the rows are ordered on.
    pub received_at: i64,
    /// A Sent copy's receipt: [`RECEIPT_DELIVERED`] / [`RECEIPT_READ`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// `true` on a Sent copy whose queued item the bridge never drained: its
    /// serving principal ended — revoked, re-consented without the bridge
    /// block or under a new holder key — before the fetch, and the nest
    /// deleted the undeliverable ciphertext (`apps/bridges.md` § Phase G →
    /// *When the bridge stops serving*). The user sends again. Absent on the
    /// wire means not marked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undelivered: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InboxFetchReply {
    pub messages: Vec<BridgedMessageInfo>,
    /// The family marker for the requested room's peer (one-room reads only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_state: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `conversation.send` — queue one item for the bridge and store the Sent
/// copy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SendRequest {
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    /// Sealed to the room's `bridge_x25519` — the item the bridge drains.
    #[serde(with = "serde_bytes")]
    pub sealed_for_bridge: Vec<u8>,
    /// Sealed to the user's own recipient key — the Sent row.
    #[serde(with = "serde_bytes")]
    pub sealed_for_self: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What the send recorded.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SendReply {
    /// The Sent row's id — the one `inbox.fetch` serves it under.
    pub id: i64,
    /// The account's own far address, as the bridge reported it (empty until
    /// it has).
    #[serde(default)]
    pub self_address: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.push.conversation_changed` — the nudge. No content.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConversationChangedPush {
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn a_room_row_rides_dag_cbor_and_its_vector_reads_back_typed() {
        let room = BridgedRoomInfo {
            room_id: vec![7; 32],
            bridge_id: "matrix".into(),
            capabilities: [
                (
                    "supports_reactions".to_string(),
                    BridgeCapabilityValue::Flag(true),
                ),
                (
                    "delivery_mode".to_string(),
                    BridgeCapabilityValue::Mode("Async".into()),
                ),
            ]
            .into_iter()
            .collect(),
            bridge_x25519: vec![9; 32],
            guardian_state: Some("held".into()),
            ..Default::default()
        };
        let back: BridgedRoomInfo = decode_strict(&encode_canonical(&room).unwrap()).unwrap();
        assert_eq!(back, room);
    }

    #[test]
    fn an_inbox_request_without_a_room_is_the_whole_inbox() {
        let req = InboxFetchRequest {
            after_id: 41,
            ..Default::default()
        };
        let back: InboxFetchRequest = decode_strict(&encode_canonical(&req).unwrap()).unwrap();
        assert_eq!(back.room_id, None);
        assert_eq!(back.after_id, 41);
    }
}
