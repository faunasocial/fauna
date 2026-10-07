//! The nest-backed glue of the **bridged rail** — the one object every app
//! registers (a native app on its session, `ConversationsSession::
//! register_bridged`; the browser on its manager), implementing
//! `fauna_conversations`' [`BridgedSink`] / [`BridgedSource`] seams over
//! [`BridgedConversationClient`] (`docs/goal/ui/conversations.md` § Where
//! logic lives → *The `Bridged` adapter*, ruling 2 — the user-side contract).
//!
//! What lives here and nowhere else on the app side is the crypto the nest is
//! blind to: an outbound message is sealed twice — to the bridge principal's
//! X25519 key (the item the bridge drains) and to the account's own recipient
//! key (the Sent row) — and every inbox row is opened under the account's
//! standing mail keys, the same recipient key mail records are sealed to. The
//! keys come through a [`BridgedKeySource`] — natively the session's shared
//! [`MailKeyCache`], so the MSEK never leaves shared Rust; in the browser the
//! key set its receive loop already holds. Until the account has one (mail
//! custody not yet provisioned) the rooms still list and paint, and the inbox
//! read is a quiet no-op that retries at the next wake.
//!
//! The logic is written once, in inherent `*_inner` methods generic over the
//! transport; the two seam impls per target are shims over them, because the
//! native `#[async_trait]` arm cannot prove a boxed future `Send` for an
//! arbitrary `R` (the [`crate::NestOutboundMailSink`] shape).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

#[cfg(not(target_arch = "wasm32"))]
use fauna_client::NestClient;
use fauna_client_bridges::conversations::BridgedConversationClient;
use fauna_conversations::backend::{
    BridgedOutbound, BridgedRecord, BridgedResolved, BridgedSent, BridgedSink, BridgedSource,
};
use fauna_conversations::backends::bridged::{BridgeIdentity, BridgedRoomRecord};
use fauna_conversations::capabilities::declared_bridge_capabilities;
use fauna_conversations::snapshot::GuardianState;
use fauna_core::source_glyph::SourceGlyph;
#[cfg(not(target_arch = "wasm32"))]
use fauna_mls::wrapped_blob::derive_recipient_xwing_keypair;
use fauna_mls::wrapped_blob::{
    StandingMailKeypair, XWingPublicKey, seal_to_recipient, seal_to_recipient_xwing,
};
use fauna_protocol::bridged_conversations::{BridgedMessageInfo, BridgedRoomInfo};
use fauna_protocol::kind_manifest::BridgeCapabilityValue;
use fauna_protocol::{RpcErrorClass, RpcRequester};

#[cfg(not(target_arch = "wasm32"))]
use crate::{MailKeyCache, MailKeys};

/// The account's recipient keys as the glue uses them: seal the Sent copy,
/// open an inbox row.
pub trait BridgedKeys {
    /// Seal `body` to the account's own recipient key — the Sent row, the
    /// same hybrid seal the nest applies to a mail `Sent` copy.
    fn seal_for_self(&self, body: &str) -> Result<Vec<u8>, String>;

    /// Open one bridged row — a bare inner record sealed to the account's
    /// recipient key — to its text.
    fn open_bridged(&self, sealed: &[u8], received_at_ms: i64) -> Option<String>;
}

/// Where the glue gets the account's [`BridgedKeys`]. `None` from either
/// method is "no recipient key yet" — mail custody not provisioned — never an
/// error. Static-dispatch only, like [`RpcRequester`], so each target's
/// future keeps its own `Send`-ness.
pub trait BridgedKeySource {
    type Keys: BridgedKeys;

    /// The keys as currently held.
    fn get(&self) -> impl Future<Output = Option<Self::Keys>>;

    /// Re-derive after a row missed under the held set (the account's keys
    /// can rotate under a running session), then answer as [`Self::get`].
    fn refresh(&self) -> impl Future<Output = Option<Self::Keys>>;
}

