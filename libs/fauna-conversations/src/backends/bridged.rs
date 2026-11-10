//! The bridged rail — one backend for every bridge (`docs/goal/ui/conversations.md`
//! § Where logic lives → *The `Bridged` adapter*).
//!
//! A bridge is a principal with keys that carries a far network's conversations
//! into Fauna; per-bridge identity — id, label, glyph, capability vector, the
//! X25519 key outbound items are sealed to — comes from the bridge's manifest
//! (`docs/goal/architecture/third-party.md` § The manifest → *The `bridge`
//! block*) by way of the user-side `rooms.list` read, which the glue loads into
//! this backend's registry ([`BridgedBackend::set_identities`]). There is no
//! per-bridge Rust and no per-bridge app change.
//!
//! What the backend answers, per ruling 2 of that section:
//! - **identity** — [`RailBackend::bridge_identity`], which the manager projects
//!   onto the thread's `bridge` and `glyph`;
//! - **capabilities** — the vector declared for the thread's bridge, overlaid on
//!   the rail's most-restrictive answer; `encryption` is never declared, it is
//!   the room's derived class;
//! - **the room** — [`crate::room::transport_room`]: the bridge principal sits
//!   on the floor, so the class is transport-only by construction — and a room
//!   no live bridge serves says so ([`crate::room::RoomSnapshot::bridge_disconnected`]);
//! - **addresses** — resolved by the nest against each bridge's grammar
//!   ([`BridgedSink::resolve`]); no app compiles a third party's pattern.
//!
//! The wire, the HPKE seal and the open live in the glue behind the
//! [`BridgedSink`] / [`BridgedSource`] seams, the discipline mail's
//! `OutboundMailSink` / `InboundMailSource` set: this crate takes no wire, HPKE
//! or regex dependency.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::address::{Rail, TypedAddress};
use crate::backend::{
    BackendError, BridgedOutbound, BridgedRecord, BridgedRoomRef, BridgedSink, BridgedSource,
    InboundBucket, MailFeed, RailBackend, RailInboundMessage, ResolveResult, ResolvedAttachment,
    SendOutcome,
};
use crate::capabilities::{ThreadCapabilities, derive_capabilities};
use crate::compose::ComposeState;
use crate::manager::ConversationsManager;
use crate::message::{BodyFormat, MessageBadges, MessageId};
use crate::snapshot::{GuardianState, ThreadDetail};
use async_trait::async_trait;
use fauna_core::source_glyph::{BridgeIdentitySnapshot, SourceGlyph};

/// One bridge as the registry holds it — a `rooms.list` row's bridge half.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeIdentity {
    /// The manifest's `bridge.id`.
    pub id: String,
    /// The declared display label.
    pub label: String,
    /// The declared glyph (`SourceGlyph::from_id` of the manifest's id).
    pub glyph: SourceGlyph,
    /// The declared capability vector — the manifest's `capabilities`, which is
    /// the [`ThreadCapabilities`] record minus `encryption`. Whatever this
    /// carries in `encryption` is ignored: the room's class decides it.
    pub capabilities: ThreadCapabilities,
    /// The bridge principal's X25519 public key — what an outbound item is
    /// sealed to.
    pub bridge_x25519: [u8; 32],
}

impl BridgeIdentity {
    fn snapshot(&self) -> BridgeIdentitySnapshot {
        BridgeIdentitySnapshot {
            id: self.id.clone(),
            label: self.label.clone(),
            glyph: self.glyph,
        }
    }
}

/// One bridged room as the user-side `rooms.list` reports it — the row the
/// glue hands across [`BridgedSource::rooms`], with nothing a wire type names.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgedRoomRecord {
    /// The manifest's `bridge.id` of the bridge the room rides.
    pub bridge_id: String,
    /// The bridge as it declared itself — `None` for a room no live principal
    /// serves (`architecture/apps/bridges.md` § Phase G → *When the bridge
    /// stops serving*): its thread then falls to the rail's most-restrictive
    /// vector and the generic glyph, by ruling 2 (b).
    pub identity: Option<BridgeIdentity>,
    /// The far participants, the account's own address excluded.
    pub participants: Vec<String>,
    /// The account's own far address, once the bridge has reported it.
    pub self_address: Option<String>,
    /// The family gate's marker for the room's peer, as the nest computed it.
    pub guardian_state: Option<GuardianState>,
    /// The nest's `disconnected`: no live principal serves the room — what
    /// paints `thread-room-notice`'s `bridge-disconnected` state. `identity`
    /// is `None` for such a room too; this is the fact, that the consequence.
    pub disconnected: bool,
    /// The row ids of this room's Sent copies the nest stamped undelivered —
    /// what the glue found re-reading a room it saw disconnected. The inbox
    /// is read by cursor, so a stamp that lands on a row this device has
    /// already read is never served again on the incremental read; this is
    /// how it reaches the thread anyway. Empty for a connected room.
    pub undelivered_rows: Vec<i64>,
    /// The room's newest local remove, unix ms; 0 when none — the nest's
    /// `last_removed_at` (`bridged-adapter.md` § Removing bridged rows, rule
    /// 5). The glue re-reads the room when it moves past the value it last
    /// saw; the backend itself does not read it.
    pub last_removed_at: i64,
    /// The row ids of this room the nest serves as tombstones — what the
    /// glue's re-read found when `last_removed_at` moved: rows another device
    /// removed that this one already read, which the incremental read never
    /// serves again. The driver has the manager forget them. Empty unless the
    /// value moved.
    pub removed_rows: Vec<i64>,
}

/// The key a room's guardian marker is kept under: its bridge and its far
/// participants, sorted — what a thread's participants reduce to once the
/// account's own address is dropped.
type RoomKey = (String, Vec<String>);

pub struct BridgedBackend {
    sink: Arc<dyn BridgedSink>,
    registry: RwLock<HashMap<String, BridgeIdentity>>,
    /// The family gate's marker per room, from the newest `rooms.list` read.
    guardian: RwLock<HashMap<RoomKey, GuardianState>>,
    /// The rooms no live principal serves, from the newest `rooms.list` read.
    disconnected: RwLock<HashSet<RoomKey>>,
    /// Every room the newest `rooms.list` read listed — what tells a room
    /// gone from the list (removed, here or on another device) at the next
    /// read ([`Self::set_rooms`]'s answer).
    listed: RwLock<HashSet<RoomKey>>,
    /// The Sent copies the nest stamped undelivered, by message id — only
    /// ever grows: a stamp is final, and a later adoption reconnects the
    /// room without delivering what was dropped.
    undelivered: RwLock<HashSet<MessageId>>,
    /// The account's own address on each bridge's far network, as the bridge
    /// reports it — learned from a send ([`crate::backend::BridgedSent`]) and
    /// from the inbox's own Sent rows; what tells the user's own message from a
    /// peer's when a row is bucketed.
    self_addresses: RwLock<HashMap<String, String>>,
}

