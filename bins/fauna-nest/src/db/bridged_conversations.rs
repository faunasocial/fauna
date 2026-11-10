//! The bridged-conversation family's sealed mailbox
//! (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue → Phase G;
//! every payload's semantics are `docs/goal/ui/conversations.md` § Where logic
//! lives → *The `Bridged` adapter*'s, the roster's
//! `docs/goal/behavior/conversation-rooms.md` § Bridged rooms').
//!
//! The nest opens nothing here. A room is born by the bridge's `room.upsert`
//! or the user's `rooms.open`, in one transaction that writes the
//! `bridge_conversation_rooms` row, the `rooms` row (`transport_only`, which
//! nothing re-classes) and the floor roster's two seats — the user as `user`,
//! the bridge principal as `bridge`. The far participants are never floor
//! principals; they live on the room row, replaced only by a
//! `room.members` report.
//!
//! **A room belongs to `(account, bridge id)`, never to the principal**
//! (`apps/bridges.md` § Phase G → *When the bridge stops serving*): its id
//! derives from them, and `bridge_principal_id` names whichever roster row
//! currently declares that bridge id. A principal's end
//! ([`end_bridged_outbox_in_tx`]) deletes nothing but its undrained outbox,
//! stamping each Sent row `undelivered_at`; a principal that comes to declare
//! the id adopts the rooms ([`rebind_bridged_rooms_in_tx`]), both inside the
//! roster's own transaction.
//!
//! Tables: [`super::migrations`]' `MIGRATIONS_BRIDGED_CONVERSATIONS`.

use anyhow::{Context, Result};
use fauna_protocol::kind_manifest::BridgeBlock;
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};

/// Inbound rows one `(account, bridge principal)` may hold — the
/// `MAX_NOSTR_DMS_PER_ACTOR` shape: the deposit seam is the plane a party
/// outside the nest writes, so it is bounded by the resource it fills. A hard
/// constant, never a configuration surface. The owner's own Sent copies are
/// not counted.
pub const MAX_BRIDGED_INBOUND_PER_PRINCIPAL: i64 = 50_000;
/// Store-wide backstop over every account's inbound rows.
pub const MAX_BRIDGED_INBOUND_TOTAL: i64 = 500_000;
/// Inbound rows one room may hold — one `(account, bridge, far room)`, which
/// on a first-party leg is one peer. Well under
/// [`MAX_BRIDGED_INBOUND_PER_PRINCIPAL`], so one far party's flood fills only
/// its own room and a second peer's message to the same account is still
/// stored (`docs/goal/behavior/activitypub.md` § Security posture → *Inbound*,
/// the bridged-DM bounds). A hard constant.
pub const MAX_BRIDGED_INBOUND_PER_ROOM: i64 = 10_000;
/// Inbound rows one bridge principal may hold across every account — a
/// first-party leg's principal id is the same for every account, so this is
/// the leg's share of [`MAX_BRIDGED_INBOUND_TOTAL`]: a flood on one leg stops
/// before it can take the store from the others. A hard constant.
pub const MAX_BRIDGED_INBOUND_PER_LEG: i64 = MAX_BRIDGED_INBOUND_TOTAL / 2;
/// Rooms one `(account, bridge principal)` may hold.
pub const MAX_BRIDGED_ROOMS_PER_PRINCIPAL: i64 = 10_000;
/// Undrained outbound items one `(account, bridge principal)` may queue — a
/// bridge that stops draining stops the send, rather than growing the box.
pub const MAX_BRIDGED_OUTBOX_PER_PRINCIPAL: i64 = 1_000;

/// The first-party Nostr DM leg's bridge id — the in-process caller of this
/// family (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
/// adapter*, ruling 2 (e); `docs/goal/ui/nostr.md` § Implementation status
/// today → DMs). The same string the bridge-DM gate keys its verdict rows on.
pub const NOSTR_LEG_BRIDGE_ID: &str = "nostr";
/// The leg's declared label.
pub const NOSTR_LEG_LABEL: &str = "Nostr";
/// The leg's declared glyph — a Nostr DM thread renders ⚡ as before.
pub const NOSTR_LEG_GLYPH: &str = "bolt";
/// The leg's address grammar: a bech32 `npub`. `conversation.rooms.open`
/// matches it and then keys the room on the canonical 32-byte hex the ingest
/// and the gate use (`crate::nostr::bridge_leg::far_room_id`).
pub const NOSTR_LEG_ADDRESS_GRAMMAR: &str = "^npub1[02-9ac-hj-np-z]{58}$";
/// The principal id the leg's rooms seat as their bridge member. The leg is
/// in-process, so it has no roster row: this constant is its identity, served
/// beside an account's consented bridges wherever the account has a Nostr
/// account linked.
pub const NOSTR_LEG_PRINCIPAL_ID: [u8; 16] = *b"fauna:nostr-leg\0";

/// The first-party Bluesky DM leg's bridge id (`conversations.md` § *The
/// `Bridged` adapter*, ruling 3): `chat.bsky` direct messages of a
/// consume-side linked account, polled and sent under that account's OAuth
/// session (`crate::bluesky::dm_leg`). The string the bridge-DM gate keys its
/// verdict rows on.
pub const BLUESKY_LEG_BRIDGE_ID: &str = "bluesky";
/// The leg's declared label.
pub const BLUESKY_LEG_LABEL: &str = "Bluesky";
/// The leg's declared glyph.
pub const BLUESKY_LEG_GLYPH: &str = "butterfly";
/// The leg's address grammar: the peer's DID — what the chat service names a
/// convo member by, and what the room and the gate key on — or a handle, with
/// or without its leading `@`, which `conversation.rooms.open` resolves to the
/// DID under the account's session (`crate::bluesky::dm_leg::far_room_id`).
pub const BLUESKY_LEG_ADDRESS_GRAMMAR: &str = r"^(did:(plc|web):[A-Za-z0-9._:%-]{1,256}|@?([A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z]([A-Za-z0-9-]*[A-Za-z0-9])?)$";
/// The principal id the Bluesky leg's rooms seat as their bridge member.
pub const BLUESKY_LEG_PRINCIPAL_ID: [u8; 16] = *b"fauna:bsky-leg\0\0";

/// The first-party ActivityPub DM leg's bridge id (ruling 3): a Fediverse
/// direct message — an AS2 `Note` addressed to a local actor and to no
/// collection (`crate::activitypub::dm_leg`).
pub const ACTIVITYPUB_LEG_BRIDGE_ID: &str = "activitypub";
/// The leg's declared label — the user-facing word for the network
/// (`activitypub.md` § Naming).
pub const ACTIVITYPUB_LEG_LABEL: &str = "Fediverse";
/// The leg's declared glyph.
pub const ACTIVITYPUB_LEG_GLYPH: &str = "globe";
/// The leg's address grammar: the peer's actor URI — the identity an inbox
/// delivery's HTTP signature proves, and what the room and the gate key on —
/// or the two-`@` `@user@host` it is known by, which `conversation.rooms.open`
/// resolves by WebFinger (`crate::activitypub::dm_leg::far_room_id`); a bare
/// `user@host` is an email address to every app's address probe.
/// Unbounded repetitions on purpose: the family bounds a far id's length
/// itself, and a counted repetition over a Unicode class outgrows the
/// grammar compiler's size limit.
pub const ACTIVITYPUB_LEG_ADDRESS_GRAMMAR: &str = r"^(https://[^\s/]+/\S+|@[^\s@/:?#%]+@([A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)$";
/// [`ACTIVITYPUB_LEG_ADDRESS_GRAMMAR`] widened by the AP loopback test hook
/// (`crate::ssrf::ap_loopback_hook_open` — a `test-hooks` build with
/// `FAUNA_TEST_AP_ALLOW_LOOPBACK` set, never a shipped one): an `http` actor
/// URI on a loopback host and an `@user@<loopback>:<port>` account too — the
/// in-test fediverse server's spellings, so a journey can open a room from the
/// picker with a peer only the dial-side hook lets the nest reach. The dial
/// itself is still the hook's to permit (`activitypub::outbound`).
pub const ACTIVITYPUB_LEG_LOOPBACK_TEST_GRAMMAR: &str = r"^(https://[^\s/]+/\S+|http://(127(\.[0-9]{1,3}){3}|localhost)(:[0-9]{1,5})?/\S+|@[^\s@/:?#%]+@([A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?|@[^\s@/:?#%]+@(127(\.[0-9]{1,3}){3}|localhost)(:[0-9]{1,5})?)$";
/// The principal id the ActivityPub leg's rooms seat as their bridge member.
pub const ACTIVITYPUB_LEG_PRINCIPAL_ID: [u8; 16] = *b"fauna:ap-leg\0\0\0\0";

/// A first-party leg's declared block: its identity and the family's flag
/// vector — `can_manage_members` answers the two membership flags, every
/// other affordance is withheld, delivery is `Async`. `encryption` is never
/// declared: it is the room's derived class (ruling 1).
fn leg_block(
    id: &str,
    glyph: &str,
    address_grammar: &str,
    can_manage_members: bool,
) -> BridgeBlock {
    use fauna_protocol::kind_manifest::BridgeCapabilityValue::{Flag, Mode};
    let flags = [
        ("supports_attachments", false),
        ("supports_markdown", false),
        ("supports_reactions", false),
        ("supports_message_delete", false),
        ("supports_per_message_reply", false),
        ("supports_membership_change", false),
        ("supports_recipient_selection", false),
        ("supports_rename", false),
        ("supports_subject", false),
        ("can_invite", can_manage_members),
        ("can_remove_members", can_manage_members),
        ("can_set_policy", false),
        ("can_appoint_admins", false),
        ("can_transfer_ownership", false),
        ("can_leave_room", false),
    ];
    BridgeBlock {
        id: id.into(),
        glyph: glyph.into(),
        address_grammar: address_grammar.into(),
        capabilities: flags
            .into_iter()
            .map(|(k, v)| (k.to_string(), Flag(v)))
            .chain([("delivery_mode".to_string(), Mode("Async".into()))])
            .collect(),
        extra: Default::default(),
    }
}

/// The Nostr leg's declared vector — today's `(Nostr, _)` constants of
/// `fauna_conversations::capabilities::derive_capabilities` minus
/// `encryption`, which is the room's derived class (ruling 1).
#[must_use]
pub fn nostr_leg_block() -> BridgeBlock {
    leg_block(
        NOSTR_LEG_BRIDGE_ID,
        NOSTR_LEG_GLYPH,
        NOSTR_LEG_ADDRESS_GRAMMAR,
        true,
    )
}