/// [`BridgedKeys::seal_for_self`] over the account's X-Wing recipient public
/// key — the one seal both targets' key sets apply.
pub fn seal_bridged_for_self(own_public: &XWingPublicKey, body: &str) -> Result<Vec<u8>, String> {
    seal_to_recipient_xwing(body.as_bytes(), own_public)
        .and_then(|env| env.to_canonical_bytes())
        .map_err(|e| e.to_string())
}

/// [`BridgedKeys::open_bridged`] over a standing key set and its mail-epoch
/// roots — the one open both targets' key sets apply. The seal instant is the
/// nest's `received_at`, never the far side's claim.
pub fn open_bridged_row(
    standing: &[StandingMailKeypair],
    epoch_roots: &[[u8; 32]],
    sealed: &[u8],
    received_at_ms: i64,
) -> Option<String> {
    let roots: Vec<&[u8; 32]> = epoch_roots.iter().collect();
    let secs = u64::try_from(received_at_ms / 1000).unwrap_or(0);
    let plain =
        fauna_mail::open_sealed_inner_record_with_keys(sealed, &roots, secs, standing).ok()?;
    String::from_utf8(plain).ok()
}

/// The nest's refusal for an address no consented bridge's grammar admits, or
/// more than one does (`conversation.rooms.open` with no `bridge_id`).
const ADDRESS_REFUSED: &str = "fauna.bridges.address_refused";

/// What the glue remembers of a room between reads: the id `send` and the
/// inbox name it by, and who is in it.
#[derive(Clone)]
struct KnownRoom {
    room_id: Vec<u8>,
    bridge_id: String,
    participants: Vec<String>,
    self_address: Option<String>,
}

impl KnownRoom {
    fn of(info: &BridgedRoomInfo) -> Self {
        Self {
            room_id: info.room_id.clone(),
            bridge_id: info.bridge_id.clone(),
            participants: info.participants.clone(),
            self_address: info.self_address.clone().filter(|a| !a.is_empty()),
        }
    }
}

/// See the module docs.
pub struct NestBridgedGlue<R: RpcRequester, K> {
    client: BridgedConversationClient<R>,
    keys: Arc<K>,
    /// The rooms of the newest `rooms.list` (and every `rooms.open` since),
    /// by room id.
    rooms: Mutex<HashMap<Vec<u8>, KnownRoom>>,
    /// The highest inbox row id this glue has read, opened or skipped.
    read_through: Mutex<i64>,
}

impl<R, K> NestBridgedGlue<R, K>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    K: BridgedKeySource,
{
    /// Over the session's connection and its mail keys — natively the shared
    /// [`MailKeyCache`].
    pub fn new(nest: R, keys: Arc<K>) -> Arc<Self> {
        Arc::new(Self {
            client: BridgedConversationClient::new(nest),
            keys,
            rooms: Mutex::new(HashMap::new()),
            read_through: Mutex::new(0),
        })
    }

    fn remember(&self, info: &BridgedRoomInfo) {
        self.rooms
            .lock()
            .unwrap()
            .insert(info.room_id.clone(), KnownRoom::of(info));
    }

    fn known(&self, room_id: &[u8]) -> Option<KnownRoom> {
        self.rooms.lock().unwrap().get(room_id).cloned()
    }

    fn room_for(&self, bridge_id: &str, peer: &str) -> Option<KnownRoom> {
        self.rooms
            .lock()
            .unwrap()
            .values()
            .find(|r| r.bridge_id == bridge_id && r.participants == [peer])
            .cloned()
    }

    async fn list_rooms(&self) -> Result<Vec<BridgedRoomInfo>, String> {
        let rooms = self.client.rooms_list().await.map_err(user_sentence)?;
        *self.rooms.lock().unwrap() = rooms
            .iter()
            .map(|info| (info.room_id.clone(), KnownRoom::of(info)))
            .collect();
        Ok(rooms)
    }