impl BridgedBackend {
    pub fn new(sink: Arc<dyn BridgedSink>) -> Self {
        Self {
            sink,
            registry: RwLock::new(HashMap::new()),
            guardian: RwLock::new(HashMap::new()),
            disconnected: RwLock::new(HashSet::new()),
            listed: RwLock::new(HashSet::new()),
            undelivered: RwLock::new(HashSet::new()),
            self_addresses: RwLock::new(HashMap::new()),
        }
    }

    /// Load one `rooms.list` read: the registry becomes the identities of the
    /// rooms a live principal serves ([`Self::set_identities`]), each room's
    /// guardian marker and disconnected flag replace the last read's — so a
    /// marker the nest no longer computes (the knob relaxed, the guardian
    /// decided) or a room a bridge adopted again leaves with the next read —
    /// the undelivered stamps the glue found are kept, and the account's own
    /// far addresses are learned.
    ///
    /// Answers the rooms the previous read listed and this one does not — a
    /// room removed from the nest, here or on another device
    /// (`bridged-adapter.md` § Removing bridged rows, rule 5) — whose threads
    /// the driver drops ([`poll_inbound_bridged`]). Only a room once listed
    /// can vanish: a thread whose room this device has never seen listed is
    /// left alone.
    pub fn set_rooms(&self, rooms: Vec<BridgedRoomRecord>) -> Vec<BridgedRoomRef> {
        let mut guardian = HashMap::new();
        let mut disconnected = HashSet::new();
        let mut listed = HashSet::new();
        let mut identities: HashMap<String, BridgeIdentity> = HashMap::new();
        for room in rooms {
            if let Some(me) = &room.self_address {
                self.note_self_address(&room.bridge_id, me);
            }
            for row in &room.undelivered_rows {
                self.note_undelivered(*row);
            }
            let mut far = room.participants;
            far.sort();
            let key = (room.bridge_id.clone(), far);
            listed.insert(key.clone());
            if room.disconnected {
                disconnected.insert(key.clone());
            }
            if let Some(state) = room.guardian_state {
                guardian.insert(key, state);
            }
            if let Some(identity) = room.identity {
                identities.insert(identity.id.clone(), identity);
            }
        }
        *self.guardian.write().unwrap() = guardian;
        *self.disconnected.write().unwrap() = disconnected;
        self.set_identities(identities.into_values().collect());
        let previous = std::mem::replace(&mut *self.listed.write().unwrap(), listed.clone());
        let mut vanished: Vec<BridgedRoomRef> = previous
            .difference(&listed)
            .map(|(bridge_id, participants)| BridgedRoomRef {
                bridge_id: bridge_id.clone(),
                participants: participants.clone(),
            })
            .collect();
        vanished
            .sort_by(|a, b| (&a.bridge_id, &a.participants).cmp(&(&b.bridge_id, &b.participants)));
        vanished
    }

    /// The room these `participants` make, as the seams name it — `None` for
    /// participants on no bridge.
    pub fn room_ref(&self, participants: &[TypedAddress]) -> Option<BridgedRoomRef> {
        self.room_key(participants)
            .map(|(bridge_id, participants)| BridgedRoomRef {
                bridge_id,
                participants,
            })
    }

    /// Record that the Sent row `row_id` was stamped undelivered.
    pub fn note_undelivered(&self, row_id: i64) {
        self.undelivered
            .write()
            .unwrap()
            .insert(bridged_message_id(row_id));
    }

    /// Replace the registry with `identities` — the bridges the glue read off
    /// the user-side `rooms.list`. A bridge no longer listed leaves: its
    /// threads fall back to the rail's most-restrictive vector and the generic
    /// glyph until it is listed again.
    pub fn set_identities(&self, identities: Vec<BridgeIdentity>) {
        *self.registry.write().unwrap() = identities
            .into_iter()
            .map(|identity| (identity.id.clone(), identity))
            .collect();
    }

    /// Record the account's own far-network address on `bridge_id`.
    pub fn note_self_address(&self, bridge_id: &str, address: &str) {
        if address.is_empty() {
            return;
        }
        self.self_addresses
            .write()
            .unwrap()
            .insert(bridge_id.to_string(), address.to_string());
    }

    fn identity(&self, bridge_id: &str) -> Option<BridgeIdentity> {
        self.registry.read().unwrap().get(bridge_id).cloned()
    }

    /// The key the room these `participants` make is kept under — its
    /// bridge and its far participants, the account's own address dropped.
    fn room_key(&self, participants: &[TypedAddress]) -> Option<RoomKey> {
        let bridge_id = bridge_id_of(participants)?;
        let mut far: Vec<String> = participants
            .iter()
            .filter(|p| !self.is_self(p))
            .filter_map(|p| match p {
                TypedAddress::Bridged { address, .. } => Some(address.clone()),
                _ => None,
            })
            .collect();
        far.sort();
        Some((bridge_id.to_string(), far))
    }

    fn is_self(&self, address: &TypedAddress) -> bool {
        let TypedAddress::Bridged { bridge_id, address } = address else {
            return false;
        };
        // An address-less party on a bridge is the account itself: the nest
        // stamps a Sent copy's sender empty until the bridge has reported the
        // account's far address, and no far peer is ever nameless.
        if address.is_empty() {
            return true;
        }
        self.self_addresses
            .read()
            .unwrap()
            .get(bridge_id)
            .is_some_and(|me| me == address)
    }
}

/// The send refusal for an account with no recipient key to seal its own Sent
/// copy to — the sentence the glue's [`BridgedSink::send`] answers with, kept
/// here so every app's glue says the same thing.
pub fn no_recipient_key_refusal() -> String {
    fauna_i18n::strings::conversations::unified::BRIDGED_NO_RECIPIENT_KEY.to_string()
}

/// The remove refusal for a room the nest does not list — the sentence the
/// glue's [`BridgedSink::remove_messages`] / [`BridgedSink::remove_room`]
/// answer with when no `rooms.list` row makes the thread's room, kept here so
/// every app's glue says the same thing.
pub fn room_not_known_refusal() -> String {
    fauna_i18n::strings::error::send::NOT_SUPPORTED.to_string()
}

/// The bridge a thread rides — the `bridge_id` of its first bridged
/// participant. A bridged room is one bridge's: every participant shares it.
pub fn bridge_id_of(participants: &[TypedAddress]) -> Option<&str> {
    participants.iter().find_map(|p| match p {
        TypedAddress::Bridged { bridge_id, .. } => Some(bridge_id.as_str()),
        _ => None,
    })
}

/// The [`MessageId`] of the bridged row `row_id` — one derivation for the send
/// ([`BridgedBackend::send`]) and the inbox's echo of the same row.
pub fn bridged_message_id(row_id: i64) -> MessageId {
    MessageId(format!("<{row_id}@bridged>"))
}