/// The Bluesky leg's declared block: a one-to-one text rail.
#[must_use]
pub fn bluesky_leg_block() -> BridgeBlock {
    leg_block(
        BLUESKY_LEG_BRIDGE_ID,
        BLUESKY_LEG_GLYPH,
        BLUESKY_LEG_ADDRESS_GRAMMAR,
        false,
    )
}

/// The ActivityPub leg's declared block: a one-to-one text rail.
#[must_use]
pub fn activitypub_leg_block() -> BridgeBlock {
    activitypub_leg_block_with(crate::ssrf::ap_loopback_hook_open())
}

/// [`activitypub_leg_block`] with the loopback test hook's state given, so the
/// shipped grammar is pinned without touching the process environment.
fn activitypub_leg_block_with(loopback_hook: bool) -> BridgeBlock {
    leg_block(
        ACTIVITYPUB_LEG_BRIDGE_ID,
        ACTIVITYPUB_LEG_GLYPH,
        if loopback_hook {
            ACTIVITYPUB_LEG_LOOPBACK_TEST_GRAMMAR
        } else {
            ACTIVITYPUB_LEG_ADDRESS_GRAMMAR
        },
        false,
    )
}

/// A first-party leg's X25519 keypair, `(secret, public)`, minted on first use
/// and kept in `first_party_bridge_keys`, the secret wrapped under the
/// deployment seed read on this connection (`crate::nest_kek`,
/// `FIRST_PARTY_BRIDGE_CONTEXT` — so a seed rotation re-keys it with every
/// sibling). The in-process twin of a hosted plugin's `holder.key`: it guards
/// nothing the nest cannot already read, since the leg is the nest and a room
/// it serves is transport-only by derivation (`conversations.md` § *The
/// `Bridged` adapter*, ruling 2's honest exception).
///
/// # Errors
/// The nest holds no deployment seed; the stored row does not open under it.
pub fn first_party_bridge_key(
    conn: &rusqlite::Connection,
    bridge_id: &str,
) -> Result<([u8; 32], [u8; 32])> {
    use crate::nest_kek::{
        FIRST_PARTY_BRIDGE_CONTEXT, require_deployment_seed, unwrap_32, wrap_32,
    };
    let seed = require_deployment_seed(conn)?;
    let held = conn
        .query_row(
            "SELECT secret_wrapped, x25519_public FROM first_party_bridge_keys
              WHERE bridge_id = ?1",
            [bridge_id],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .context("read first-party bridge key")?;
    if let Some((wrapped, public)) = held {
        let secret = unwrap_32(FIRST_PARTY_BRIDGE_CONTEXT, &seed, &wrapped)
            .with_context(|| format!("open first-party bridge key {bridge_id}"))?;
        let public = <[u8; 32]>::try_from(public.as_slice())
            .map_err(|_| anyhow::anyhow!("first-party bridge key {bridge_id}: bad public"))?;
        return Ok((secret, public));
    }
    let (secret, public) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let wrapped = wrap_32(FIRST_PARTY_BRIDGE_CONTEXT, &seed, &secret)?;
    conn.execute(
        "INSERT INTO first_party_bridge_keys (bridge_id, secret_wrapped, x25519_public, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![bridge_id, wrapped, &public[..], now_epoch_millis()],
    )
    .context("mint first-party bridge key")?;
    Ok((secret, public))
}

/// One bridged room's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedRoomRow {
    pub room_id: Vec<u8>,
    pub bridge_principal_id: Vec<u8>,
    pub bridge_id: String,
    pub far_room_id: String,
    pub label: Option<String>,
    pub participants: Vec<String>,
    pub self_address: Option<String>,
    /// The declared vector as stored (the manifest's `capabilities`, JSON).
    pub capabilities: String,
    pub bridge_x25519: Vec<u8>,
    pub created_at: i64,
    pub last_at: i64,
    /// The room's newest local remove, unix ms; 0 when none
    /// (`bridged-adapter.md` § Removing bridged rows, rule 5).
    pub last_removed_at: i64,
}

/// One stored message row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedMessageRow {
    pub id: i64,
    pub room_id: Vec<u8>,
    pub bridge_id: String,
    pub outbound: bool,
    pub sender: String,
    pub sealed_content: Vec<u8>,
    pub created_at: i64,
    pub received_at: i64,
    pub receipt: Option<String>,
    /// A Sent row whose outbox item the nest deleted when its serving
    /// principal ended — never drained, never deliverable.
    pub undelivered: bool,
    /// A tombstone: the user removed the nest's copy; `sealed_content` is
    /// empty and the row stays only to keep the dedup.
    pub removed: bool,
}

/// One undrained outbound item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedOutboxRow {
    pub id: i64,
    pub far_room_id: String,
    pub ciphertext: Vec<u8>,
    pub queued_at: i64,
}

/// What a room upsert needs to know about the bridge it is born on: the
/// roster row's identity and the two values the room snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeSeat<'a> {
    pub principal_id: &'a [u8],
    pub bridge: &'a BridgeBlock,
    pub bridge_x25519: &'a [u8; 32],
}

/// The bridge's side of a room upsert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomShape {
    pub label: Option<String>,
    pub participants: Vec<String>,
    pub self_address: Option<String>,
}

/// Why a write was refused — typed so a handler answers it as the caller's
/// error, never as a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BridgedRefused {
    /// No room of this principal's under that far id.
    #[error("no such bridged room")]
    NoSuchRoom,
    /// A store cap is reached; nothing was written.
    #[error("the bridged conversation store is full")]
    Full,
}

/// What a deposit stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deposited {
    /// A new row.
    Stored(i64),
    /// `(room, far_message_id)` was already stored: its id, nothing written.
    Duplicate(i64),
}

/// What [`CacheDb::bridged_deposit_precheck`] found would refuse a deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositPrecheck {
    /// The far message id is already stored — in the named room, or, when
    /// the caller cannot name one yet, in any room of this principal's.
    Duplicate,
    /// An inbound cap is reached.
    Full,
}

/// One room's summary: its far id, its row count, and its newest row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedRoomSummary {
    pub bridge_id: String,
    pub far_room_id: String,
    pub participants: Vec<String>,
    pub message_count: i64,
    /// The newest row's `received_at`, unix ms.
    pub last_received_at: i64,
    pub last_sealed: Option<Vec<u8>>,
    pub last_outbound: bool,
}

/// The `rooms` row's id for a bridged room: a keyed derivation over the
/// account, the bridge id and the far room id, so a find-or-mint is idempotent
/// by construction and two accounts on one bridge never share a room.
pub fn bridged_room_id(actor_id: &[u8; 32], bridge_id: &str, far_room_id: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("fauna 2026-10-03 bridged conversation room id");
    h.update(actor_id);
    h.update(&(bridge_id.len() as u64).to_le_bytes());
    h.update(bridge_id.as_bytes());
    h.update(far_room_id.as_bytes());
    *h.finalize().as_bytes()
}

const ROOM_COLUMNS: &str = "room_id, bridge_principal_id, bridge_id, far_room_id, label,
     participants, self_address, capabilities, bridge_x25519, created_at, last_at,
     COALESCE(last_removed_at, 0)";

fn room_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BridgedRoomRow> {
    let participants: String = r.get(5)?;
    Ok(BridgedRoomRow {
        room_id: r.get(0)?,
        bridge_principal_id: r.get(1)?,
        bridge_id: r.get(2)?,
        far_room_id: r.get(3)?,
        label: r.get(4)?,
        // A list this nest wrote itself; undecodable is corruption, rendered
        // as no participants rather than hiding the room.
        participants: serde_json::from_str(&participants).unwrap_or_default(),
        self_address: r.get(6)?,
        capabilities: r.get(7)?,
        bridge_x25519: r.get(8)?,
        created_at: r.get(9)?,
        last_at: r.get(10)?,
        last_removed_at: r.get(11)?,
    })
}

const MESSAGE_SELECT: &str = "SELECT m.id, m.room_id, r.bridge_id, m.direction, m.sender,
            m.sealed_content, m.created_at, m.received_at, m.receipt,
            m.undelivered_at IS NOT NULL, m.removed_at IS NOT NULL
       FROM bridge_conversation_messages m
       JOIN bridge_conversation_rooms r ON r.room_id = m.room_id";

fn message_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BridgedMessageRow> {
    Ok(BridgedMessageRow {
        id: r.get(0)?,
        room_id: r.get(1)?,
        bridge_id: r.get(2)?,
        outbound: r.get::<_, String>(3)? == "out",
        sender: r.get(4)?,
        sealed_content: r.get(5)?,
        created_at: r.get(6)?,
        received_at: r.get(7)?,
        receipt: r.get(8)?,
        undelivered: r.get(9)?,
        removed: r.get(10)?,
    })
}

/// A principal stops serving: delete its undrained outbox items — ciphertext
/// sealed to a key no live principal holds any more — and stamp each one's
/// Sent row `undelivered_at`, so the thread can say *not delivered*. Rooms and
/// messages are untouched (the user's sealed content). Returns the rooms whose
/// Sent rows were stamped, for the post-commit nudge.
///
/// Called inside the roster's own transaction: the revoke, a re-consent whose
/// manifest drops or moves the bridge id, a holder-key replacement.
pub(super) fn end_bridged_outbox_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    principal_id: &[u8],
    now: i64,
) -> Result<Vec<Vec<u8>>> {
    let rooms: Vec<Vec<u8>> = {
        let mut stmt = tx
            .prepare(
                "SELECT DISTINCT room_id FROM bridge_conversation_outbox
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2",
            )
            .context("prepare undrained rooms")?;
        stmt.query_map(rusqlite::params![&actor_id[..], principal_id], |r| r.get(0))
            .context("query undrained rooms")?
            .collect::<std::result::Result<_, _>>()
            .context("collect undrained rooms")?
    };
    if rooms.is_empty() {
        return Ok(rooms);
    }
    tx.execute(
        "UPDATE bridge_conversation_messages SET undelivered_at = ?3
          WHERE actor_id = ?1 AND id IN
                (SELECT message_id FROM bridge_conversation_outbox
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2)",
        rusqlite::params![&actor_id[..], principal_id, now],
    )
    .context("stamp undelivered sent rows")?;
    tx.execute(
        "DELETE FROM bridge_conversation_outbox
          WHERE actor_id = ?1 AND bridge_principal_id = ?2",
        rusqlite::params![&actor_id[..], principal_id],
    )
    .context("delete undeliverable outbox")?;
    Ok(rooms)
}