    /// `rooms.open` with no bridge: the nest picks the one consented bridge
    /// whose grammar admits `raw`. Its `address_refused` — none, or several —
    /// is "not a bridged address"; any other failure is the caller's to treat
    /// as unavailable.
    async fn resolve_inner(&self, raw: String) -> Result<Option<BridgedResolved>, String> {
        match self.client.rooms_open(None, raw.clone()).await {
            Ok(room) => {
                self.remember(&room);
                Ok(Some(BridgedResolved {
                    address: room.participants.first().cloned().unwrap_or(raw),
                    bridge_id: room.bridge_id,
                }))
            }
            Err(e)
                if e.as_rpc_error()
                    .is_some_and(|rpc| rpc.code == ADDRESS_REFUSED) =>
            {
                Ok(None)
            }
            Err(e) => Err(user_sentence(e)),
        }
    }

    async fn send_inner(&self, outbound: BridgedOutbound) -> Result<BridgedSent, String> {
        // The shared backend refuses a multi-party send before it reaches a
        // sink (a user-opened room is 1:1); this arm only keeps the glue from
        // ever minting one room per peer.
        let [peer] = outbound.peers.as_slice() else {
            return Err("a bridged send names exactly one far address".to_string());
        };
        let keys = self
            .keys
            .get()
            .await
            .ok_or_else(fauna_conversations::backends::bridged::no_recipient_key_refusal)?;
        let room = match self.room_for(&outbound.bridge_id, peer) {
            Some(room) => room,
            None => {
                let info = self
                    .client
                    .rooms_open(Some(outbound.bridge_id.clone()), peer.clone())
                    .await
                    .map_err(user_sentence)?;
                self.remember(&info);
                KnownRoom::of(&info)
            }
        };
        let sealed_for_bridge = seal_for_bridge(&outbound.body, &outbound.bridge_x25519)?;
        let sealed_for_self = keys.seal_for_self(&outbound.body)?;
        let reply = self
            .client
            .send(room.room_id, sealed_for_bridge, sealed_for_self)
            .await
            .map_err(user_sentence)?;
        Ok(BridgedSent {
            row_id: reply.id,
            self_address: reply.self_address,
        })
    }

    async fn rooms_inner(&self) -> Result<Vec<BridgedRoomRecord>, String> {
        Ok(self.list_rooms().await?.iter().map(room_record).collect())
    }

    async fn fetch_inner(&self, after_id: i64, limit: u32) -> Result<Vec<BridgedRecord>, String> {
        // No recipient key yet → nothing here can be opened; leave the cursor
        // where it is and read again at the next wake.
        let Some(mut keys) = self.keys.get().await else {
            return Ok(Vec::new());
        };
        let mut refreshed = false;
        let mut relisted = false;
        // The driver stops on an empty page; a page whose every row was
        // skipped is not the end of the inbox, so read on from past it.
        loop {
            let after_id = after_id.max(*self.read_through.lock().unwrap());
            let page = self
                .client
                .inbox_fetch(None, after_id, limit)
                .await
                .map_err(user_sentence)?;
            let mut out = Vec::with_capacity(page.messages.len());
            for msg in &page.messages {
                let body = match keys.open_bridged(&msg.sealed_content, msg.received_at) {
                    Some(body) => Some(body),
                    // A miss against a stale key set is not a fact about the
                    // row (the account's keys can rotate under a running
                    // session): re-derive once per read before giving the row
                    // up.
                    None if !refreshed => {
                        refreshed = true;
                        match self.keys.refresh().await {
                            Some(fresh) => {
                                keys = fresh;
                                keys.open_bridged(&msg.sealed_content, msg.received_at)
                            }
                            None => None,
                        }
                    }
                    None => None,
                };
                let Some(body) = body else {
                    // Skipped, and the cursor still passes it: a row that
                    // opens under no key the account holds never will.
                    tracing::warn!(row = msg.id, bridge = %msg.bridge_id, "bridged row does not open; skipped");
                    continue;
                };
                // A row of a room born since the last `rooms.list` (the
                // driver reads rooms first, so this is a deposit racing the
                // read).
                if self.known(&msg.room_id).is_none() && !relisted {
                    relisted = true;
                    if let Err(e) = self.list_rooms().await {
                        tracing::debug!(error = %e, "bridged rooms re-read failed");
                    }
                }
                out.push(inbox_record(msg, self.known(&msg.room_id).as_ref(), body));
            }
            // The driver's cursor advances only from rows it is handed, so
            // the glue remembers how far it has read past rows it skipped —
            // else a trailing unopenable row is fetched (and warned about) at
            // every wake.
            if let Some(last) = page.messages.last() {
                let mut through = self.read_through.lock().unwrap();
                *through = (*through).max(last.id);
            }
            if out.is_empty() && !page.messages.is_empty() {
                continue;
            }
            return Ok(out);
        }
    }
}