/// The bridged row a [`MessageId`] names — the inverse of
/// [`bridged_message_id`]; `None` for any other rail's id.
pub fn bridged_row_id(id: &MessageId) -> Option<i64> {
    id.0.strip_prefix('<')?
        .strip_suffix("@bridged>")?
        .parse()
        .ok()
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RailBackend for BridgedBackend {
    fn rail(&self) -> Rail {
        Rail::Bridged
    }

    /// The declared vector for the thread's bridge, overlaid on the rail's
    /// most-restrictive answer — the same read-time overlay
    /// `RoomSnapshot::gate` performs for roles. `encryption` stays the rail
    /// answer here and becomes the room's derived class in the manager's
    /// projection; a bridge never declares it.
    ///
    /// The declared `supports_message_delete` is masked to `false`: Phase G
    /// carries no far-side delete, so a declared `true` would offer a
    /// *Delete for everyone* that reaches no one; the local remove, derived
    /// from the rail and never declared, is offered in its place
    /// (`bridged-adapter.md` § Removing bridged rows, rule 1).
    fn capabilities(&self, thread: &ThreadDetail) -> ThreadCapabilities {
        let base = derive_capabilities(Rail::Bridged, thread.flavor.clone());
        match bridge_id_of(&thread.participants).and_then(|id| self.identity(id)) {
            Some(identity) => ThreadCapabilities {
                encryption: base.encryption,
                can_send: base.can_send,
                supports_local_remove: base.supports_local_remove,
                supports_message_delete: false,
                ..identity.capabilities
            },
            // No identity is the send's own refusal (`NotSupported`, below):
            // the bar is withheld rather than offered to fail.
            None => ThreadCapabilities {
                can_send: false,
                ..base
            },
        }
    }

    fn room_state(&self, thread: &ThreadDetail) -> Option<crate::room::RoomSnapshot> {
        let mut room = crate::room::transport_room(thread.participants.len());
        room.bridge_disconnected = self
            .room_key(&thread.participants)
            .is_some_and(|key| self.disconnected.read().unwrap().contains(&key));
        Some(room)
    }

    fn bridge_identity(&self, bridge_id: &str) -> Option<BridgeIdentitySnapshot> {
        self.identity(bridge_id).map(|identity| identity.snapshot())
    }

    fn bridge_identities(&self) -> Vec<BridgeIdentitySnapshot> {
        let mut all: Vec<BridgeIdentitySnapshot> = self
            .registry
            .read()
            .unwrap()
            .values()
            .map(BridgeIdentity::snapshot)
            .collect();
        all.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        all
    }

    fn guardian_state(&self, participants: &[TypedAddress]) -> Option<GuardianState> {
        let key = self.room_key(participants)?;
        self.guardian.read().unwrap().get(&key).copied()
    }

    fn message_undelivered(&self, id: &MessageId) -> bool {
        self.undelivered.read().unwrap().contains(id)
    }

    /// `conversation.messages.remove` over the sink, for the one row `message`
    /// names. A room the thread's participants do not make, or an id of no
    /// bridged row, is refused before the wire.
    async fn remove_message(
        &self,
        thread: &ThreadDetail,
        message: &MessageId,
    ) -> Result<(), BackendError> {
        let room = self
            .room_ref(&thread.participants)
            .ok_or(BackendError::NotSupported)?;
        let row = bridged_row_id(message).ok_or(BackendError::NotSupported)?;
        self.sink
            .remove_messages(room, vec![row])
            .await
            .map(|_removed| ())
            .map_err(BackendError::transport_from_seam)
    }

    /// `conversation.rooms.remove` over the sink, for the room the thread's
    /// participants make.
    async fn remove_thread(&self, thread: &ThreadDetail) -> Result<(), BackendError> {
        let room = self
            .room_ref(&thread.participants)
            .ok_or(BackendError::NotSupported)?;
        self.sink
            .remove_room(room)
            .await
            .map(|_removed| ())
            .map_err(BackendError::transport_from_seam)
    }

    /// Asks the nest, which matches `raw` against every bridge grammar serving
    /// the account. A transport failure is `NotFound`, never `Error`: a bridge
    /// that cannot be asked has not recognised the address as its own, and
    /// claiming it would stop the probe chain on a guess.
    async fn resolve_address(&self, raw: &str) -> ResolveResult {
        match self.sink.resolve(raw.to_string()).await {
            Ok(Some(resolved)) => ResolveResult::Resolved(TypedAddress::Bridged {
                bridge_id: resolved.bridge_id,
                address: resolved.address,
            }),
            Ok(None) => ResolveResult::NotFound,
            Err(e) => {
                tracing::debug!(error = %e, "bridged resolve unavailable; not claiming the address");
                ResolveResult::NotFound
            }
        }
    }

    fn bucket_inbound(
        &self,
        msg: RailInboundMessage,
        _mailbox: Option<MailFeed>,
    ) -> Result<InboundBucket, BackendError> {
        let is_own = self.is_self(&msg.sender);
        let mut bucket = crate::backend::bucket_inbound_common(msg, None, is_own);
        // A bridged room is its FAR participants: the account's own far
        // address is not one of them. Dropping it here is what makes a
        // deposit, the user's own Sent copy and a thread the user opened from
        // the picker (whose participants are the resolved peer alone) all
        // bucket into the one thread — whether or not the bridge had reported
        // the account's address when each row was read.
        bucket.participants.retain(|p| !self.is_self(p));
        Ok(bucket)
    }

    async fn send(
        &self,
        thread: &ThreadDetail,
        compose: &ComposeState,
        // A bridge's attachments ride its declared vector; the family's send
        // carries the body alone until a bridge declares one.
        _attachments: &[ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError> {
        let bridge_id = bridge_id_of(&thread.participants)
            .ok_or_else(|| {
                BackendError::Refusal(fauna_i18n::strings::error::send::NO_RECIPIENTS.to_string())
            })?
            .to_string();
        let identity = self
            .identity(&bridge_id)
            .ok_or(BackendError::NotSupported)?;
        let recipient_source: &[TypedAddress] = if compose.reply_recipients.is_empty() {
            &thread.participants
        } else {
            &compose.reply_recipients
        };
        let peers: Vec<String> = recipient_source
            .iter()
            .filter(|a| !self.is_self(a))
            .filter_map(|a| match a {
                TypedAddress::Bridged {
                    bridge_id: b,
                    address,
                } if *b == bridge_id => Some(address.clone()),
                _ => None,
            })
            .collect();
        if peers.is_empty() {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::error::send::NO_RECIPIENTS.to_string(),
            ));
        }
        // A user-opened room is 1:1 — `rooms.open` takes one far address, and
        // a multi-party open is not ruled. Refused here, for every app's glue
        // alike, rather than minting one room per peer.
        if peers.len() > 1 {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::conversations::unified::BRIDGED_ONE_RECIPIENT.to_string(),
            ));
        }
        let sent = self
            .sink
            .send(BridgedOutbound {
                bridge_id: bridge_id.clone(),
                peers,
                body: compose.body_draft.clone(),
                bridge_x25519: identity.bridge_x25519,
            })
            .await
            .map_err(BackendError::transport_from_seam)?;
        self.note_self_address(&bridge_id, &sent.self_address);
        Ok(SendOutcome {
            message_id: bridged_message_id(sent.row_id),
            timestamp_ms: fauna_core::data::Timestamp::now_millis() as i64,
            sender: TypedAddress::Bridged {
                bridge_id,
                address: sent.self_address,
            },
            // Not on the account data plane: a bridged row is not a
            // content-scope feed record.
            plane_ref: None,
            attachment_coordinates: Vec::new(),
        })
    }
}