/// A principal comes to declare `bridge_id`: every room of
/// `(account, bridge_id)` is rebound to it — the row's serving principal, its
/// snapshotted key and vector, and the floor's bridge seat — so the rooms
/// reconnect at once, history intact, new sends sealed to the new key.
/// Idempotent for the principal already serving them. Returns the rooms
/// rebound.
pub(super) fn rebind_bridged_rooms_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    bridge_id: &str,
    seat: &BridgeSeat<'_>,
) -> Result<u32> {
    let capabilities =
        serde_json::to_string(&seat.bridge.capabilities).context("encode declared vector")?;
    let rebound = tx
        .execute(
            "UPDATE bridge_conversation_rooms
                SET bridge_principal_id = ?3, bridge_x25519 = ?4, capabilities = ?5
              WHERE actor_id = ?1 AND bridge_id = ?2",
            rusqlite::params![
                &actor_id[..],
                bridge_id,
                seat.principal_id,
                &seat.bridge_x25519[..],
                capabilities,
            ],
        )
        .context("rebind bridged rooms")?;
    tx.execute(
        "UPDATE room_members SET principal_id = ?3
          WHERE principal_kind = 'bridge' AND principal_id != ?3
            AND room_id IN (SELECT room_id FROM bridge_conversation_rooms
                             WHERE actor_id = ?1 AND bridge_id = ?2)",
        rusqlite::params![&actor_id[..], bridge_id, seat.principal_id],
    )
    .context("reseat bridged rooms")?;
    Ok(u32::try_from(rebound).unwrap_or(u32::MAX))
}

fn count(conn: &rusqlite::Connection, sql: &str, params: impl rusqlite::Params) -> Result<i64> {
    conn.query_row(sql, params, |r| r.get(0))
        .context("count bridged rows")
}

/// Is any inbound cap reached for a deposit from `principal_id` to `actor_id`
/// — the room's (when `room_id` names one), the principal's for this
/// account, the principal's across every account (the leg's share), the
/// store's? The one test the deposit transaction and the precheck share.
/// Every arm counts LIVE rows only (`removed_at IS NULL`), so a remove frees
/// exactly what it removes (`bridged-adapter.md` § Removing bridged rows,
/// rule 2).
fn inbound_full(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    principal_id: &[u8],
    room_id: Option<&[u8]>,
) -> Result<bool> {
    if let Some(room) = room_id
        && count(
            conn,
            "SELECT COUNT(*) FROM bridge_conversation_messages
              WHERE room_id = ?1 AND direction = 'in' AND removed_at IS NULL",
            rusqlite::params![room],
        )? >= MAX_BRIDGED_INBOUND_PER_ROOM
    {
        return Ok(true);
    }
    let mine = count(
        conn,
        "SELECT COUNT(*) FROM bridge_conversation_messages m
           JOIN bridge_conversation_rooms r ON r.room_id = m.room_id
          WHERE m.actor_id = ?1 AND r.bridge_principal_id = ?2 AND m.direction = 'in'
            AND m.removed_at IS NULL",
        rusqlite::params![&actor_id[..], principal_id],
    )?;
    if mine >= MAX_BRIDGED_INBOUND_PER_PRINCIPAL {
        return Ok(true);
    }
    let leg = count(
        conn,
        "SELECT COUNT(*) FROM bridge_conversation_messages m
           JOIN bridge_conversation_rooms r ON r.room_id = m.room_id
          WHERE r.bridge_principal_id = ?1 AND m.direction = 'in' AND m.removed_at IS NULL",
        rusqlite::params![principal_id],
    )?;
    if leg >= MAX_BRIDGED_INBOUND_PER_LEG {
        return Ok(true);
    }
    let total = count(
        conn,
        "SELECT COUNT(*) FROM bridge_conversation_messages
          WHERE direction = 'in' AND removed_at IS NULL",
        [],
    )?;
    Ok(total >= MAX_BRIDGED_INBOUND_TOTAL)
}

/// Is `room_id` one of the caller's bridged rooms (removed or not)?
fn caller_room_exists(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    room_id: &[u8],
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM bridge_conversation_rooms
                         WHERE actor_id = ?1 AND room_id = ?2)",
        rusqlite::params![&actor_id[..], room_id],
        |r| r.get(0),
    )
    .context("read the caller's bridged room")
}

/// Tombstone the room's live rows `ids` (every live row when `None`), stamp
/// the room's `last_removed_at` when anything moved, and bound the room's
/// tombstones (rule 3). Returns how many rows were tombstoned.
fn tombstone_in_tx(
    tx: &rusqlite::Transaction<'_>,
    room_id: &[u8],
    ids: Option<&[i64]>,
    now: i64,
) -> Result<u32> {
    let mut removed = 0u32;
    match ids {
        Some(ids) => {
            for id in ids {
                removed += u32::try_from(
                    tx.execute(
                        "UPDATE bridge_conversation_messages
                            SET sealed_content = x'', removed_at = ?3
                          WHERE room_id = ?1 AND id = ?2 AND removed_at IS NULL",
                        rusqlite::params![room_id, id, now],
                    )
                    .context("tombstone bridged row")?,
                )
                .unwrap_or(0);
            }
        }
        None => {
            removed = u32::try_from(
                tx.execute(
                    "UPDATE bridge_conversation_messages
                        SET sealed_content = x'', removed_at = ?2
                      WHERE room_id = ?1 AND removed_at IS NULL",
                    rusqlite::params![room_id, now],
                )
                .context("tombstone bridged room rows")?,
            )
            .unwrap_or(u32::MAX);
        }
    }
    if removed > 0 {
        tx.execute(
            "UPDATE bridge_conversation_rooms SET last_removed_at = ?2 WHERE room_id = ?1",
            rusqlite::params![room_id, now],
        )
        .context("stamp bridged room removal")?;
    }
    // The oldest tombstones go first: their far ids are the least likely to
    // be re-served, so a room's rows never exceed twice its cap.
    tx.execute(
        "DELETE FROM bridge_conversation_messages
          WHERE room_id = ?1 AND removed_at IS NOT NULL
            AND id NOT IN (SELECT id FROM bridge_conversation_messages
                            WHERE room_id = ?1 AND removed_at IS NOT NULL
                            ORDER BY id DESC LIMIT ?2)",
        rusqlite::params![room_id, MAX_BRIDGED_INBOUND_PER_ROOM],
    )
    .context("prune bridged tombstones")?;
    Ok(removed)
}

/// The body of a send: the Sent copy stored, the item queued, the room
/// touched — inside the caller's transaction.
fn send_in(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    room: &BridgedRoomRow,
    sealed_for_bridge: &[u8],
    sealed_for_self: &[u8],
) -> Result<i64> {
    let now = now_epoch_millis();
    let queued = count(
        tx,
        "SELECT COUNT(*) FROM bridge_conversation_outbox
          WHERE actor_id = ?1 AND bridge_principal_id = ?2",
        rusqlite::params![&actor_id[..], room.bridge_principal_id],
    )?;
    if queued >= MAX_BRIDGED_OUTBOX_PER_PRINCIPAL {
        return Err(anyhow::anyhow!(BridgedRefused::Full));
    }
    tx.execute(
        "INSERT INTO bridge_conversation_messages
            (room_id, actor_id, direction, far_message_id, sender, sealed_content,
             created_at, received_at)
         VALUES (?1, ?2, 'out', NULL, ?3, ?4, ?5, ?5)",
        rusqlite::params![
            room.room_id,
            &actor_id[..],
            room.self_address.as_deref().unwrap_or(""),
            sealed_for_self,
            now
        ],
    )
    .context("insert bridged sent row")?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO bridge_conversation_outbox
            (message_id, room_id, actor_id, bridge_principal_id, ciphertext, queued_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            id,
            room.room_id,
            &actor_id[..],
            room.bridge_principal_id,
            sealed_for_bridge,
            now
        ],
    )
    .context("queue bridged outbound")?;
    tx.execute(
        // A send is a new live row: it reopens a removed room (rule 4).
        "UPDATE bridge_conversation_rooms SET last_at = ?2, removed_at = NULL
          WHERE room_id = ?1",
        rusqlite::params![room.room_id, now],
    )
    .context("touch bridged room")?;
    Ok(id)
}