/// A seam error as the sentence the send slot renders: the nest's own
/// localized refusal when the request reached it, the transport's text
/// otherwise.
fn user_sentence<E: RpcErrorClass + std::fmt::Display>(e: E) -> String {
    match e.as_rpc_error() {
        Some(rpc) => rpc.localized().to_string(),
        None => e.to_string(),
    }
}

/// One `rooms.list` row as the shared backend takes it. A row no live
/// principal serves carries no identity (`apps/bridges.md` § Phase G → *When
/// the bridge stops serving*), and so does one whose key is not 32 bytes — a
/// bridge the app could not seal to must not offer a composer.
fn room_record(info: &BridgedRoomInfo) -> BridgedRoomRecord {
    let key: Option<[u8; 32]> = info.bridge_x25519.as_slice().try_into().ok();
    let identity = key
        .filter(|_| !info.disconnected)
        .map(|bridge_x25519| BridgeIdentity {
            id: info.bridge_id.clone(),
            label: info.bridge_label.clone(),
            glyph: SourceGlyph::from_id(&info.glyph),
            capabilities: declared_bridge_capabilities(
                |name| match info.capabilities.get(name) {
                    Some(BridgeCapabilityValue::Flag(on)) => Some(*on),
                    _ => None,
                },
                match info.capabilities.get("delivery_mode") {
                    Some(BridgeCapabilityValue::Mode(mode)) => Some(mode.as_str()),
                    _ => None,
                },
            ),
            bridge_x25519,
        });
    BridgedRoomRecord {
        bridge_id: info.bridge_id.clone(),
        identity,
        participants: info.participants.clone(),
        self_address: info.self_address.clone().filter(|a| !a.is_empty()),
        guardian_state: info.guardian_state.as_deref().map(GuardianState::from_wire),
    }
}