// ── Inbound bridged receive driver ────────────────────────────────────

/// Drive the bridged inbox end-to-end: page `source` until exhausted, map each
/// opened row onto a [`RailInboundMessage`], and ingest it into `manager` on the
/// bridged rail. The receive counterpart to [`BridgedBackend::send`], in shared
/// Rust so every app runs the same bucket and ingest path (priority #2) —
/// the bridged twin of [`crate::backends::smtp::poll_inbound_mail`].
///
/// `after_id` is the paging cursor, advanced to the highest row id seen; `seen`
/// dedups by row id across calls (`ingest_inbound` is not idempotent). An
/// outbound row teaches `backend` the account's own far address before it is
/// bucketed, so the user's Sent copy lands as their own message. Returns the
/// number of newly ingested messages.
///
/// The rooms are read first and loaded into `backend`
/// ([`BridgedBackend::set_rooms`]), so a row ingested by this pass already
/// paints its bridge's declared identity and its guardian marker — and a pass
/// that ingests nothing still refreshes both (a consent that landed, a
/// guardian's decision).
///
/// The user's local removes reach every device here (`bridged-adapter.md`
/// § Removing bridged rows, rule 5): a room gone from the list drops its
/// thread, the tombstones a room's re-read found (its record's
/// `removed_rows`) and every row the inbox serves as a tombstone are
/// forgotten, never ingested.
pub async fn poll_inbound_bridged(
    source: &dyn BridgedSource,
    backend: &BridgedBackend,
    manager: &ConversationsManager,
    after_id: &mut i64,
    seen: &mut HashSet<i64>,
    page_limit: u32,
) -> Result<usize, BackendError> {
    let rooms = source
        .rooms()
        .await
        .map_err(BackendError::transport_from_seam)?;
    let removed: Vec<i64> = rooms
        .iter()
        .flat_map(|room| room.removed_rows.iter().copied())
        .collect();
    let vanished = backend.set_rooms(rooms);
    if !vanished.is_empty() {
        for (thread, participants) in manager.threads_on_rail(Rail::Bridged) {
            if backend
                .room_ref(&participants)
                .is_some_and(|room| vanished.contains(&room))
            {
                manager.forget_thread(&thread);
            }
        }
    }
    for row in removed {
        manager.forget_message(&bridged_message_id(row));
    }
    let mut ingested = 0usize;
    loop {
        let rows = source
            .fetch(*after_id, page_limit)
            .await
            .map_err(BackendError::transport_from_seam)?;
        if rows.is_empty() {
            break;
        }
        let count = rows.len();
        for rec in rows {
            if rec.id > *after_id {
                *after_id = rec.id;
            }
            // A tombstone is never ingested; a row held under its id leaves.
            if rec.removed {
                seen.insert(rec.id);
                manager.forget_message(&bridged_message_id(rec.id));
                continue;
            }
            // Before the dedup: a stamp is news about a row already held.
            if rec.undelivered {
                backend.note_undelivered(rec.id);
            }
            if !seen.insert(rec.id) {
                continue;
            }
            if rec.outbound {
                backend.note_self_address(&rec.bridge_id, &rec.sender);
            }
            manager.ingest_inbound(bridged_record_to_message(&rec))?;
            ingested += 1;
        }
        if page_limit != 0 && (count as u32) < page_limit {
            break;
        }
    }
    Ok(ingested)
}