impl CacheDb {
    /// Find or mint the room `(account, bridge, far_room_id)`, seating the
    /// floor roster at birth; an existing room takes `shape`'s label and self
    /// address where given and the seat's current vector and key, and keeps
    /// its far participants (those are `room.members`'). Returns the room and
    /// whether this call minted it.
    ///
    /// # Errors
    /// [`BridgedRefused::Full`] when minting would pass
    /// [`MAX_BRIDGED_ROOMS_PER_PRINCIPAL`].
    pub async fn upsert_bridged_room(
        &self,
        actor_id: &[u8; 32],
        seat: &BridgeSeat<'_>,
        far_room_id: &str,
        shape: &RoomShape,
    ) -> Result<(BridgedRoomRow, bool)> {
        let now = now_epoch_millis();
        let capabilities =
            serde_json::to_string(&seat.bridge.capabilities).context("encode declared vector")?;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged room upsert")?;
        // Found by its natural key, never by re-deriving the id: a room moved
        // to a successor keeps the id derived from its predecessor.
        let held: Option<Vec<u8>> = tx
            .query_row(
                "SELECT room_id FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND bridge_id = ?2 AND far_room_id = ?3",
                rusqlite::params![&actor_id[..], seat.bridge.id, far_room_id],
                |r| r.get(0),
            )
            .optional()
            .context("read bridged room")?;
        let exists = held.is_some();
        let room_id = held
            .unwrap_or_else(|| bridged_room_id(actor_id, &seat.bridge.id, far_room_id).to_vec());
        if exists {
            tx.execute(
                "UPDATE bridge_conversation_rooms
                    SET label = COALESCE(?2, label),
                        self_address = COALESCE(?3, self_address),
                        capabilities = ?4,
                        bridge_x25519 = ?5,
                        bridge_principal_id = ?6
                  WHERE room_id = ?1",
                rusqlite::params![
                    &room_id[..],
                    shape.label,
                    shape.self_address,
                    capabilities,
                    &seat.bridge_x25519[..],
                    seat.principal_id,
                ],
            )
            .context("update bridged room")?;
            // The seat follows the serving principal (a rebind the consent
            // did not reach — a first-party leg with no roster row).
            tx.execute(
                "UPDATE room_members SET principal_id = ?2
                  WHERE room_id = ?1 AND principal_kind = 'bridge' AND principal_id != ?2",
                rusqlite::params![&room_id[..], seat.principal_id],
            )
            .context("reseat bridged room")?;
        } else {
            let rooms = count(
                &tx,
                "SELECT COUNT(*) FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2",
                rusqlite::params![&actor_id[..], seat.principal_id],
            )?;
            if rooms >= MAX_BRIDGED_ROOMS_PER_PRINCIPAL {
                return Err(anyhow::anyhow!(BridgedRefused::Full));
            }
            let participants =
                serde_json::to_string(&shape.participants).context("encode participants")?;
            tx.execute(
                "INSERT INTO bridge_conversation_rooms
                    (room_id, actor_id, bridge_principal_id, bridge_id, far_room_id, label,
                     participants, self_address, capabilities, bridge_x25519, created_at, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                rusqlite::params![
                    &room_id[..],
                    &actor_id[..],
                    seat.principal_id,
                    seat.bridge.id,
                    far_room_id,
                    shape.label,
                    participants,
                    shape.self_address,
                    capabilities,
                    &seat.bridge_x25519[..],
                    now,
                ],
            )
            .context("insert bridged room")?;
            // The room record: transport-only by derivation — a bridge
            // principal is a member — and nothing re-classes it.
            tx.execute(
                "INSERT INTO rooms (room_id, class, created_at, updated_at)
                 VALUES (?1, 'transport_only', ?2, ?2)",
                rusqlite::params![&room_id[..], now],
            )
            .context("insert bridged rooms row")?;
            for (principal, kind) in [(&actor_id[..], "user"), (seat.principal_id, "bridge")] {
                tx.execute(
                    "INSERT INTO room_members (room_id, principal_id, principal_kind,
                                               joined_at, reported_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    rusqlite::params![&room_id[..], principal, kind, now],
                )
                .context("seat bridged room member")?;
            }
        }
        let row = tx
            .query_row(
                &format!("SELECT {ROOM_COLUMNS} FROM bridge_conversation_rooms WHERE room_id = ?1"),
                rusqlite::params![&room_id[..]],
                room_from_row,
            )
            .context("read back bridged room")?;
        tx.commit().context("commit bridged room upsert")?;
        Ok((row, !exists))
    }

    /// Replace a room's far participants — the user's and the bridge's seats
    /// are never written by a report. `false` when the principal serves no
    /// such room. A report on a room the user removed is dropped
    /// (`bridged-adapter.md` § Removing bridged rows, rule 1): `true`, nothing
    /// written.
    pub async fn set_bridged_room_participants(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        bridge_id: &str,
        far_room_id: &str,
        participants: &[String],
    ) -> Result<bool> {
        let participants = serde_json::to_string(participants).context("encode participants")?;
        let conn = self.conn.lock().await;
        let removed: Option<bool> = conn
            .query_row(
                "SELECT removed_at IS NOT NULL FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND far_room_id = ?3 AND bridge_id = ?4",
                rusqlite::params![&actor_id[..], principal_id, far_room_id, bridge_id],
                |r| r.get(0),
            )
            .optional()
            .context("read bridged room for a members report")?;
        match removed {
            None => Ok(false),
            Some(true) => Ok(true),
            Some(false) => {
                conn.execute(
                    "UPDATE bridge_conversation_rooms SET participants = ?4
                      WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND far_room_id = ?3 AND bridge_id = ?5",
                    rusqlite::params![&actor_id[..], principal_id, far_room_id, participants, bridge_id],
                )
                .context("set bridged room participants")?;
                Ok(true)
            }
        }
    }

    /// One room of the caller's, by its `rooms` id.
    pub async fn get_bridged_room(
        &self,
        actor_id: &[u8; 32],
        room_id: &[u8],
    ) -> Result<Option<BridgedRoomRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {ROOM_COLUMNS} FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND room_id = ?2"
            ),
            rusqlite::params![&actor_id[..], room_id],
            room_from_row,
        )
        .optional()
        .context("get bridged room")
    }

    /// One room a principal serves, by the bridge's far id.
    pub async fn get_bridged_room_by_far_id(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        bridge_id: &str,
        far_room_id: &str,
    ) -> Result<Option<BridgedRoomRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {ROOM_COLUMNS} FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND far_room_id = ?3 AND bridge_id = ?4"
            ),
            rusqlite::params![&actor_id[..], principal_id, far_room_id, bridge_id],
            room_from_row,
        )
        .optional()
        .context("get bridged room by far id")
    }

    /// Every bridged room of the caller's, newest activity first — a room the
    /// user removed omitted until a new live row reopens it (rule 4).
    pub async fn list_bridged_rooms(&self, actor_id: &[u8; 32]) -> Result<Vec<BridgedRoomRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ROOM_COLUMNS} FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND removed_at IS NULL
                  ORDER BY last_at DESC, room_id ASC"
            ))
            .context("prepare list bridged rooms")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor_id[..]], room_from_row)
            .context("query bridged rooms")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect bridged rooms")?;
        Ok(rows)
    }

    /// Store one inbound row in a room the principal serves. Idempotent on
    /// `(room, far_message_id)`. The family gate is the caller's, composed
    /// before this is reached.
    ///
    /// # Errors
    /// [`BridgedRefused::NoSuchRoom`]; [`BridgedRefused::Full`] past any
    /// inbound cap ([`inbound_full`]) — nothing is written.
    #[allow(clippy::too_many_arguments)]
    pub async fn deposit_bridged_message(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        bridge_id: &str,
        far_room_id: &str,
        far_message_id: &str,
        sender: &str,
        sealed_content: &[u8],
        created_at: i64,
    ) -> Result<(Vec<u8>, Deposited)> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged deposit")?;
        let Some(room_id) = tx
            .query_row(
                "SELECT room_id FROM bridge_conversation_rooms
                  WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND far_room_id = ?3 AND bridge_id = ?4",
                rusqlite::params![&actor_id[..], principal_id, far_room_id, bridge_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .context("read deposit room")?
        else {
            return Err(anyhow::anyhow!(BridgedRefused::NoSuchRoom));
        };
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM bridge_conversation_messages
                  WHERE room_id = ?1 AND far_message_id = ?2",
                rusqlite::params![room_id, far_message_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .context("read deposit duplicate")?
        {
            return Ok((room_id, Deposited::Duplicate(id)));
        }
        if inbound_full(&tx, actor_id, principal_id, Some(&room_id))? {
            return Err(anyhow::anyhow!(BridgedRefused::Full));
        }
        tx.execute(
            "INSERT INTO bridge_conversation_messages
                (room_id, actor_id, direction, far_message_id, sender, sealed_content,
                 created_at, received_at)
             VALUES (?1, ?2, 'in', ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                room_id,
                &actor_id[..],
                far_message_id,
                sender,
                sealed_content,
                created_at,
                now
            ],
        )
        .context("insert bridged deposit")?;
        let id = tx.last_insert_rowid();
        tx.execute(
            // A new live row reopens a removed room (rule 4); a duplicate
            // returned above and reopens nothing.
            "UPDATE bridge_conversation_rooms SET last_at = ?2, removed_at = NULL
              WHERE room_id = ?1",
            rusqlite::params![room_id, now],
        )
        .context("touch bridged room")?;
        tx.commit().context("commit bridged deposit")?;
        Ok((room_id, Deposited::Stored(id)))
    }

    /// The caller's rows with `id > after_id`, oldest first — one room's, or
    /// every room's.
    pub async fn fetch_bridged_inbox(
        &self,
        actor_id: &[u8; 32],
        room_id: Option<&[u8]>,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<BridgedMessageRow>> {
        let conn = self.conn.lock().await;
        let rows = match room_id {
            Some(room) => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{MESSAGE_SELECT} WHERE m.actor_id = ?1 AND m.room_id = ?2 AND m.id > ?3
                          ORDER BY m.id ASC LIMIT ?4"
                    ))
                    .context("prepare bridged inbox")?;
                stmt.query_map(
                    rusqlite::params![&actor_id[..], room, after_id, limit],
                    message_from_row,
                )
                .context("query bridged inbox")?
                .collect::<std::result::Result<Vec<_>, _>>()
            }
            None => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{MESSAGE_SELECT} WHERE m.actor_id = ?1 AND m.id > ?2
                          ORDER BY m.id ASC LIMIT ?3"
                    ))
                    .context("prepare bridged inbox")?;
                stmt.query_map(
                    rusqlite::params![&actor_id[..], after_id, limit],
                    message_from_row,
                )
                .context("query bridged inbox")?
                .collect::<std::result::Result<Vec<_>, _>>()
            }
        }
        .context("collect bridged inbox")?;
        Ok(rows)
    }

    /// The user's local remove of rows of one of their rooms
    /// (`bridged-adapter.md` § Removing bridged rows): each live row named is
    /// tombstoned — content emptied, `removed_at` stamped, its far id kept so
    /// a re-served copy is still a duplicate — the room's `last_removed_at`
    /// moves, and the room's oldest tombstones past
    /// [`MAX_BRIDGED_INBOUND_PER_ROOM`] are pruned, in one transaction. An id
    /// of no live row of this room counts nothing. The outbox is untouched: a
    /// send already queued stands. Returns how many rows were tombstoned.
    ///
    /// # Errors
    /// [`BridgedRefused::NoSuchRoom`] when the room is not the caller's.
    pub async fn remove_bridged_messages(
        &self,
        actor_id: &[u8; 32],
        room_id: &[u8],
        ids: &[i64],
    ) -> Result<u32> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged remove")?;
        if !caller_room_exists(&tx, actor_id, room_id)? {
            return Err(anyhow::anyhow!(BridgedRefused::NoSuchRoom));
        }
        let removed = tombstone_in_tx(&tx, room_id, Some(ids), now)?;
        tx.commit().context("commit bridged remove")?;
        Ok(removed)
    }

    /// The user's local remove of a whole room: every live row tombstoned
    /// (as [`Self::remove_bridged_messages`]) and the room stamped
    /// `removed_at`, so `rooms.list` omits it until a new live row — a far
    /// message with an unseen id, a send, the user's `rooms.open` — reopens
    /// it. The room's own material (key, far id, roster seat) stays.
    /// Idempotent: a repeat answers 0 and moves nothing.
    ///
    /// # Errors
    /// [`BridgedRefused::NoSuchRoom`] when the room is not the caller's.
    pub async fn remove_bridged_room(&self, actor_id: &[u8; 32], room_id: &[u8]) -> Result<u32> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged room remove")?;
        if !caller_room_exists(&tx, actor_id, room_id)? {
            return Err(anyhow::anyhow!(BridgedRefused::NoSuchRoom));
        }
        let removed = tombstone_in_tx(&tx, room_id, None, now)?;
        tx.execute(
            "UPDATE bridge_conversation_rooms SET removed_at = ?2, last_removed_at = ?2
              WHERE room_id = ?1 AND removed_at IS NULL",
            rusqlite::params![room_id, now],
        )
        .context("hide bridged room")?;
        tx.commit().context("commit bridged room remove")?;
        Ok(removed)
    }

    /// Clear a removed room's `removed_at` — the user's own `rooms.open` on
    /// it (rule 4). `true` when the room was hidden and now lists again.
    pub async fn reopen_bridged_room(&self, actor_id: &[u8; 32], room_id: &[u8]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE bridge_conversation_rooms SET removed_at = NULL
                  WHERE actor_id = ?1 AND room_id = ?2 AND removed_at IS NOT NULL",
                rusqlite::params![&actor_id[..], room_id],
            )
            .context("reopen bridged room")?;
        Ok(n > 0)
    }

    /// Store the caller's Sent copy and queue the item sealed to the bridge,
    /// in one transaction. Returns the Sent row's id — the outbox item's too.
    ///
    /// # Errors
    /// [`BridgedRefused::Full`] past [`MAX_BRIDGED_OUTBOX_PER_PRINCIPAL`].
    pub async fn send_bridged_message(
        &self,
        actor_id: &[u8; 32],
        room: &BridgedRoomRow,
        sealed_for_bridge: &[u8],
        sealed_for_self: &[u8],
    ) -> Result<i64> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged send")?;
        let id = send_in(&tx, actor_id, room, sealed_for_bridge, sealed_for_self)?;
        tx.commit().context("commit bridged send")?;
        Ok(id)
    }

    /// [`Self::send_bridged_message`] for a send the NEST makes on the user's
    /// behalf exactly once per cause — a message no app composed, so no app
    /// can tell a retry from a second send. `once` runs inside the send's own
    /// transaction with the new row's id and records the cause (its own
    /// table, its own key); answering `false` — the cause is already recorded
    /// — rolls the whole send back and this answers `None`. The record and
    /// the message therefore exist together or not at all: a crash before the
    /// commit leaves neither, so the cause's replay sends; after it, both, so
    /// the replay sends nothing.
    ///
    /// # Errors
    /// [`BridgedRefused::Full`] past [`MAX_BRIDGED_OUTBOX_PER_PRINCIPAL`]; an
    /// error from `once`.
    pub async fn send_bridged_message_once(
        &self,
        actor_id: &[u8; 32],
        room: &BridgedRoomRow,
        sealed_for_bridge: &[u8],
        sealed_for_self: &[u8],
        once: impl FnOnce(&rusqlite::Transaction<'_>, i64) -> Result<bool>,
    ) -> Result<Option<i64>> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged send")?;
        let id = send_in(&tx, actor_id, room, sealed_for_bridge, sealed_for_self)?;
        if !once(&tx, id)? {
            // Dropping the transaction rolls the send back.
            return Ok(None);
        }
        tx.commit().context("commit bridged send")?;
        Ok(Some(id))
    }

    /// The principal's undrained items with `id > after_id`, oldest first.
    pub async fn fetch_bridged_outbox(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<BridgedOutboxRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT o.message_id, r.far_room_id, o.ciphertext, o.queued_at
                   FROM bridge_conversation_outbox o
                   JOIN bridge_conversation_rooms r ON r.room_id = o.room_id
                  WHERE o.actor_id = ?1 AND o.bridge_principal_id = ?2 AND o.message_id > ?3
                  ORDER BY o.message_id ASC LIMIT ?4",
            )
            .context("prepare bridged outbox")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor_id[..], principal_id, after_id, limit],
                |r| {
                    Ok(BridgedOutboxRow {
                        id: r.get(0)?,
                        far_room_id: r.get(1)?,
                        ciphertext: r.get(2)?,
                        queued_at: r.get(3)?,
                    })
                },
            )
            .context("query bridged outbox")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect bridged outbox")?;
        Ok(rows)
    }

    /// Delete delivered items; ids the principal does not hold are ignored.
    pub async fn ack_bridged_outbox(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        ids: &[i64],
    ) -> Result<u32> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin bridged ack")?;
        let mut acked = 0u32;
        for id in ids {
            acked += u32::try_from(
                tx.execute(
                    "DELETE FROM bridge_conversation_outbox
                      WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND message_id = ?3",
                    rusqlite::params![&actor_id[..], principal_id, id],
                )
                .context("ack bridged outbound")?,
            )
            .unwrap_or(0);
        }
        tx.commit().context("commit bridged ack")?;
        Ok(acked)
    }

    /// Record a receipt on one of the principal's Sent rows, never moving it
    /// backwards. The room's id when a row moved, `None` otherwise (unknown
    /// id, not this principal's, removed by the user, or already at or past
    /// `state`).
    pub async fn record_bridged_receipt(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        bridge_id: &str,
        id: i64,
        state: &str,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "UPDATE bridge_conversation_messages SET receipt = ?4
              WHERE id = ?3 AND actor_id = ?1 AND direction = 'out' AND removed_at IS NULL
                AND room_id IN (SELECT room_id FROM bridge_conversation_rooms
                                 WHERE actor_id = ?1 AND bridge_principal_id = ?2
                                   AND bridge_id = ?5)
                AND (receipt IS NULL OR (receipt = 'delivered' AND ?4 = 'read'))
              RETURNING room_id",
            rusqlite::params![&actor_id[..], principal_id, id, state, bridge_id],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("record bridged receipt")
    }

    /// Stamp one of the principal's Sent rows with the far network's id of the
    /// message it became — what an in-process leg records after its far send,
    /// so reading the same message back from the far side is a duplicate
    /// rather than a second row. A row already holding an id, an unknown id,
    /// or a far id the room already stores (the read-back won the race) is
    /// left as it is.
    pub async fn stamp_bridged_sent_far_id(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        id: i64,
        far_message_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE OR IGNORE bridge_conversation_messages SET far_message_id = ?4
              WHERE id = ?3 AND actor_id = ?1 AND direction = 'out'
                AND far_message_id IS NULL
                AND room_id IN (SELECT room_id FROM bridge_conversation_rooms
                                 WHERE actor_id = ?1 AND bridge_principal_id = ?2)",
            rusqlite::params![&actor_id[..], principal_id, id, far_message_id],
        )
        .context("stamp bridged sent far id")?;
        Ok(())
    }

    /// Would an inbound deposit of `far_message_id` from `principal_id` be a
    /// duplicate, or past an inbound cap? The deposit itself re-checks both
    /// inside its transaction; this is the cheap answer an in-process caller
    /// asks before paying for an unwrap it would then throw away.
    ///
    /// `far_room_id` names the room when the caller knows it, and then the
    /// duplicate test is that room's — the key the deposit's own is on, so a
    /// sender minting a far id under another peer's path never shadows that
    /// peer's message — and the room's cap is asked too. `None` (a Nostr wrap,
    /// whose sender only the unwrap reveals) asks the principal-wide
    /// duplicate test and every cap but the room's.
    pub async fn bridged_deposit_precheck(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
        far_room_id: Option<&str>,
        far_message_id: &str,
    ) -> Result<Option<DepositPrecheck>> {
        let conn = self.conn.lock().await;
        let room_id: Option<Vec<u8>> = match far_room_id {
            Some(far) => conn
                .query_row(
                    "SELECT room_id FROM bridge_conversation_rooms
                      WHERE actor_id = ?1 AND bridge_principal_id = ?2 AND far_room_id = ?3",
                    rusqlite::params![&actor_id[..], principal_id, far],
                    |r| r.get(0),
                )
                .optional()
                .context("read precheck room")?,
            None => None,
        };
        let seen: bool = match (far_room_id, &room_id) {
            // A room not yet minted holds nothing.
            (Some(_), None) => false,
            (Some(_), Some(room)) => conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM bridge_conversation_messages
                                     WHERE room_id = ?1 AND far_message_id = ?2)",
                    rusqlite::params![room, far_message_id],
                    |r| r.get(0),
                )
                .context("read deposit duplicate")?,
            (None, _) => conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM bridge_conversation_messages m
                                      JOIN bridge_conversation_rooms r ON r.room_id = m.room_id
                                     WHERE m.actor_id = ?1 AND r.bridge_principal_id = ?2
                                       AND m.far_message_id = ?3)",
                    rusqlite::params![&actor_id[..], principal_id, far_message_id],
                    |r| r.get(0),
                )
                .context("read deposit duplicate")?,
        };
        if seen {
            return Ok(Some(DepositPrecheck::Duplicate));
        }
        Ok(
            inbound_full(&conn, actor_id, principal_id, room_id.as_deref())?
                .then_some(DepositPrecheck::Full),
        )
    }

    /// The rooms the guardian's held-DM queue reads for a ward: every room
    /// that holds a row — live **or tombstoned** — and the room whether or not
    /// the ward removed it, as `(bridge_id, participants, newest received_at
    /// in unix ms)`, newest first. The ward's local remove is the user's own
    /// view and never takes a held peer off the guardian's queue
    /// (`family-safety.md` § The bridge-DM gate, honest bound (a);
    /// `bridged-adapter.md` § Removing bridged rows): a removed room keeps its
    /// row and its participants, so the queue lists it from the room alone.
    pub async fn guardian_queue_bridged_rooms(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<(String, Vec<String>, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT r.bridge_id, r.participants, MAX(m.received_at)
                   FROM bridge_conversation_rooms r
                   JOIN bridge_conversation_messages m ON m.room_id = r.room_id
                  WHERE r.actor_id = ?1
                  GROUP BY r.room_id
                  ORDER BY MAX(m.received_at) DESC, r.room_id ASC",
            )
            .context("prepare guardian queue rooms")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor_id[..]], |r| {
                let participants: String = r.get(1)?;
                Ok((
                    r.get(0)?,
                    serde_json::from_str(&participants).unwrap_or_default(),
                    r.get(2)?,
                ))
            })
            .context("query guardian queue rooms")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect guardian queue rooms")?;
        Ok(rows)
    }

    /// The caller's rooms that hold at least one live row — every bridge's,
    /// or one principal's — newest arrival first, each with its newest live
    /// row. Tombstones and removed rooms are not counted (`bridged-adapter.md`
    /// § Removing bridged rows); the guardian's queue reads
    /// [`Self::guardian_queue_bridged_rooms`] instead, which counts both.
    pub async fn summarize_bridged_rooms(
        &self,
        actor_id: &[u8; 32],
        principal_id: Option<&[u8]>,
    ) -> Result<Vec<BridgedRoomSummary>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT r.bridge_id, r.far_room_id, r.participants, COUNT(m.id),
                        MAX(m.received_at),
                        (SELECT l.sealed_content FROM bridge_conversation_messages l
                          WHERE l.room_id = r.room_id AND l.removed_at IS NULL
                          ORDER BY l.received_at DESC, l.id DESC LIMIT 1),
                        (SELECT l.direction FROM bridge_conversation_messages l
                          WHERE l.room_id = r.room_id AND l.removed_at IS NULL
                          ORDER BY l.received_at DESC, l.id DESC LIMIT 1)
                   FROM bridge_conversation_rooms r
                   JOIN bridge_conversation_messages m
                     ON m.room_id = r.room_id AND m.removed_at IS NULL
                  WHERE r.actor_id = ?1 AND (?2 IS NULL OR r.bridge_principal_id = ?2)
                    AND r.removed_at IS NULL
                  GROUP BY r.room_id
                  ORDER BY MAX(m.received_at) DESC, r.room_id ASC",
            )
            .context("prepare bridged room summaries")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor_id[..], principal_id], |r| {
                let participants: String = r.get(2)?;
                Ok(BridgedRoomSummary {
                    bridge_id: r.get(0)?,
                    far_room_id: r.get(1)?,
                    participants: serde_json::from_str(&participants).unwrap_or_default(),
                    message_count: r.get(3)?,
                    last_received_at: r.get(4)?,
                    last_sealed: r.get(5)?,
                    last_outbound: r.get::<_, Option<String>>(6)?.as_deref() == Some("out"),
                })
            })
            .context("query bridged room summaries")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect bridged room summaries")?;
        Ok(rows)
    }

    /// Up to `limit` undrained items a principal holds across every account,
    /// oldest first — what an in-process leg drains.
    pub async fn fetch_principal_outbox(
        &self,
        principal_id: &[u8],
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, BridgedOutboxRow)>> {
        self.fetch_principal_outbox_after(principal_id, 0, limit)
            .await
    }

    /// [`Self::fetch_principal_outbox`] starting after message id `after_id`
    /// — a drain pages past items it deferred instead of re-reading them.
    pub async fn fetch_principal_outbox_after(
        &self,
        principal_id: &[u8],
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, BridgedOutboxRow)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT o.actor_id, o.message_id, r.far_room_id, o.ciphertext, o.queued_at
                   FROM bridge_conversation_outbox o
                   JOIN bridge_conversation_rooms r ON r.room_id = o.room_id
                  WHERE o.bridge_principal_id = ?1 AND o.message_id > ?3
                  ORDER BY o.message_id ASC LIMIT ?2",
            )
            .context("prepare principal outbox")?;
        let rows = stmt
            .query_map(rusqlite::params![principal_id, limit, after_id], |r| {
                Ok((
                    r.get(0)?,
                    BridgedOutboxRow {
                        id: r.get(1)?,
                        far_room_id: r.get(2)?,
                        ciphertext: r.get(3)?,
                        queued_at: r.get(4)?,
                    },
                ))
            })
            .context("query principal outbox")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect principal outbox")?;
        Ok(rows)
    }

    /// [`first_party_bridge_key`] on this database.
    pub async fn first_party_bridge_key(&self, bridge_id: &str) -> Result<([u8; 32], [u8; 32])> {
        let conn = self.conn.lock().await;
        first_party_bridge_key(&conn, bridge_id)
    }
}