/// One opened inbox row as the shared driver takes it: an inbound deposit is
/// from its far sender; a Sent copy is from the account (its far address when
/// the bridge has reported one, empty until then — the backend reads an
/// address-less sender as the account) to the room's participants. The
/// account's own address is never a recipient: a bridged room is its far
/// participants. Timestamped on the nest's `received_at`, never the far side's
/// claim.
fn inbox_record(msg: &BridgedMessageInfo, room: Option<&KnownRoom>, body: String) -> BridgedRecord {
    let (sender, recipients) = if msg.outbound {
        let sender = if msg.sender.is_empty() {
            room.and_then(|r| r.self_address.clone())
                .unwrap_or_default()
        } else {
            msg.sender.clone()
        };
        (
            sender,
            room.map(|r| r.participants.clone()).unwrap_or_default(),
        )
    } else {
        // An inbound row of a multi-party room is to the room's other far
        // participants too, so it buckets with the room's Sent copies.
        let others = room
            .map(|r| {
                r.participants
                    .iter()
                    .filter(|p| **p != msg.sender)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        (msg.sender.clone(), others)
    };
    BridgedRecord {
        id: msg.id,
        bridge_id: msg.bridge_id.clone(),
        sender,
        recipients,
        body,
        outbound: msg.outbound,
        timestamp_ms: msg.received_at,
    }
}

/// Seal `body` to the bridge principal's X25519 key — the item only the
/// bridge opens.
fn seal_for_bridge(body: &str, bridge_x25519: &[u8; 32]) -> Result<Vec<u8>, String> {
    seal_to_recipient(body.as_bytes(), bridge_x25519)
        .and_then(|env| env.to_canonical_bytes())
        .map_err(|e| e.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
impl BridgedKeys for MailKeys {
    fn seal_for_self(&self, body: &str) -> Result<Vec<u8>, String> {
        seal_bridged_for_self(&derive_recipient_xwing_keypair(&self.msek).public, body)
    }

    fn open_bridged(&self, sealed: &[u8], received_at_ms: i64) -> Option<String> {
        open_bridged_row(&self.standing, &self.epoch_roots, sealed, received_at_ms)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BridgedKeySource for MailKeyCache {
    type Keys = MailKeys;

    async fn get(&self) -> Option<MailKeys> {
        MailKeyCache::get(self).await
    }

    async fn refresh(&self) -> Option<MailKeys> {
        MailKeyCache::refresh(self).await
    }
}

// The seam impls are shims over the `*_inner` methods, one pair per concrete
// transport (see the module docs).

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl BridgedSink for NestBridgedGlue<Arc<NestClient>, MailKeyCache> {
    async fn resolve(&self, raw: String) -> Result<Option<BridgedResolved>, String> {
        self.resolve_inner(raw).await
    }

    async fn send(&self, outbound: BridgedOutbound) -> Result<BridgedSent, String> {
        self.send_inner(outbound).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl BridgedSource for NestBridgedGlue<Arc<NestClient>, MailKeyCache> {
    async fn rooms(&self) -> Result<Vec<BridgedRoomRecord>, String> {
        self.rooms_inner().await
    }

    async fn fetch(&self, after_id: i64, limit: u32) -> Result<Vec<BridgedRecord>, String> {
        self.fetch_inner(after_id, limit).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl<K: BridgedKeySource> BridgedSink for NestBridgedGlue<fauna_rpc_wasm::WsRpcClient, K> {
    async fn resolve(&self, raw: String) -> Result<Option<BridgedResolved>, String> {
        self.resolve_inner(raw).await
    }

    async fn send(&self, outbound: BridgedOutbound) -> Result<BridgedSent, String> {
        self.send_inner(outbound).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl<K: BridgedKeySource> BridgedSource for NestBridgedGlue<fauna_rpc_wasm::WsRpcClient, K> {
    async fn rooms(&self) -> Result<Vec<BridgedRoomRecord>, String> {
        self.rooms_inner().await
    }

    async fn fetch(&self, after_id: i64, limit: u32) -> Result<Vec<BridgedRecord>, String> {
        self.fetch_inner(after_id, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::capabilities::{DeliveryMode, ThreadEncryption};
    use fauna_mls::wrapped_blob::{
        MailRecordEnvelope, derive_recipient_hpke_keypair, unseal_mail_record,
    };

    fn keys(msek: [u8; 32]) -> MailKeys {
        MailKeys::from_custody([0; 32], &msek, &[])
    }

    fn info() -> BridgedRoomInfo {
        BridgedRoomInfo {
            room_id: vec![9; 32],
            bridge_id: "matrix".into(),
            bridge_label: "Matrix".into(),
            glyph: "globe".into(),
            far_room_id: "!room:example.org".into(),
            participants: vec!["@bob:example.org".into()],
            self_address: Some("@me:example.org".into()),
            capabilities: [
                (
                    "supports_membership_change".to_string(),
                    BridgeCapabilityValue::Flag(true),
                ),
                (
                    "delivery_mode".to_string(),
                    BridgeCapabilityValue::Mode("Realtime".into()),
                ),
                // A member only a newer nest knows is ignored, not an error.
                (
                    "supports_teleport".to_string(),
                    BridgeCapabilityValue::Flag(true),
                ),
            ]
            .into_iter()
            .collect(),
            bridge_x25519: vec![7; 32],
            guardian_state: Some("held".into()),
            ..Default::default()
        }
    }

    /// The Sent copy the app seals opens under the account's own standing
    /// keys — the property that makes the user's own message readable on
    /// every device — and under no other account's.
    #[test]
    fn the_sent_copy_opens_under_the_accounts_own_keys_only() {
        let mine = keys([3; 32]);
        let sealed = mine.seal_for_self("hello bob").unwrap();
        assert_eq!(mine.open_bridged(&sealed, 0).as_deref(), Some("hello bob"));
        assert_eq!(keys([4; 32]).open_bridged(&sealed, 0), None);
    }

    /// The outbound item is sealed to the bridge's key and to nothing the
    /// account holds: the account's own keys do not open it.
    #[test]
    fn the_outbound_item_opens_under_the_bridge_key_only() {
        // Any X25519 keypair stands in for the bridge principal's.
        let (bridge_secret, bridge_public) = derive_recipient_hpke_keypair(&[5u8; 32]);
        let sealed = seal_for_bridge("hello bob", &bridge_public).unwrap();
        let envelope = MailRecordEnvelope::from_canonical_bytes(&sealed).unwrap();
        assert_eq!(
            unseal_mail_record(&envelope, &bridge_secret).unwrap(),
            b"hello bob"
        );
        assert_eq!(keys([3; 32]).open_bridged(&sealed, 0), None);
    }

    /// A served room hands the backend its declared identity; the declared
    /// map lands on the most-restrictive vector member by member, and
    /// `encryption` is never the bridge's to say.
    #[test]
    fn a_served_room_carries_its_declared_identity_and_guardian_marker() {
        let record = room_record(&info());
        let identity = record.identity.expect("a served room has an identity");
        assert_eq!(
            (
                identity.id.as_str(),
                identity.label.as_str(),
                identity.glyph
            ),
            ("matrix", "Matrix", SourceGlyph::Globe)
        );
        assert_eq!(identity.bridge_x25519, [7; 32]);
        assert!(identity.capabilities.supports_membership_change);
        assert!(!identity.capabilities.supports_attachments);
        assert_eq!(identity.capabilities.delivery_mode, DeliveryMode::Realtime);
        assert_eq!(
            identity.capabilities.encryption,
            ThreadEncryption::TransportOnly
        );
        assert_eq!(record.guardian_state, Some(GuardianState::Held));
        assert_eq!(record.self_address.as_deref(), Some("@me:example.org"));
    }

    /// A room no live principal serves — and one whose key cannot be sealed
    /// to — registers no identity, so its thread offers no composer.
    #[test]
    fn a_disconnected_or_keyless_room_carries_no_identity() {
        let disconnected = BridgedRoomInfo {
            disconnected: true,
            ..info()
        };
        assert_eq!(room_record(&disconnected).identity, None);
        let keyless = BridgedRoomInfo {
            bridge_x25519: vec![1, 2, 3],
            ..info()
        };
        assert_eq!(room_record(&keyless).identity, None);
    }

    /// Both directions of a room bucket into ONE thread: an inbound deposit
    /// is from the peer, the Sent copy from the account to the peer, on the
    /// nest's clock — and the account's own address is never a recipient.
    #[test]
    fn both_directions_name_the_same_two_parties() {
        let room = KnownRoom::of(&info());
        let inbound = BridgedMessageInfo {
            id: 1,
            room_id: vec![9; 32],
            bridge_id: "matrix".into(),
            outbound: false,
            sender: "@bob:example.org".into(),
            created_at: 1,
            received_at: 5_000,
            ..Default::default()
        };
        let rec = inbox_record(&inbound, Some(&room), "hi".into());
        assert_eq!(
            (rec.sender.as_str(), rec.recipients.as_slice()),
            ("@bob:example.org", &[][..])
        );
        assert_eq!(
            rec.timestamp_ms, 5_000,
            "the nest's clock, not the far side's"
        );

        let sent = BridgedMessageInfo {
            id: 2,
            outbound: true,
            sender: String::new(),
            ..inbound
        };
        let rec = inbox_record(&sent, Some(&room), "hello".into());
        assert_eq!(
            (rec.sender.as_str(), rec.recipients.as_slice()),
            ("@me:example.org", &["@bob:example.org".to_string()][..])
        );
        assert!(rec.outbound);
    }
}