/// Map one opened bridged row onto a [`RailInboundMessage`] — the far spellings
/// become [`TypedAddress::Bridged`] on the row's bridge.
pub fn bridged_record_to_message(rec: &BridgedRecord) -> RailInboundMessage {
    let on_bridge = |address: &String| TypedAddress::unresolved_bridged(&rec.bridge_id, address);
    RailInboundMessage {
        rail: Rail::Bridged,
        sender: on_bridge(&rec.sender),
        recipients: rec.recipients.iter().map(on_bridge).collect(),
        subject: None,
        body: rec.body.clone(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: rec.timestamp_ms,
        message_id: bridged_message_id(rec.id),
        in_reply_to: None,
        attachments: Vec::new(),
        badges: MessageBadges::default(),
        plane_ref: None,
        legal_takedown_ref: None,
        admin_block_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BridgedResolved, BridgedSent};
    use crate::capabilities::{DeliveryMode, ThreadEncryption};
    use crate::room::RoomClass;
    use crate::thread::{ThreadFlavor, ThreadId};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSink {
        sent: Mutex<Vec<BridgedOutbound>>,
        /// Every `messages.remove` asked of the nest.
        removed: Mutex<Vec<(BridgedRoomRef, Vec<i64>)>>,
        /// Every `rooms.remove` asked of the nest.
        rooms_removed: Mutex<Vec<BridgedRoomRef>>,
        /// The nest's refusal, when set: every remove answers it.
        refusal: Mutex<Option<String>>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl BridgedSink for FakeSink {
        async fn resolve(&self, raw: String) -> Result<Option<BridgedResolved>, String> {
            Ok(raw.starts_with('@').then(|| BridgedResolved {
                bridge_id: "matrix".into(),
                address: raw,
            }))
        }

        async fn send(&self, outbound: BridgedOutbound) -> Result<BridgedSent, String> {
            self.sent.lock().unwrap().push(outbound);
            Ok(BridgedSent {
                row_id: 41,
                self_address: "@me:example.org".into(),
            })
        }

        async fn remove_messages(
            &self,
            room: BridgedRoomRef,
            ids: Vec<i64>,
        ) -> Result<u32, String> {
            if let Some(refusal) = self.refusal.lock().unwrap().clone() {
                return Err(refusal);
            }
            let n = ids.len() as u32;
            self.removed.lock().unwrap().push((room, ids));
            Ok(n)
        }

        async fn remove_room(&self, room: BridgedRoomRef) -> Result<u32, String> {
            if let Some(refusal) = self.refusal.lock().unwrap().clone() {
                return Err(refusal);
            }
            self.rooms_removed.lock().unwrap().push(room);
            Ok(1)
        }
    }

    /// The vector a bridge declares: chosen to differ from the rail's
    /// most-restrictive answer in every field it may declare, so a verbatim
    /// overlay is distinguishable from a partial one.
    fn declared() -> ThreadCapabilities {
        ThreadCapabilities {
            supports_attachments: true,
            supports_markdown: true,
            supports_reactions: true,
            supports_message_delete: true,
            supports_per_message_reply: true,
            supports_membership_change: true,
            supports_recipient_selection: true,
            supports_rename: true,
            supports_subject: true,
            delivery_mode: DeliveryMode::Realtime,
            // A bridge cannot declare this (the manifest refuses it); a value
            // here must never reach the thread.
            encryption: ThreadEncryption::E2E,
            can_invite: true,
            can_remove_members: true,
            can_set_policy: true,
            can_appoint_admins: true,
            can_transfer_ownership: true,
            can_leave_room: true,
            can_search_room: false,
            // Never declared: the registry and the room decide it.
            can_send: false,
            // Never declared: the rail decides it.
            supports_local_remove: false,
        }
    }

    fn matrix() -> BridgeIdentity {
        BridgeIdentity {
            id: "matrix".into(),
            label: "Matrix".into(),
            glyph: SourceGlyph::Globe,
            capabilities: declared(),
            bridge_x25519: [7u8; 32],
        }
    }

    fn thread(participants: Vec<TypedAddress>) -> ThreadDetail {
        ThreadDetail {
            thread_id: ThreadId("t-1".into()),
            rail: Rail::Bridged,
            glyph: Rail::Bridged.glyph(),
            flavor: ThreadFlavor::OneToOne,
            label: String::new(),
            participant_displays: participants.iter().map(|p| p.display()).collect(),
            participants,
            capabilities: derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne),
            messages: Vec::new(),
            compose: ComposeState::default(),
            selected_message_id: None,
            room: None,
            bridge: None,
            guardian_state: None,
        }
    }

    fn alice() -> TypedAddress {
        TypedAddress::unresolved_bridged("matrix", "@alice:example.org")
    }

    #[test]
    fn the_declared_vector_is_overlaid_verbatim_and_encryption_is_never_declared() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        backend.set_identities(vec![matrix()]);
        let caps = backend.capabilities(&thread(vec![alice()]));
        assert_eq!(
            caps,
            ThreadCapabilities {
                encryption: ThreadEncryption::TransportOnly,
                can_send: true,
                // The rail's, never the bridge's (`bridged-adapter.md`
                // § Removing bridged rows, rule 1) — and the declared
                // far-side delete is masked until the family carries one.
                supports_local_remove: true,
                supports_message_delete: false,
                ..declared()
            }
        );
    }

    #[test]
    fn an_unregistered_bridge_keeps_the_most_restrictive_vector() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        let t = thread(vec![TypedAddress::unresolved_bridged("unknown", "x")]);
        assert_eq!(
            backend.capabilities(&t),
            ThreadCapabilities {
                can_send: false,
                ..derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne)
            }
        );
        assert_eq!(backend.bridge_identity("unknown"), None);
    }

    /// The class is derived, never declared: the bridge principal on the floor
    /// makes every bridged room transport-only, whatever it declared — and the
    /// members stay participant-parallel for the chips.
    #[test]
    fn a_bridged_room_derives_transport_only() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        backend.set_identities(vec![matrix()]);
        let t = thread(vec![
            alice(),
            TypedAddress::unresolved_bridged("matrix", "@bob:example.org"),
        ]);
        let room = backend.room_state(&t).expect("a bridged thread is a room");
        assert_eq!(room.class, RoomClass::TransportOnly);
        assert_eq!(room.members.len(), 2);
        assert_eq!(
            room.gate(backend.capabilities(&t)).encryption,
            ThreadEncryption::TransportOnly
        );
    }

    #[test]
    fn the_registry_answers_the_identity_a_thread_paints() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        backend.set_identities(vec![matrix()]);
        assert_eq!(
            backend.bridge_identity("matrix"),
            Some(BridgeIdentitySnapshot {
                id: "matrix".into(),
                label: "Matrix".into(),
                glyph: SourceGlyph::Globe,
            })
        );
        // A refreshed registry drops a bridge no longer listed.
        backend.set_identities(Vec::new());
        assert_eq!(backend.bridge_identity("matrix"), None);
    }

    #[tokio::test]
    async fn resolve_asks_the_nest_and_never_claims_on_its_own() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        assert_eq!(
            backend.resolve_address("@alice:example.org").await,
            ResolveResult::Resolved(alice())
        );
        assert_eq!(
            backend.resolve_address("alice@example.org").await,
            ResolveResult::NotFound
        );
    }

    #[tokio::test]
    async fn send_seals_to_the_bridge_key_and_its_echo_lands_as_own() {
        let sink = Arc::new(FakeSink::default());
        let backend = BridgedBackend::new(sink.clone());
        backend.set_identities(vec![matrix()]);
        let t = thread(vec![alice()]);
        let compose = ComposeState {
            body_draft: "hi".into(),
            ..Default::default()
        };
        let outcome = backend.send(&t, &compose, &[]).await.unwrap();
        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].bridge_x25519, [7u8; 32]);
        assert_eq!(sent[0].peers, vec!["@alice:example.org".to_string()]);
        assert_eq!(outcome.message_id, bridged_message_id(41));

        // The inbox's echo of that row is the user's own message.
        let echo = bridged_record_to_message(&BridgedRecord {
            id: 41,
            bridge_id: "matrix".into(),
            sender: "@me:example.org".into(),
            recipients: vec!["@alice:example.org".into()],
            body: "hi".into(),
            outbound: true,
            timestamp_ms: 1,
            undelivered: false,
            removed: false,
        });
        assert_eq!(echo.message_id, outcome.message_id);
        assert!(backend.bucket_inbound(echo, None).unwrap().message.is_own);
        // A peer's deposit is not.
        let inbound = bridged_record_to_message(&BridgedRecord {
            id: 42,
            bridge_id: "matrix".into(),
            sender: "@alice:example.org".into(),
            recipients: vec!["@me:example.org".into()],
            body: "hello".into(),
            outbound: false,
            timestamp_ms: 2,
            undelivered: false,
            removed: false,
        });
        assert!(
            !backend
                .bucket_inbound(inbound, None)
                .unwrap()
                .message
                .is_own
        );
    }

    #[tokio::test]
    async fn send_to_an_unregistered_bridge_is_not_attempted() {
        let sink = Arc::new(FakeSink::default());
        let backend = BridgedBackend::new(sink.clone());
        let t = thread(vec![alice()]);
        let compose = ComposeState {
            body_draft: "hi".into(),
            ..Default::default()
        };
        assert!(matches!(
            backend.send(&t, &compose, &[]).await,
            Err(BackendError::NotSupported)
        ));
        assert!(sink.sent.lock().unwrap().is_empty());
    }

    /// A scripted `rooms.list` + inbox.
    struct FakeSource {
        rooms: Mutex<Vec<BridgedRoomRecord>>,
        rows: Vec<BridgedRecord>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl BridgedSource for FakeSource {
        async fn rooms(&self) -> Result<Vec<BridgedRoomRecord>, String> {
            Ok(self.rooms.lock().unwrap().clone())
        }

        async fn fetch(&self, after_id: i64, _limit: u32) -> Result<Vec<BridgedRecord>, String> {
            Ok(self
                .rows
                .iter()
                .filter(|r| r.id > after_id)
                .cloned()
                .collect())
        }
    }

    fn room(peer: &str, guardian_state: Option<GuardianState>) -> BridgedRoomRecord {
        BridgedRoomRecord {
            bridge_id: "matrix".into(),
            identity: Some(matrix()),
            participants: vec![peer.into()],
            self_address: Some("@me:example.org".into()),
            guardian_state,
            disconnected: false,
            undelivered_rows: Vec::new(),
            last_removed_at: 0,
            removed_rows: Vec::new(),
        }
    }

    fn deposit(id: i64, peer: &str) -> BridgedRecord {
        BridgedRecord {
            id,
            bridge_id: "matrix".into(),
            sender: peer.into(),
            recipients: vec!["@me:example.org".into()],
            body: "hello".into(),
            outbound: false,
            timestamp_ms: id,
            undelivered: false,
            removed: false,
        }
    }

    /// The whole receive leg over the seams: one poll loads the rooms, ingests
    /// the rows, and the manager's list and detail then carry the bridge's
    /// declared identity, the transport-only class and the nest's guardian
    /// marker — on the held room only. A second poll whose rooms no longer
    /// carry the marker (the guardian decided, or the knob relaxed) takes it
    /// off again: nothing is stored client-side.
    #[tokio::test]
    async fn a_poll_paints_the_declared_identity_and_the_guardian_marker() {
        let backend = Arc::new(BridgedBackend::new(Arc::new(FakeSink::default())));
        let manager = ConversationsManager::new();
        manager.register_backend(backend.clone());
        let source = FakeSource {
            rooms: Mutex::new(vec![
                room("@alice:example.org", None),
                room("@cold:example.org", Some(GuardianState::Held)),
            ]),
            rows: vec![
                deposit(1, "@alice:example.org"),
                deposit(2, "@cold:example.org"),
            ],
        };
        let (mut after, mut seen) = (0i64, HashSet::new());
        let n = poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        assert_eq!((n, after), (2, 2));

        let threads = manager.snapshot().threads;
        assert_eq!(threads.len(), 2);
        let state_of = |threads: &[crate::snapshot::ThreadSummary], peer: &str| {
            let t = threads
                .iter()
                .find(|t| t.label.contains(peer))
                .unwrap_or_else(|| panic!("no thread for {peer}: {threads:?}"));
            assert_eq!(t.rail, Rail::Bridged);
            assert_eq!(t.glyph, SourceGlyph::Globe, "the declared glyph");
            assert_eq!(t.bridge.as_ref().map(|b| b.label.as_str()), Some("Matrix"));
            (t.thread_id.clone(), t.guardian_state)
        };
        assert_eq!(state_of(&threads, "@alice:example.org").1, None);
        let (cold, held) = state_of(&threads, "@cold:example.org");
        assert_eq!(held, Some(GuardianState::Held));

        // The detail carries the same marker, the class is transport-only, and
        // the held thread is fully readable.
        let detail = manager.thread_detail(cold).expect("the held thread opens");
        assert_eq!(detail.guardian_state, Some(GuardianState::Held));
        assert_eq!(
            detail.room.as_ref().map(|r| r.class),
            Some(RoomClass::TransportOnly)
        );
        assert_eq!(detail.messages.len(), 1);

        // The nest stops computing the marker → the next poll takes it off.
        *source.rooms.lock().unwrap() = vec![
            room("@alice:example.org", None),
            room("@cold:example.org", None),
        ];
        let n = poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        assert_eq!(n, 0, "the cursor dedups");
        let threads = manager.snapshot().threads;
        assert_eq!(state_of(&threads, "@cold:example.org").1, None);
    }

    /// One room is one thread, whatever each row knew of the account's own far
    /// address: a deposit read before the bridge reported it, the user's Sent
    /// copy stamped with no sender, a Sent copy stamped with it, and a deposit
    /// addressed to it all land together — and the Sent copies are the user's
    /// own.
    #[tokio::test]
    async fn every_row_of_a_room_lands_in_one_thread() {
        let backend = Arc::new(BridgedBackend::new(Arc::new(FakeSink::default())));
        let manager = ConversationsManager::new();
        manager.register_backend(backend.clone());
        let row = |id, sender: &str, recipients: &[&str], outbound| BridgedRecord {
            id,
            bridge_id: "matrix".into(),
            sender: sender.into(),
            recipients: recipients.iter().map(|r| r.to_string()).collect(),
            body: format!("row {id}"),
            outbound,
            timestamp_ms: id,
            undelivered: false,
            removed: false,
        };
        let peer = "@alice:example.org";
        let me = "@me:example.org";
        let source = FakeSource {
            rooms: Mutex::new(vec![BridgedRoomRecord {
                self_address: None,
                ..room(peer, None)
            }]),
            rows: vec![
                row(1, peer, &[], false),
                row(2, "", &[peer], true),
                row(3, me, &[peer], true),
                row(4, peer, &[me], false),
            ],
        };
        let (mut after, mut seen) = (0i64, HashSet::new());
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let threads = manager.snapshot().threads;
        assert_eq!(threads.len(), 1, "{threads:?}");
        let detail = manager.thread_detail(threads[0].thread_id.clone()).unwrap();
        assert_eq!(detail.participants, vec![alice()]);
        let own: Vec<bool> = detail.messages.iter().map(|m| m.is_own).collect();
        assert_eq!(own, vec![false, true, true, false]);
    }

    /// A room no live principal serves registers no identity, so its thread
    /// falls to the rail's most-restrictive vector, its send withheld, and the
    /// generic glyph (ruling 2 (b)) — while the bridge's other, served rooms
    /// keep theirs.
    #[test]
    fn a_disconnected_room_registers_no_identity() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        backend.set_rooms(vec![BridgedRoomRecord {
            bridge_id: "matrix".into(),
            identity: None,
            participants: vec!["@alice:example.org".into()],
            self_address: None,
            guardian_state: Some(GuardianState::Blocked),
            disconnected: true,
            undelivered_rows: Vec::new(),
            last_removed_at: 0,
            removed_rows: Vec::new(),
        }]);
        assert_eq!(backend.bridge_identity("matrix"), None);
        assert_eq!(
            backend.capabilities(&thread(vec![alice()])),
            ThreadCapabilities {
                can_send: false,
                ..derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne)
            }
        );
        // The marker is the nest's answer about the peer, not the bridge's.
        assert_eq!(
            backend.guardian_state(&[alice()]),
            Some(GuardianState::Blocked)
        );
    }

    /// The bridge stops serving (`apps/bridges.md` § Phase G → *When the
    /// bridge stops serving*): the room says so on its notice, and the user's
    /// own message the bridge never took says so too — whether the stamp
    /// reaches the app on the row itself (a row first read after the stamp)
    /// or on the glue's re-read of the room (a row this device read before
    /// it). A peer's deposit is never marked, and the room reconnects with
    /// the next read once a bridge adopts it, the stamps kept.
    #[tokio::test]
    async fn a_disconnected_room_says_so_and_its_undelivered_sent_copies_too() {
        let backend = Arc::new(BridgedBackend::new(Arc::new(FakeSink::default())));
        let manager = ConversationsManager::new();
        manager.register_backend(backend.clone());
        let peer = "@alice:example.org";
        let me = "@me:example.org";
        let sent = |id, undelivered| BridgedRecord {
            id,
            bridge_id: "matrix".into(),
            sender: me.into(),
            recipients: vec![peer.into()],
            body: format!("sent {id}"),
            outbound: true,
            timestamp_ms: id,
            undelivered,
            removed: false,
        };
        let source = FakeSource {
            rooms: Mutex::new(vec![room(peer, None)]),
            rows: vec![deposit(1, peer), sent(2, false)],
        };
        let (mut after, mut seen) = (0i64, HashSet::new());
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let thread_id = manager.snapshot().threads[0].thread_id.clone();
        let detail = manager.thread_detail(thread_id.clone()).unwrap();
        assert!(!detail.room.as_ref().unwrap().bridge_disconnected);
        assert_eq!(detail.room.as_ref().unwrap().notice(), None);
        assert!(detail.capabilities.can_send, "a served room is writable");
        assert!(detail.messages.iter().all(|m| !m.undelivered));

        // Revoked: the room is disconnected, and the glue's re-read found
        // row 2 stamped — a row the cursor has already passed.
        *source.rooms.lock().unwrap() = vec![BridgedRoomRecord {
            identity: None,
            disconnected: true,
            undelivered_rows: vec![2],
            last_removed_at: 0,
            removed_rows: Vec::new(),
            ..room(peer, None)
        }];
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let detail = manager.thread_detail(thread_id.clone()).unwrap();
        let room_now = detail.room.as_ref().unwrap();
        assert!(room_now.bridge_disconnected);
        assert_eq!(
            room_now.notice(),
            Some(crate::room::RoomNotice::BridgeDisconnected)
        );
        // Readable, never writable: the bar the notice explains is withheld.
        assert!(!detail.capabilities.can_send);
        let marks: Vec<(bool, bool)> = detail
            .messages
            .iter()
            .map(|m| (m.is_own, m.undelivered))
            .collect();
        assert_eq!(marks, vec![(false, false), (true, true)]);
        // The body stays: the user needs it to send again.
        assert!(!detail.messages[1].body.is_empty());

        // A row first read after its stamp carries it itself.
        let late = FakeSource {
            rooms: Mutex::new(source.rooms.lock().unwrap().clone()),
            rows: vec![sent(3, true)],
        };
        poll_inbound_bridged(&late, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let detail = manager.thread_detail(thread_id.clone()).unwrap();
        assert_eq!(detail.messages.iter().filter(|m| m.undelivered).count(), 2);

        // Adopted again: the notice goes, the stamps stay.
        *late.rooms.lock().unwrap() = vec![room(peer, None)];
        poll_inbound_bridged(&late, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let detail = manager.thread_detail(thread_id).unwrap();
        assert_eq!(detail.room.as_ref().unwrap().notice(), None);
        assert_eq!(detail.messages.iter().filter(|m| m.undelivered).count(), 2);
    }

    // ── The local remove (`bridged-adapter.md` § Removing bridged rows) ──

    /// One poll of one room with two rows — a peer's deposit and the user's
    /// own Sent copy — over a sink that records the removes.
    async fn one_room_polled() -> (
        Arc<FakeSink>,
        Arc<BridgedBackend>,
        Arc<ConversationsManager>,
        FakeSource,
        crate::thread::ThreadId,
    ) {
        let sink = Arc::new(FakeSink::default());
        let backend = Arc::new(BridgedBackend::new(sink.clone()));
        let manager = ConversationsManager::new();
        manager.register_backend(backend.clone());
        let peer = "@alice:example.org";
        let source = FakeSource {
            rooms: Mutex::new(vec![room(peer, None)]),
            rows: vec![
                deposit(1, peer),
                BridgedRecord {
                    sender: "@me:example.org".into(),
                    recipients: vec![peer.into()],
                    outbound: true,
                    ..deposit(2, peer)
                },
            ],
        };
        let (mut after, mut seen) = (0i64, HashSet::new());
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let thread = manager.snapshot().threads[0].thread_id.clone();
        (sink, backend, manager, source, thread)
    }

    /// Rule 1: every message of a bridged thread may be removed — the
    /// user's own Sent copy too — and none offers the far-side delete.
    #[tokio::test]
    async fn every_row_of_a_bridged_thread_is_removable_and_none_deletable() {
        let (_, _, manager, _, thread) = one_room_polled().await;
        let detail = manager.thread_detail(thread).unwrap();
        assert!(detail.capabilities.supports_local_remove);
        assert!(!detail.capabilities.supports_message_delete);
        let marks: Vec<(bool, bool, bool)> = detail
            .messages
            .iter()
            .map(|m| (m.is_own, m.can_remove, m.can_delete))
            .collect();
        assert_eq!(marks, vec![(false, true, false), (true, true, false)]);
    }

    /// Rules 5 and 6: the remove asks the nest for exactly that row of that
    /// room, and the message leaves the manager once the nest answers.
    #[tokio::test]
    async fn remove_message_asks_the_nest_then_forgets_the_row() {
        let (sink, _, manager, _, thread) = one_room_polled().await;
        manager
            .remove_message(thread.clone(), bridged_message_id(1))
            .await;
        assert_eq!(
            sink.removed.lock().unwrap().clone(),
            vec![(
                BridgedRoomRef {
                    bridge_id: "matrix".into(),
                    participants: vec!["@alice:example.org".into()],
                },
                vec![1]
            )]
        );
        let detail = manager.thread_detail(thread).unwrap();
        let ids: Vec<MessageId> = detail
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        assert_eq!(ids, vec![bridged_message_id(2)]);
        assert_eq!(manager.page_error_diagnostic(), None);
    }

    /// Rule 5: never optimistic — a refusal leaves the row in place and
    /// says so on the page error.
    #[tokio::test]
    async fn a_refused_remove_keeps_the_row_and_says_why() {
        let (sink, _, manager, _, thread) = one_room_polled().await;
        *sink.refusal.lock().unwrap() = Some("the nest said no".into());
        manager
            .remove_message(thread.clone(), bridged_message_id(1))
            .await;
        assert_eq!(
            manager
                .thread_detail(thread.clone())
                .unwrap()
                .messages
                .len(),
            2
        );
        assert_eq!(
            manager.page_error_diagnostic().as_deref(),
            Some("conversations.unified.error_remove_message: the nest said no")
        );
        manager.remove_thread(thread.clone()).await;
        assert!(manager.thread_detail(thread).is_some(), "the thread stays");
        assert!(
            manager
                .page_error_diagnostic()
                .is_some_and(|e| e.starts_with("conversations.unified.error_remove_thread"))
        );
    }

    /// The manager refuses before the wire a message whose snapshot does not
    /// say `can_remove` — on a rail with no local remove, nothing reaches a
    /// backend and the page error says why.
    #[tokio::test]
    async fn remove_is_refused_where_the_flags_do_not_offer_it() {
        let manager = ConversationsManager::new();
        manager.register_backend(Arc::new(crate::backends::mock::MockRailBackend::new(
            Rail::Smtp,
        )));
        let mail = RailInboundMessage {
            rail: Rail::Smtp,
            sender: TypedAddress::unresolved_for_rail(Rail::Smtp, "bob@example.org").unwrap(),
            recipients: Vec::new(),
            message_id: MessageId("<m1@example.org>".into()),
            ..bridged_record_to_message(&deposit(1, "x"))
        };
        manager.ingest_inbound(mail).unwrap();
        let thread = manager.snapshot().threads[0].thread_id.clone();
        let detail = manager.thread_detail(thread.clone()).unwrap();
        assert!(!detail.capabilities.supports_local_remove);
        assert!(detail.messages.iter().all(|m| !m.can_remove));
        manager
            .remove_message(thread.clone(), MessageId("<m1@example.org>".into()))
            .await;
        assert!(
            manager
                .page_error_diagnostic()
                .is_some_and(|e| e.starts_with("conversations.unified.error_remove_message"))
        );
        manager.remove_thread(thread.clone()).await;
        assert!(
            manager
                .page_error_diagnostic()
                .is_some_and(|e| e.starts_with("conversations.unified.error_remove_thread"))
        );
        assert_eq!(manager.thread_detail(thread).unwrap().messages.len(), 1);
    }

    /// Rule 4: the whole room goes to the nest, and its thread leaves the
    /// list — and the selection with it.
    #[tokio::test]
    async fn remove_thread_asks_the_nest_then_drops_the_thread() {
        let (sink, _, manager, _, thread) = one_room_polled().await;
        manager.select_thread(thread.clone());
        manager.remove_thread(thread.clone()).await;
        assert_eq!(
            sink.rooms_removed.lock().unwrap().clone(),
            vec![BridgedRoomRef {
                bridge_id: "matrix".into(),
                participants: vec!["@alice:example.org".into()],
            }]
        );
        assert!(manager.thread_detail(thread).is_none());
        assert!(manager.snapshot().threads.is_empty());
        assert_eq!(manager.snapshot().selected_thread_id, None);
    }

    /// Rule 5, the other devices: a row the inbox serves as a tombstone is
    /// forgotten and never ingested; the rows a room's re-read names
    /// (`removed_rows`) are forgotten; a room gone from `rooms.list` drops
    /// its thread — and only a room once listed can vanish.
    #[tokio::test]
    async fn another_devices_removes_reach_this_one() {
        let (_, backend, manager, source, thread) = one_room_polled().await;
        let (mut after, mut seen) = (2i64, [1i64, 2].into_iter().collect::<HashSet<_>>());

        // The re-read found row 1 tombstoned: it leaves; row 2 stays.
        *source.rooms.lock().unwrap() = vec![BridgedRoomRecord {
            last_removed_at: 10,
            removed_rows: vec![1],
            ..room("@alice:example.org", None)
        }];
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        let ids: Vec<MessageId> = manager
            .thread_detail(thread.clone())
            .unwrap()
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        assert_eq!(ids, vec![bridged_message_id(2)]);

        // A tombstone served by the inbox is never ingested — a fresh read
        // skips it — and a held row under its id leaves.
        let tomb = FakeSource {
            rooms: Mutex::new(source.rooms.lock().unwrap().clone()),
            rows: vec![
                BridgedRecord {
                    removed: true,
                    body: String::new(),
                    ..deposit(2, "@alice:example.org")
                },
                BridgedRecord {
                    removed: true,
                    body: String::new(),
                    ..deposit(3, "@alice:example.org")
                },
            ],
        };
        let n = poll_inbound_bridged(&tomb, &backend, &manager, &mut 0, &mut HashSet::new(), 0)
            .await
            .unwrap();
        assert_eq!(n, 0, "a tombstone is never ingested");
        assert!(
            manager
                .thread_detail(thread.clone())
                .unwrap()
                .messages
                .is_empty()
        );

        // The room is gone from the list: its thread goes too.
        *tomb.rooms.lock().unwrap() = Vec::new();
        poll_inbound_bridged(&tomb, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        assert!(manager.thread_detail(thread).is_none());
    }

    /// Rule 2 reaches local search: a removed row stops resolving through
    /// [`ConversationsManager::locate_message`], the seam the local search
    /// arm resolves every Conversation hit through, so its hit is DROPPED at
    /// projection — deletion is display-healed, never index-healed
    /// (`content-index-ingest.md` § Ingest triggers, v1 → *Deletion is
    /// display-healed*; `content-index.md` § Where queries run). Covers all
    /// three reaches: this device's row remove, another device's tombstone,
    /// and a room removed whole.
    #[tokio::test]
    async fn a_removed_row_no_longer_resolves_for_local_search() {
        let resolves = |m: &ConversationsManager, row: i64| {
            m.locate_message(&bridged_message_id(row).0).is_some()
        };

        // This device removes row 1: it stops resolving, row 2 still does.
        let (_, _, manager, _, thread) = one_room_polled().await;
        assert!(resolves(&manager, 1) && resolves(&manager, 2));
        manager
            .remove_message(thread.clone(), bridged_message_id(1))
            .await;
        assert!(!resolves(&manager, 1), "a removed row must not resolve");
        assert!(resolves(&manager, 2));

        // A whole room removed: none of its rows resolves.
        manager.remove_thread(thread).await;
        assert!(
            !resolves(&manager, 2),
            "a removed room's rows must not resolve"
        );

        // Another device's tombstone (`removed_rows` on the re-read).
        let (_, backend, manager, source, _) = one_room_polled().await;
        let (mut after, mut seen) = (2i64, [1i64, 2].into_iter().collect::<HashSet<_>>());
        *source.rooms.lock().unwrap() = vec![BridgedRoomRecord {
            last_removed_at: 10,
            removed_rows: vec![1],
            ..room("@alice:example.org", None)
        }];
        poll_inbound_bridged(&source, &backend, &manager, &mut after, &mut seen, 0)
            .await
            .unwrap();
        assert!(
            !resolves(&manager, 1),
            "another device's remove must reach search"
        );
        assert!(resolves(&manager, 2));
    }

    /// A thread whose room this device never saw listed is not a vanished
    /// room: an empty first read drops nothing.
    #[test]
    fn only_a_room_once_listed_can_vanish() {
        let backend = BridgedBackend::new(Arc::new(FakeSink::default()));
        assert!(backend.set_rooms(Vec::new()).is_empty());
        assert!(
            backend
                .set_rooms(vec![room("@alice:example.org", None)])
                .is_empty()
        );
        assert_eq!(
            backend.set_rooms(Vec::new()),
            vec![BridgedRoomRef {
                bridge_id: "matrix".into(),
                participants: vec!["@alice:example.org".into()],
            }]
        );
    }

    #[test]
    fn a_bridged_message_id_round_trips_to_its_row() {
        assert_eq!(bridged_row_id(&bridged_message_id(41)), Some(41));
        assert_eq!(bridged_row_id(&MessageId("<41@example.org>".into())), None);
        assert_eq!(bridged_row_id(&MessageId("41".into())), None);
    }
}