#[cfg(test)]
mod leg_block_tests {
    use super::*;

    /// Each first-party leg's grammar compiles under the manifest's bound and
    /// admits the address its rooms key on — a grammar that does not compile
    /// matches nothing, and `rooms.open` would refuse every address.
    #[test]
    fn each_leg_grammar_admits_its_own_address() {
        assert!(bluesky_leg_block().address_matches("did:plc:ewvi7nxzyoun6zhxrhs64oiz"));
        assert!(bluesky_leg_block().address_matches("did:web:example.com"));
        for handle in ["alice.bsky.social", "@Alice.Example.Com"] {
            assert!(bluesky_leg_block().address_matches(handle), "{handle}");
        }
        for not_one in ["alice", "@alice@remote.example", "-a.example"] {
            assert!(!bluesky_leg_block().address_matches(not_one), "{not_one}");
        }
        let ap = activitypub_leg_block_with(false);
        assert!(ap.address_matches("https://remote.example/users/bob"));
        assert!(ap.address_matches("@bob@remote.example"));
        assert!(ap.address_matches("@Bob@Social.Example"));
        for not_one in [
            "http://remote.example/users/bob",
            "bob@remote.example",
            "@bob",
            "@bob@localhost",
            "@bob@remote.example/x",
            "alice.bsky.social",
        ] {
            assert!(!ap.address_matches(not_one), "{not_one}");
        }
    }

    /// The shipped ActivityPub grammar admits no loopback spelling — `https`
    /// and a dotted DNS host only (ruling 2 (d), and the SSRF posture of
    /// `network-exposure.md`); only the loopback test hook widens it, and
    /// only to loopback: a private-range host stays refused even then.
    #[test]
    fn activitypub_grammar_admits_loopback_under_the_test_hook_only() {
        let shipped = activitypub_leg_block_with(false);
        let hooked = activitypub_leg_block_with(true);
        for address in [
            "@frank@127.0.0.1:41234",
            "@frank@localhost:41234",
            "http://127.0.0.1:41234/users/frank",
        ] {
            assert!(!shipped.address_matches(address), "{address}");
            assert!(hooked.address_matches(address), "{address}");
        }
        for address in ["https://remote.example/users/bob", "@bob@remote.example"] {
            assert!(hooked.address_matches(address), "{address}");
        }
        for not_one in [
            "@bob@10.0.0.1:80",
            "@bob@remote.example:8443",
            "http://remote.example/users/bob",
            "http://10.0.0.1/users/bob",
            "bob@127.0.0.1:41234",
        ] {
            assert!(!hooked.address_matches(not_one), "{not_one}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::kind_manifest::BridgeCapabilityValue;

    const ALICE: [u8; 32] = [0xA1; 32];
    const BOB: [u8; 32] = [0xB2; 32];
    const PRINCIPAL: &[u8] = &[0x51; 16];
    const KEY: [u8; 32] = [0x99; 32];

    fn block() -> BridgeBlock {
        BridgeBlock {
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
        }
    }

    async fn room(db: &CacheDb, actor: &[u8; 32], far: &str) -> BridgedRoomRow {
        let b = block();
        let seat = BridgeSeat {
            principal_id: PRINCIPAL,
            bridge: &b,
            bridge_x25519: &KEY,
        };
        let shape = RoomShape {
            participants: vec!["@bob:example.org".into()],
            self_address: Some("@alice:example.org".into()),
            ..Default::default()
        };
        db.upsert_bridged_room(actor, &seat, far, &shape)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn a_room_is_born_transport_only_with_the_user_and_the_bridge_seated_once() {
        let db = CacheDb::open_in_memory().unwrap();
        let first = room(&db, &ALICE, "!r:example.org").await;
        let again = room(&db, &ALICE, "!r:example.org").await;
        assert_eq!(first.room_id, again.room_id, "find-or-mint is idempotent");
        assert_ne!(
            room(&db, &BOB, "!r:example.org").await.room_id,
            first.room_id
        );
        let conn = db.conn.lock().await;
        let class: String = conn
            .query_row(
                "SELECT class FROM rooms WHERE room_id = ?1",
                [&first.room_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(class, "transport_only");
        let mut seats: Vec<(Vec<u8>, String)> = conn
            .prepare("SELECT principal_id, principal_kind FROM room_members WHERE room_id = ?1")
            .unwrap()
            .query_map([&first.room_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        seats.sort();
        assert_eq!(
            seats,
            vec![
                (PRINCIPAL.to_vec(), "bridge".into()),
                (ALICE.to_vec(), "user".into())
            ]
        );
    }

    #[tokio::test]
    async fn deposits_dedupe_and_read_back_in_arrival_order_with_the_sent_copy() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        // The far side claims a later time for the first message than the
        // second: arrival order wins.
        let (_, a) = db
            .deposit_bridged_message(
                &ALICE,
                PRINCIPAL,
                "matrix",
                "!r:example.org",
                "$1",
                "@bob:example.org",
                b"one",
                9_000,
            )
            .await
            .unwrap();
        let (_, b) = db
            .deposit_bridged_message(
                &ALICE,
                PRINCIPAL,
                "matrix",
                "!r:example.org",
                "$2",
                "@bob:example.org",
                b"two",
                1_000,
            )
            .await
            .unwrap();
        let (_, dup) = db
            .deposit_bridged_message(
                &ALICE,
                PRINCIPAL,
                "matrix",
                "!r:example.org",
                "$1",
                "@bob:example.org",
                b"one again",
                0,
            )
            .await
            .unwrap();
        let Deposited::Stored(a) = a else {
            panic!("stored")
        };
        assert!(matches!(b, Deposited::Stored(_)));
        assert_eq!(dup, Deposited::Duplicate(a));
        let sent = db
            .send_bridged_message(&ALICE, &r, b"to-bridge", b"to-self")
            .await
            .unwrap();
        let rows = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 100)
            .await
            .unwrap();
        let bodies: Vec<&[u8]> = rows.iter().map(|m| m.sealed_content.as_slice()).collect();
        assert_eq!(bodies, [&b"one"[..], b"two", b"to-self"]);
        let last = rows.last().unwrap();
        assert_eq!(
            (last.id, last.outbound, last.sender.as_str()),
            (sent, true, "@alice:example.org")
        );
        // The whole-inbox read sees the same rows; another account sees none.
        assert_eq!(
            db.fetch_bridged_inbox(&ALICE, None, a, 100)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            db.fetch_bridged_inbox(&BOB, None, 0, 100)
                .await
                .unwrap()
                .is_empty()
        );
        // A room the principal does not serve refuses.
        let err = db
            .deposit_bridged_message(&ALICE, PRINCIPAL, "matrix", "!nope", "$9", "@x", b"x", 0)
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<BridgedRefused>(),
            Some(&BridgedRefused::NoSuchRoom)
        );
    }

    /// `n` inbound rows into a room, far ids `<tag>-0 ..`, written straight to
    /// the table in one transaction — the caps are tens of thousands of rows.
    async fn fill(db: &CacheDb, actor: &[u8; 32], room_id: &[u8], tag: &str, n: i64) {
        let mut conn = db.conn.lock().await;
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO bridge_conversation_messages
                        (room_id, actor_id, direction, far_message_id, sender, sealed_content,
                         created_at, received_at)
                     VALUES (?1, ?2, 'in', ?3, '@bob:example.org', x'01', 0, 0)",
                )
                .unwrap();
            for i in 0..n {
                stmt.execute(rusqlite::params![room_id, &actor[..], format!("{tag}-{i}")])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    async fn deposit(
        db: &CacheDb,
        actor: &[u8; 32],
        far_room: &str,
        far_id: &str,
    ) -> Result<Deposited> {
        db.deposit_bridged_message(
            actor,
            PRINCIPAL,
            "matrix",
            far_room,
            far_id,
            "@bob:example.org",
            b"x",
            0,
        )
        .await
        .map(|(_, d)| d)
    }

    fn is_full(r: Result<Deposited>) -> bool {
        r.err()
            .and_then(|e| e.downcast_ref::<BridgedRefused>().copied())
            == Some(BridgedRefused::Full)
    }

    async fn first_id(db: &CacheDb, room_id: &[u8]) -> i64 {
        db.conn
            .lock()
            .await
            .query_row(
                "SELECT MIN(id) FROM bridge_conversation_messages
                  WHERE room_id = ?1 AND removed_at IS NULL",
                [room_id],
                |r| r.get(0),
            )
            .unwrap()
    }

    async fn rows_in(db: &CacheDb, room_id: &[u8]) -> i64 {
        db.conn
            .lock()
            .await
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages WHERE room_id = ?1",
                [room_id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `bridged-adapter.md` § Removing bridged rows, rule 2: a remove frees
    /// exactly what it removes — the room's slot, the account's, the leg's —
    /// and keeps the dedup, so a re-served far message stores nothing.
    #[tokio::test]
    async fn a_remove_frees_every_cap_arm_and_keeps_the_dedup() {
        let db = CacheDb::open_in_memory().unwrap();
        // The room arm.
        let r0 = room(&db, &ALICE, "!r0").await;
        fill(&db, &ALICE, &r0.room_id, "r0", MAX_BRIDGED_INBOUND_PER_ROOM).await;
        assert!(is_full(deposit(&db, &ALICE, "!r0", "new").await));
        let oldest = first_id(&db, &r0.room_id).await;
        assert_eq!(
            db.remove_bridged_messages(&ALICE, &r0.room_id, &[oldest])
                .await
                .unwrap(),
            1
        );
        // A repeat counts nothing.
        assert_eq!(
            db.remove_bridged_messages(&ALICE, &r0.room_id, &[oldest, -7])
                .await
                .unwrap(),
            0
        );
        let before = rows_in(&db, &r0.room_id).await;
        assert_eq!(
            deposit(&db, &ALICE, "!r0", "r0-0").await.unwrap(),
            Deposited::Duplicate(oldest),
            "the tombstone keeps the dedup"
        );
        assert_eq!(rows_in(&db, &r0.room_id).await, before);
        assert!(matches!(
            deposit(&db, &ALICE, "!r0", "new").await.unwrap(),
            Deposited::Stored(_)
        ));
        // The account's arm: four more full rooms make 50,000 live rows.
        let mut rooms = vec![r0];
        for i in 1..5 {
            let far = format!("!r{i}");
            let r = room(&db, &ALICE, &far).await;
            fill(&db, &ALICE, &r.room_id, &far, MAX_BRIDGED_INBOUND_PER_ROOM).await;
            rooms.push(r);
        }
        room(&db, &ALICE, "!r5").await;
        assert!(is_full(deposit(&db, &ALICE, "!r5", "a").await));
        let id = first_id(&db, &rooms[1].room_id).await;
        db.remove_bridged_messages(&ALICE, &rooms[1].room_id, &[id])
            .await
            .unwrap();
        assert!(matches!(
            deposit(&db, &ALICE, "!r5", "a").await.unwrap(),
            Deposited::Stored(_)
        ));
        // The leg's arm: four more accounts at 50,000 make the leg's 250,000.
        let mut other = Vec::new();
        for a in 1..5u8 {
            let actor = [a; 32];
            for i in 0..5 {
                let far = format!("!r{i}");
                let r = room(&db, &actor, &far).await;
                fill(&db, &actor, &r.room_id, &far, MAX_BRIDGED_INBOUND_PER_ROOM).await;
                other.push((actor, r));
            }
        }
        room(&db, &BOB, "!b").await;
        assert!(is_full(deposit(&db, &BOB, "!b", "b").await));
        let (actor, r) = &other[0];
        let id = first_id(&db, &r.room_id).await;
        db.remove_bridged_messages(actor, &r.room_id, &[id])
            .await
            .unwrap();
        assert!(matches!(
            deposit(&db, &BOB, "!b", "b").await.unwrap(),
            Deposited::Stored(_)
        ));
        // Another account's room is no room of the caller's.
        let err = db
            .remove_bridged_messages(&BOB, &rooms[0].room_id, &[1])
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<BridgedRefused>(),
            Some(&BridgedRefused::NoSuchRoom)
        );
    }

    /// Rules 1, 4 and 5: a removed room leaves the list, keeps its material
    /// and its dedup, serves its tombstones flagged and empty, drops a receipt
    /// or a members report, and lists again — holding only the new row — on a
    /// new live row; a duplicate never reopens it.
    #[tokio::test]
    async fn a_removed_room_is_hidden_and_a_new_row_reopens_it() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        assert_eq!(r.last_removed_at, 0);
        for far in ["$1", "$2"] {
            deposit(&db, &ALICE, "!r:example.org", far).await.unwrap();
        }
        let sent = db
            .send_bridged_message(&ALICE, &r, b"to-bridge", b"to-self")
            .await
            .unwrap();
        assert_eq!(db.remove_bridged_room(&ALICE, &r.room_id).await.unwrap(), 3);
        assert_eq!(db.remove_bridged_room(&ALICE, &r.room_id).await.unwrap(), 0);
        assert!(db.list_bridged_rooms(&ALICE).await.unwrap().is_empty());
        assert!(
            db.summarize_bridged_rooms(&ALICE, None)
                .await
                .unwrap()
                .is_empty()
        );
        let held = db
            .get_bridged_room(&ALICE, &r.room_id)
            .await
            .unwrap()
            .unwrap();
        assert!(held.last_removed_at > 0);
        assert_eq!(held.bridge_x25519, KEY, "the room's material stays");
        // The outbox stands: the user removed their copy, not the message.
        assert_eq!(
            db.fetch_bridged_outbox(&ALICE, PRINCIPAL, 0, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        // A receipt or a members report naming the removed row is dropped.
        assert!(
            db.record_bridged_receipt(&ALICE, PRINCIPAL, "matrix", sent, "delivered")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.set_bridged_room_participants(
                &ALICE,
                PRINCIPAL,
                "matrix",
                "!r:example.org",
                &["@carol:example.org".into()]
            )
            .await
            .unwrap()
        );
        let tombstones = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 100)
            .await
            .unwrap();
        assert_eq!(tombstones.len(), 3);
        assert!(
            tombstones
                .iter()
                .all(|m| m.removed && m.sealed_content.is_empty() && m.receipt.is_none())
        );
        // A re-served far message is a duplicate and reopens nothing.
        assert!(matches!(
            deposit(&db, &ALICE, "!r:example.org", "$1").await.unwrap(),
            Deposited::Duplicate(_)
        ));
        assert!(db.list_bridged_rooms(&ALICE).await.unwrap().is_empty());
        // A new far message reopens the room, holding only what arrived since.
        let Deposited::Stored(new) = deposit(&db, &ALICE, "!r:example.org", "$3").await.unwrap()
        else {
            panic!("stored")
        };
        let listed = db.list_bridged_rooms(&ALICE).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].participants, vec!["@bob:example.org".to_string()]);
        let live: Vec<i64> = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| !m.removed)
            .map(|m| m.id)
            .collect();
        assert_eq!(live, vec![new]);
        // The user's own open reopens a removed room too; a send likewise.
        db.remove_bridged_room(&ALICE, &r.room_id).await.unwrap();
        assert!(db.reopen_bridged_room(&ALICE, &r.room_id).await.unwrap());
        assert!(!db.reopen_bridged_room(&ALICE, &r.room_id).await.unwrap());
        assert_eq!(db.list_bridged_rooms(&ALICE).await.unwrap().len(), 1);
        db.remove_bridged_room(&ALICE, &r.room_id).await.unwrap();
        db.send_bridged_message(&ALICE, &r, b"to-bridge", b"to-self")
            .await
            .unwrap();
        assert_eq!(db.list_bridged_rooms(&ALICE).await.unwrap().len(), 1);
    }

    /// Rule 3: a room keeps at most `MAX_BRIDGED_INBOUND_PER_ROOM`
    /// tombstones, the oldest pruned first, so its rows never exceed twice
    /// its cap.
    #[tokio::test]
    async fn tombstones_are_bounded_oldest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r").await;
        fill(
            &db,
            &ALICE,
            &r.room_id,
            "first",
            MAX_BRIDGED_INBOUND_PER_ROOM,
        )
        .await;
        db.remove_bridged_room(&ALICE, &r.room_id).await.unwrap();
        fill(
            &db,
            &ALICE,
            &r.room_id,
            "second",
            MAX_BRIDGED_INBOUND_PER_ROOM,
        )
        .await;
        assert_eq!(
            rows_in(&db, &r.room_id).await,
            2 * MAX_BRIDGED_INBOUND_PER_ROOM
        );
        // One more remove: twenty thousand tombstones, half of them pruned.
        let id = first_id(&db, &r.room_id).await;
        db.remove_bridged_messages(&ALICE, &r.room_id, &[id])
            .await
            .unwrap();
        assert!(rows_in(&db, &r.room_id).await <= 2 * MAX_BRIDGED_INBOUND_PER_ROOM);
        let tombstones: i64 = db
            .conn
            .lock()
            .await
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages
                  WHERE room_id = ?1 AND removed_at IS NOT NULL",
                [&r.room_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tombstones, MAX_BRIDGED_INBOUND_PER_ROOM);
        // The oldest far ids went; the newest tombstone still dedups.
        assert!(matches!(
            deposit(&db, &ALICE, "!r", "second-0").await.unwrap(),
            Deposited::Duplicate(_)
        ));
        assert!(matches!(
            deposit(&db, &ALICE, "!r", "first-0").await.unwrap(),
            Deposited::Stored(_)
        ));
    }

    #[tokio::test]
    async fn the_outbox_drains_by_ack_and_receipts_only_move_forward() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        let id = db
            .send_bridged_message(&ALICE, &r, b"sealed", b"self")
            .await
            .unwrap();
        let items = db
            .fetch_bridged_outbox(&ALICE, PRINCIPAL, 0, 10)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            (items[0].id, items[0].far_room_id.as_str()),
            (id, "!r:example.org")
        );
        // Another principal drains nothing and acks nothing.
        assert!(
            db.fetch_bridged_outbox(&ALICE, &[0x52; 16], 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.ack_bridged_outbox(&ALICE, &[0x52; 16], &[id])
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            db.ack_bridged_outbox(&ALICE, PRINCIPAL, &[id])
                .await
                .unwrap(),
            1
        );
        assert!(
            db.fetch_bridged_outbox(&ALICE, PRINCIPAL, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        // Receipts: read after delivered moves; delivered after read does not.
        assert!(
            db.record_bridged_receipt(&ALICE, PRINCIPAL, "matrix", id, "read")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.record_bridged_receipt(&ALICE, PRINCIPAL, "matrix", id, "delivered")
                .await
                .unwrap()
                .is_none()
        );
        let row = &db.fetch_bridged_inbox(&ALICE, None, 0, 10).await.unwrap()[0];
        assert_eq!(row.receipt.as_deref(), Some("read"));
    }

    /// The deployment-seed rotation's hand-off window (`box-recovery.md`
    /// § Deployment-seed rotation → *The bounded hand-off window*): the leg's
    /// key has no boot moment, so its first mint can follow a committed
    /// ceremony while an outgoing generation still serves. It reads the seed
    /// from `nest_keypair` on the connection it inserts on, so it seals under
    /// the successor, and the next rotation re-keys it with the key itself
    /// unchanged.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_leg_key_mint_in_the_rotation_window_seals_under_the_successor_seed() {
        use crate::test_support::{every_row_opens_under, seat_deployment_seed};
        use zeroize::Zeroizing;

        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        seat_deployment_seed(&db, &a).await;
        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        let minted = db
            .first_party_bridge_key(NOSTR_LEG_BRIDGE_ID)
            .await
            .expect("the leg mints its key");
        assert!(
            every_row_opens_under(
                &db,
                "first_party_bridge_keys",
                "secret_wrapped",
                crate::nest_kek::FIRST_PARTY_BRIDGE_CONTEXT,
                &b,
            )
            .await,
            "a first mint after the ceremony sealed the leg's key under the retired seed"
        );

        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation runs")
            .expect("and commits — the leg's key does not wedge the walk");
        assert_eq!(
            db.first_party_bridge_key(NOSTR_LEG_BRIDGE_ID)
                .await
                .expect("the key opens under the new seed"),
            minted,
            "a rotation re-wraps the key; the key itself stays"
        );
    }

    #[tokio::test]
    async fn a_members_report_replaces_only_the_far_participants() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        assert!(
            db.set_bridged_room_participants(
                &ALICE,
                PRINCIPAL,
                "matrix",
                "!r:example.org",
                &["@carol:example.org".into()]
            )
            .await
            .unwrap()
        );
        let after = db
            .get_bridged_room(&ALICE, &r.room_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.participants, vec!["@carol:example.org".to_string()]);
        assert!(
            !db.set_bridged_room_participants(&ALICE, PRINCIPAL, "matrix", "!other", &[])
                .await
                .unwrap()
        );
        let seats: i64 = db
            .conn
            .lock()
            .await
            .query_row(
                "SELECT COUNT(*) FROM room_members WHERE room_id = ?1",
                [&r.room_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seats, 2);
    }

    /// `apps/bridges.md` § Phase G → *When the bridge stops serving*, rule 2:
    /// a room's serving principal is the one declaring the room's `bridge.id`,
    /// so the same principal reporting under another id — a re-consent that
    /// moved it — finds, reports on and receipts none of the old id's rooms.
    #[tokio::test]
    async fn a_principal_under_another_bridge_id_reaches_none_of_the_old_ids_rooms() {
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        let id = db
            .send_bridged_message(&ALICE, &r, b"sealed", b"self")
            .await
            .unwrap();
        assert!(
            db.get_bridged_room_by_far_id(&ALICE, PRINCIPAL, "signal", "!r:example.org")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !db.set_bridged_room_participants(
                &ALICE,
                PRINCIPAL,
                "signal",
                "!r:example.org",
                &["@mallory:example.org".into()]
            )
            .await
            .unwrap()
        );
        assert!(
            db.record_bridged_receipt(&ALICE, PRINCIPAL, "signal", id, "delivered")
                .await
                .unwrap()
                .is_none()
        );
        let after = db
            .get_bridged_room(&ALICE, &r.room_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.participants, vec!["@bob:example.org".to_string()]);
        let row = &db.fetch_bridged_inbox(&ALICE, None, 0, 10).await.unwrap()[0];
        assert_eq!(row.receipt, None);
        // Under the room's own id the same calls reach it.
        assert!(
            db.get_bridged_room_by_far_id(&ALICE, PRINCIPAL, "matrix", "!r:example.org")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.record_bridged_receipt(&ALICE, PRINCIPAL, "matrix", id, "delivered")
                .await
                .unwrap()
                .is_some()
        );
    }

    /// `apps/bridges.md` § Phase G → *When the bridge stops serving*: a
    /// principal's end deletes its undrained outbox and nothing else, stamping
    /// each Sent row; a principal that comes to declare the bridge id adopts
    /// the rooms — row, key, vector and the floor seat.
    #[tokio::test]
    async fn a_principals_end_deletes_only_its_undrained_outbox_and_a_new_principal_adopts_the_rooms()
     {
        const NEXT: &[u8] = &[0x52; 16];
        const NEXT_KEY: [u8; 32] = [0x77; 32];
        let db = CacheDb::open_in_memory().unwrap();
        let r = room(&db, &ALICE, "!r:example.org").await;
        let drained = db
            .send_bridged_message(&ALICE, &r, b"to-bridge-1", b"to-self-1")
            .await
            .unwrap();
        let stranded = db
            .send_bridged_message(&ALICE, &r, b"to-bridge-2", b"to-self-2")
            .await
            .unwrap();
        assert_eq!(
            db.ack_bridged_outbox(&ALICE, PRINCIPAL, &[drained])
                .await
                .unwrap(),
            1
        );

        let ended = {
            let mut conn = db.conn.lock().await;
            let tx = conn.transaction().unwrap();
            let ended = end_bridged_outbox_in_tx(&tx, &ALICE, PRINCIPAL, 1_000).unwrap();
            // Idempotent: a second end finds nothing.
            assert!(
                end_bridged_outbox_in_tx(&tx, &ALICE, PRINCIPAL, 1_001)
                    .unwrap()
                    .is_empty()
            );
            tx.commit().unwrap();
            ended
        };
        assert_eq!(ended, vec![r.room_id.clone()]);
        assert!(
            db.fetch_bridged_outbox(&ALICE, PRINCIPAL, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        // The room and both Sent copies stay; only the stranded one says so.
        assert_eq!(db.list_bridged_rooms(&ALICE).await.unwrap().len(), 1);
        let rows = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 10)
            .await
            .unwrap();
        let marked: Vec<(i64, bool)> = rows.iter().map(|m| (m.id, m.undelivered)).collect();
        assert_eq!(marked, vec![(drained, false), (stranded, true)]);

        // Another principal declares the bridge id: the rooms follow it.
        let b = block();
        let seat = BridgeSeat {
            principal_id: NEXT,
            bridge: &b,
            bridge_x25519: &NEXT_KEY,
        };
        {
            let mut conn = db.conn.lock().await;
            let tx = conn.transaction().unwrap();
            assert_eq!(
                rebind_bridged_rooms_in_tx(&tx, &ALICE, "matrix", &seat).unwrap(),
                1
            );
            // Another bridge id's rooms are untouched by name.
            assert_eq!(
                rebind_bridged_rooms_in_tx(&tx, &ALICE, "signal", &seat).unwrap(),
                0
            );
            tx.commit().unwrap();
        }
        let r = db
            .get_bridged_room(&ALICE, &r.room_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.bridge_principal_id, NEXT);
        assert_eq!(r.bridge_x25519, NEXT_KEY);
        let conn = db.conn.lock().await;
        let seat_now: Vec<u8> = conn
            .query_row(
                "SELECT principal_id FROM room_members
                  WHERE room_id = ?1 AND principal_kind = 'bridge'",
                [&r.room_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seat_now, NEXT);
        drop(conn);
        // The old principal's view is empty; a new send queues for the new one.
        assert!(
            db.get_bridged_room_by_far_id(&ALICE, PRINCIPAL, "matrix", "!r:example.org")
                .await
                .unwrap()
                .is_none()
        );
        let id = db
            .send_bridged_message(&ALICE, &r, b"to-bridge-3", b"to-self-3")
            .await
            .unwrap();
        let queued: Vec<i64> = db
            .fetch_bridged_outbox(&ALICE, NEXT, 0, 10)
            .await
            .unwrap()
            .iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(queued, vec![id]);
    }
}
