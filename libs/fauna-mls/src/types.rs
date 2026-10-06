//! Core types for the MLS messaging layer.

use fauna_core::data::{ContentHash, Timestamp};
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A 32-byte channel identifier derived from the MLS group id.
///
/// Computed as `blake3::derive_key("fauna.channel.v1", group_id)`.
///
/// Serializes as a 32-byte CBOR byte string through its own serde impl, like
/// every serialized fixed-width byte field
/// (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
/// "Fixed-size byte arrays"), so a field of this type needs no attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub [u8; 32]);

impl Serialize for ChannelId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_bytes::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for ChannelId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde_bytes::deserialize(deserializer).map(Self)
    }
}

impl ChannelId {
    /// Derive a ChannelId from an MLS group id.
    pub fn from_group_id(group_id: &[u8]) -> Self {
        Self(fauna_core::folder_keys::channel_id_for_group(group_id))
    }

    /// Parse a hex-encoded 32-byte ChannelId. Surrounding whitespace is trimmed.
    pub fn from_hex(s: &str) -> Result<Self, fauna_core::hex32::Hex32Error> {
        fauna_core::hex32::decode(s).map(Self)
    }

    /// Derive a ChannelId from a hex-encoded MLS **group id** (not an
    /// already-derived `ChannelId` hex — use [`Self::from_hex`] for that).
    /// Plain hex decode, deliberately NOT `from_hex`/`hex32::decode`: the real
    /// MLS group id is not fixed-size (OpenMLS mints its own, 16 bytes today),
    /// and this KDF already treats it as an arbitrary-length slice. Surrounding
    /// whitespace is trimmed.
    pub fn from_group_id_hex(group_id_hex: &str) -> Result<Self, hex::FromHexError> {
        let raw = hex::decode(group_id_hex.trim())?;
        Ok(Self::from_group_id(&raw))
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

/// A message sent within a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// The actor who sent this message.
    pub sender: ActorId,
    /// Monotonically increasing sequence number within this sender's stream.
    pub sequence: u64,
    /// The MLS epoch at which this message was encrypted.
    pub channel_epoch: u64,
    /// The message content.
    pub body: ChannelMessageBody,
    /// When the message was created.
    pub timestamp: Timestamp,
}

/// The body of a channel message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelMessageBody {
    /// A plain text message.
    Text(String),
    /// A chat message carrying one or more attachments, with optional caption
    /// text. Each attachment's bytes are sealed as a **separate** per-channel
    /// blob — under the group's `derive_blob_key(epoch_secret)` on the
    /// end-to-end class; under the room's attachment content kind off the
    /// message's generation on the community class
    /// (`fauna_core::group_content::ROOM_ATTACHMENT_CONTENT_KIND`,
    /// `conversation-rooms.md` § The three classes → *Community* →
    /// *Attachments — the second content kind*) — and stored
    /// content-addressed on the nest (the canonical byte-source surface
    /// `PUT /api/v1/blob/{cid_b32}`); only the lightweight reference rides here
    /// (see [`ChannelAttachment`]). The receiver GETs each blob by its
    /// `sealed_cid`, `decrypt_blob`s it under the message's epoch blob key, and
    /// renders under `blob_hash` (the BLAKE3 of the *plaintext* — the uniform
    /// cross-rail content handle). This mirrors the SMTP rail's
    /// `multipart/mixed` body+attachments shape (`docs/goal/ui/conversations.md`
    /// § Attachments + § Encryption at rest).
    Attachments {
        /// Optional caption shown alongside the attachments (empty when none).
        body: String,
        /// The attachment references (always ≥ 1).
        attachments: Vec<ChannelAttachment>,
    },
    /// A device-sync control message.
    DeviceSync(DeviceSyncMessage),
    /// A group metadata change notification.
    GroupMeta(GroupMetaMessage),
    /// A CalDAV scheduling iMIP (`REQUEST`/`REPLY`/`CANCEL`) delivered to a
    /// **mailbox-less** Fauna attendee over a one-off MLS channel — the WS-RPC
    /// sealed-delivery rail for an attendee with CalDAV enabled but email
    /// disabled (`docs/goal/behavior/caldav-server.md` § Server-side
    /// auto-schedule). Carries the raw RFC 5322 message verbatim — the *same*
    /// bytes the email rail would carry (`fauna_core::ical::ImipMessage.raw_rfc5322`)
    /// — so the recipient's receive loop extracts the `text/calendar` part with
    /// the same `fauna_mail::extract_text_calendar_part` it uses for inbound mail
    /// and routes by METHOD to `CalDavClient::apply_inbound_*` (Slice 4). It
    /// never renders as a chat bubble: the channel is tagged
    /// [`WelcomeKind::Scheduling`](../../fauna_protocol/conversations/enum.WelcomeKind.html)
    /// on delivery, and the chat-display helpers (`channel_message_to_inbound`)
    /// ignore every non-`Text` body.
    Scheduling(#[serde(with = "serde_bytes")] Vec<u8>),
    /// Toggle one emoji reaction on one prior message in this channel.
    /// `target_seq` is the target message's nest segment-store seq (the
    /// cross-member-stable identity behind MessageId("conv:{channel}:{seq}")).
    Reaction {
        target_seq: u64,
        emoji: String,
        op: ReactionOp,
    },
    /// Sender-only cooperative tombstone for one of the sender's own prior
    /// messages. Compliant clients render a "deleted" placeholder; the original
    /// sealed envelope is never removed from the append-only store.
    Delete { target_seq: u64 },
    /// A custody-ceremony payload (`docs/goal/architecture/account-data-plane.md`
    /// § Replica posture → *The custody grant + ceremony*, W8.4 (account-data-plane.md § Workstreams)): the
    /// **verbatim canonical `fauna_core::custody_ceremony::CustodyCeremonyMessage`
    /// bytes** — an offer, accept, or witness delivery riding an established
    /// conversation between the owner and the host account. Carried as bytes
    /// and never re-encoded (the inner signatures cover exactly this
    /// encoding), the same discipline as
    /// [`GroupMetaMessage::Succession`].
    ///
    /// **Never a chat bubble**: the receive loop hands the bytes to the
    /// session-wired `CustodyCeremonySink` as a thread *effect* and renders
    /// nothing (`channel_message_to_inbound` ignores every non-`Text` body),
    /// so no app grows a render arm for it — the consent surface (T16) reads
    /// the durably captured state instead.
    ///
    /// **Trust:** the bytes are a *claim* until the sink's implementor
    /// verifies them (`fauna_core::custody_ceremony::verify_custody_*`) —
    /// and unlike the Succession statement, a ceremony step is its author's
    /// act, so verification also binds the payload's signer to the
    /// MLS-authenticated transport sender and (for an offer) its addressee
    /// to the reading actor. A forwarded or replayed payload conveys
    /// nothing.
    ///
    /// **Older clients:** exactly the Succession argument — a decoder that
    /// predates this variant fails the record's dag-cbor decode inside
    /// `MlsEngine::decrypt`, and the shared poll loop skips undecodable
    /// records without stalling the feed, which is what makes this variant
    /// additive-legal within the major (`version-compatibility.md` § Dim 2).
    /// The ceremony degrades honestly: the payload sits store-and-forward in
    /// the channel until a capable build reads it.
    Custody(#[serde(with = "serde_bytes")] Vec<u8>),
    /// One **custody receipt** — a custodian's signed, dated attestation of
    /// what it holds for this channel's other party, as the canonical bytes of
    /// its `EmbedAsBytes` wire shape (`fauna_core::custody_receipt`;
    /// `account-data-plane.md` § Replica posture → *Custody policy* (T15):
    /// "the custodian's periodic check-ins **are** the A7 custody receipts").
    ///
    /// **Deliberately its own body rather than a fourth
    /// `CustodyCeremonyMessage` variant**, for two reasons that point the same
    /// way. Semantically, the ceremony is a one-shot three-step handshake and
    /// its message enum is the security-critical part a strict decoder must
    /// refuse to guess at; a receipt is periodic and informational. And
    /// mechanically, that strictness has different consequences at the two
    /// levels: an unknown *ceremony* payload surfaces as a counted payload
    /// FAILURE on an older host — recurring, once per receipt, forever —
    /// whereas an unknown *body* is skipped by the shared poll loop, which is
    /// exactly the additive-legality argument the `Custody` variant above
    /// makes (`version-compatibility.md` § Dim 2). So an older owner simply
    /// never sees receipts and renders "no receipt yet", the honest state
    /// `ui/nests.md` § Trust facet already requires — instead of an error
    /// counter that climbs for a message it was never meant to read.
    ///
    /// The receiver verifies it against the custodian key its own grant named
    /// (`verify_custody_receipt`) before folding it onto the custody row; an
    /// unverifiable one is dropped, never rendered as coverage.
    CustodyReceipt(#[serde(with = "serde_bytes")] Vec<u8>),
    /// One member's **share-set endpoint advertisement** — where this member
    /// can be dialed for the shared file set this channel *is*, as the
    /// canonical bytes of `fauna_core::share_endpoints::ShareEndpoints`
    /// (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
    /// *Discovery*; the W8 share twin's slice F).
    ///
    /// **Why the set's own channel carries this**, rather than a nest door or
    /// a peer-plane kind: the contract requires that "peer endpoints [are]
    /// learned only through authenticated channels", and the group is already
    /// exactly that channel. Two properties fall out with no new machinery —
    /// only members can *read* an advertisement (it is group-sealed), and
    /// only a member can *make* one anybody believes (MLS authenticates the
    /// sending leaf). The T5 symmetry the same-account leg gets from the
    /// account plane, the share leg gets from the group.
    ///
    /// **Never a chat bubble**, the `Custody` discipline verbatim: the
    /// receive loop hands the bytes to the session-wired
    /// [`ShareEndpointsSink`](../../fauna_conversations/backend/trait.ShareEndpointsSink.html)
    /// as a thread *effect* and renders nothing.
    ///
    /// **Trust:** the payload's `member_actor` is *self-asserted* and fully
    /// attacker-controllable — an in-group member runs a patched client and
    /// writes any victim's id into it. It is a claim until
    /// `fauna_peer_share::bind_share_advertisement` binds it to the
    /// MLS-authenticated sender and to the carrying channel, refusing rather
    /// than repairing a mismatch. Without that binding any member of a set
    /// could durably redirect every other member's dials at a box it
    /// controls.
    ///
    /// **Older clients:** exactly the `Custody` argument — a decoder that
    /// predates this variant fails the record's dag-cbor decode inside
    /// [`MlsEngine::decrypt`](../../fauna_mls/engine/struct.MlsEngine.html#method.decrypt),
    /// and the shared poll loop skips undecodable records without stalling
    /// the feed, which is what makes the variant additive-legal within the
    /// major (`version-compatibility.md` § Dim 2). Such a client simply never
    /// caches peer candidates and falls back to the nest — which is the
    /// contract's own always-on source, so the degradation is honest.
    ShareEndpoints(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// The operation carried by a [`ChannelMessageBody::Reaction`] message —
/// whether the sender is adding or removing their emoji reaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReactionOp {
    /// The sender is adding this emoji reaction to the target message.
    Add,
    /// The sender is retracting a previously-added emoji reaction.
    Remove,
    /// An op a newer build writes and this one does not name — the exact
    /// string read, carried so a history slice's `reaction_log` merged and
    /// re-uploaded by this build keeps it (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*). It adds no reaction: the
    /// fold ranks it as [`Self::Remove`] and counts only [`Self::Add`]. No
    /// build sends one.
    #[serde(untagged)]
    Other(String),
}

/// A single attachment referenced by a [`ChannelMessageBody::Attachments`]
/// message. The bytes themselves are **not** here — they ride as a separate
/// sealed blob the receiver fetches by [`Self::sealed_cid`]
/// (`docs/goal/ui/conversations.md` § Attachments). Carrying only the reference
/// keeps the sealed channel message small regardless of attachment size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelAttachment {
    /// BLAKE3 of the **plaintext** bytes — the uniform render handle every
    /// app caches + renders off (matches `AttachmentSnapshot.blob_hash`).
    pub blob_hash: ContentHash,
    /// BLAKE3 of the **sealed** bytes — the nest content-address used to GET the
    /// blob back (`GET /api/v1/blob/{sealed_cid}`).
    pub sealed_cid: ContentHash,
    /// Original filename.
    pub filename: String,
    /// MIME type (drives the bubble's icon / inline-image decision).
    pub mime_type: String,
    /// Plaintext size in bytes.
    pub size_bytes: u64,
    /// Whether the attachment is an image (drives `dm-attachment-image[i]` vs
    /// `dm-attachment-file[i]`).
    pub is_image: bool,
    /// The MLS epoch the blob was sealed at — selects the right (possibly
    /// historical, grace-decrypt) blob key on the receiver. **End-to-end class
    /// only**: a community room's attachment opens under the naming message's
    /// generation (the `RoomSealed` envelope's, which the author's signature
    /// binds), so there this field is written `0` and never read.
    pub epoch: u64,
}

/// The result of sealing a conversation-attachment blob via
/// [`crate::engine::MlsEngine::seal_conversation_blob`]: the sealed bytes to
/// upload to the nest, their content-address (the GET key), and the MLS epoch
/// the seal used (stamped into the message's [`ChannelAttachment`] so the
/// receiver picks the right blob key).
#[derive(Debug, Clone)]
pub struct SealedConvBlob {
    /// The AEAD-sealed blob bytes to upload to the nest blob store.
    pub sealed: Vec<u8>,
    /// BLAKE3 of `sealed` — the nest content-address (the `sealed_cid` the
    /// receiver GETs by).
    pub sealed_cid: [u8; 32],
    /// The MLS epoch the seal used.
    pub epoch: u64,
}

/// The result of sealing a shared folder's **content-key envelope** via
/// [`crate::engine::MlsEngine::seal_content_key_envelope`]: the AEAD-sealed
/// generation bundle (the M2 mechanism — `docs/goal/architecture/key-material-hierarchy.md`
/// § M2 content-key mechanism) and the MLS epoch the seal used.
///
/// The owner seals the **full** `FolderContentKeys::generations` bundle under
/// the group's *current* epoch secret (a dedicated `"fauna.fileset-keys.v1"`
/// exporter, domain-separated from `"fauna.blob.v1"`/`"fauna.chunk.v1"`) and
/// publishes it nest-side opaque (`fauna.folders.content_key.put`), re-published
/// on every membership change. A member (Welcome → current epoch secret) reads it
/// and reconstructs the history via
/// [`crate::engine::MlsEngine::open_content_key_envelope`] →
/// `FolderContentKeys::from_generations`. `epoch` is carried as the sealing-epoch
/// metadata (staleness / which epoch the bytes decrypt under) — the read path
/// derives the *current* epoch envelope key, since the envelope is always
/// re-published at the current epoch on every membership change.
#[derive(Debug, Clone)]
pub struct SealedContentKeyEnvelope {
    /// The AEAD-sealed canonical-encoded generation bundle to publish nest-side
    /// (opaque ciphertext; the nest never holds the group secret).
    pub sealed: Vec<u8>,
    /// The MLS epoch the seal used (the current epoch at publish time).
    pub epoch: u64,
}

/// The product of [`MlsEngine::build_scheduling_delivery`](crate::engine::MlsEngine::build_scheduling_delivery):
/// everything needed to deliver a one-off MLS *scheduling* channel to a single
/// recipient over `welcome.deliver` + `channel.send`, without the caller holding
/// any group state afterward. The CalDAV auto-schedule **gateway** builds this
/// with an *ephemeral* engine (the MDA never holds the organizer's secret) and
/// ships the three byte-blobs over the caller-scoped WS-RPC rail; the Fauna
/// app rail builds it with the organizer's own engine
/// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule).
#[derive(Debug, Clone)]
pub struct SchedulingDelivery {
    /// The MLS Welcome the recipient processes to join the one-off group.
    pub welcome_bytes: Vec<u8>,
    /// The derived channel id (the recipient derives the same id from the Welcome).
    pub channel_id: ChannelId,
    /// The ready-to-post `ChannelEnvelope::Application` bytes carrying the iMIP as
    /// the group's first (and only) application message.
    pub app_envelope: Vec<u8>,
}

/// Messages used for syncing state across a single actor's devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceSyncMessage {
    /// A blob was added to the local store.
    BlobAdded {
        blob_hash: ContentHash,
        path: String,
        size_bytes: u64,
        media_type: String,
        created_at: Timestamp,
    },
    /// A blob was removed from the local store.
    BlobRemoved {
        blob_hash: ContentHash,
        path: String,
        removed_at: Timestamp,
    },
    /// File metadata was updated (e.g. rename).
    MetadataUpdated {
        blob_hash: ContentHash,
        old_path: String,
        new_path: String,
        updated_at: Timestamp,
    },
    /// Arbitrary key-value state sync (read state, drafts, preferences).
    StateSync {
        namespace: String,
        key: String,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
        updated_at: Timestamp,
    },
}

/// Notifications about group metadata changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GroupMetaMessage {
    /// The group name was changed.
    NameChanged(String),
    /// A member's role was changed.
    RoleChanged {
        /// The actor whose role changed.
        actor_id: ActorId,
        /// The new role name.
        new_role: String,
    },
    /// The in-group identity-succession statement
    /// (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS
    /// groups*): the **verbatim canonical `SignedIdentitySuccession` bytes**,
    /// posted by the succession sweep between the add-successor and remove-old
    /// commits so members render continuity instead of "someone added a
    /// stranger". Carried as bytes and never re-encoded — the signatures cover
    /// exactly this encoding, and every other plane (the nest's succession
    /// store, `fauna.recovery.succession.lookup`, the federation push) carries
    /// the same verbatim bytes.
    ///
    /// **Trust:** the bytes are a *claim* until verified. A consumer runs the
    /// § The succession statement verification rule
    /// (`fauna_core::recovery::SignedIdentitySuccession::verify` against a head
    /// it independently knows, or the anchored chain walk) before rendering any
    /// continuity effect. The MLS-authenticated transport sender is
    /// deliberately **not** part of that rule: any member may carry the
    /// statement (the member-side re-add remedy posts it too) — the signatures
    /// are the authority, and binding the sender would refuse a valid statement
    /// carried by an honest member.
    ///
    /// **Older clients:** a decoder that predates this variant fails the
    /// record's dag-cbor decode inside `MlsEngine::decrypt`, and the shared
    /// poll loop skips undecodable records without stalling the feed
    /// (`poll_inbound_conv`'s contract) — the member stays in the group and
    /// renders the bare add. That shipped skip-not-crash behavior is what makes
    /// this variant additive-legal within the major
    /// (`docs/goal/architecture/version-compatibility.md` § Dim 2).
    Succession(#[serde(with = "serde_bytes")] Vec<u8>),
    /// **History for a joiner** (`docs/goal/behavior/conversation-rooms.md`
    /// § History for joiners, the end-to-end arm): the inviting device's
    /// re-sealed `history/<channel_hex>` slice — the canonical dag-cbor bytes
    /// of `fauna_conversations::store::history::ChannelHistorySlice`, the
    /// same shape the user's own devices re-seal to one another — posted as
    /// an application message in the newcomer's first epoch. Produced only
    /// under the room policy's `full` history rule; an honest member refuses
    /// to produce one the policy does not authorize.
    ///
    /// ⚠ **Every member of that epoch opens it, and any member can seal
    /// one** — MLS authenticates who sealed it, not whether it is history. So
    /// the receiver decides (§ History for joiners → *What a device accepts*):
    /// only the device the sealing account admitted, in the epoch it joined
    /// at, once, under `full`, takes it — for its messages alone — and every
    /// other member refuses it. The slice is the inviter's **attestation** of
    /// the pre-join transcript, trusted exactly as far as `full` trusts the
    /// inviter.
    ///
    /// **Older clients:** additive-legal for the reason [`Self::Succession`]
    /// is — an undecodable record is skipped, never a stall.
    HistorySlice(#[serde(with = "serde_bytes")] Vec<u8>),
    /// **An ownership offer** (`docs/goal/behavior/conversation-rooms.md`
    /// § Roles and authorization → *Ownership transfer*): the canonical
    /// dag-cbor bytes of `crate::room_policy::RoomOwnershipOffer` — the
    /// policy naming the room's next owner and the outgoing owner's
    /// countersignature over it, bound to this channel. Posted by the owner's
    /// device; every member decrypts it, and only the named member's device
    /// acts, by signing the same policy and committing it with both
    /// signatures. Nothing changes hands until that commit is folded.
    ///
    /// **Older clients:** additive-legal for the reason [`Self::Succession`]
    /// is — an undecodable record is skipped, never a stall. An older member
    /// does, however, refuse the transfer *commit* itself (its judge admits
    /// no owner change off the succession chain) — a version skew of the
    /// policy's own rules that the extension type cannot express, accepted
    /// while no released app carries the room policy (2026-09-09).
    OwnershipOffer(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// Wire envelope for messages posted to a channel endpoint — re-exported from
/// its wire home `fauna-protocol` (the nest strict-decodes it engine-free, and
/// mls-free clients construct it; moved 2026-07-08 for the folder
/// Remove-commit distribution). Same type, same dag-cbor bytes; engine-side
/// users keep importing it from here.
pub use fauna_protocol::conversations::ChannelEnvelope;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_rides_as_a_32_byte_string() {
        let id = ChannelId([0x33; 32]);
        let bytes = fauna_cbor::encode_canonical(&id).unwrap();
        let mut expect = vec![0x58, 0x20];
        expect.extend_from_slice(&[0x33; 32]);
        assert_eq!(bytes, expect);
        assert_eq!(fauna_cbor::decode_strict::<ChannelId>(&bytes).unwrap(), id);
    }

    #[test]
    fn reaction_and_delete_bodies_round_trip() {
        let r = ChannelMessageBody::Reaction {
            target_seq: 42,
            emoji: "👍".into(),
            op: ReactionOp::Add,
        };
        let bytes = fauna_cbor::encode_canonical(&r).unwrap();
        let back: ChannelMessageBody = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            matches!(back, ChannelMessageBody::Reaction { target_seq: 42, ref emoji, op: ReactionOp::Add } if emoji == "👍")
        );

        let d = ChannelMessageBody::Delete { target_seq: 7 };
        let back: ChannelMessageBody =
            fauna_cbor::decode_strict(&fauna_cbor::encode_canonical(&d).unwrap()).unwrap();
        assert!(matches!(back, ChannelMessageBody::Delete { target_seq: 7 }));
    }

    /// A newer build's reaction op — modelled by a twin with one more variant
    /// — decodes into the carrying arm and re-encodes to the same bytes, so a
    /// history slice's `reaction_log` this build merges and re-uploads keeps
    /// it (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*). What it does in the fold is pinned beside the fold.
    #[test]
    fn an_unknown_reaction_op_is_carried_byte_identically() {
        #[derive(Serialize)]
        enum NewerReactionOp {
            #[allow(dead_code)]
            Add,
            Toggle,
        }
        #[derive(Serialize)]
        enum NewerBody {
            Reaction {
                target_seq: u64,
                emoji: String,
                op: NewerReactionOp,
            },
        }
        let bytes = fauna_cbor::encode_canonical(&NewerBody::Reaction {
            target_seq: 42,
            emoji: "👍".into(),
            op: NewerReactionOp::Toggle,
        })
        .unwrap();
        let back: ChannelMessageBody = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(matches!(
            &back,
            ChannelMessageBody::Reaction { op: ReactionOp::Other(op), .. } if op == "Toggle"
        ));
        assert_eq!(fauna_cbor::encode_canonical(&back).unwrap(), bytes);
    }

    #[test]
    fn succession_statement_body_round_trips_verbatim() {
        let bytes = vec![0x42u8; 96];
        let m = ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(bytes.clone()));
        let back: ChannelMessageBody =
            fauna_cbor::decode_strict(&fauna_cbor::encode_canonical(&m).unwrap()).unwrap();
        assert!(
            matches!(back, ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(b)) if b == bytes),
            "the carried statement bytes must survive the wire byte-for-byte"
        );
    }

    #[test]
    fn custody_ceremony_body_round_trips_verbatim() {
        let bytes = vec![0x1Du8; 140];
        let m = ChannelMessageBody::Custody(bytes.clone());
        let back: ChannelMessageBody =
            fauna_cbor::decode_strict(&fauna_cbor::encode_canonical(&m).unwrap()).unwrap();
        assert!(
            matches!(back, ChannelMessageBody::Custody(b) if b == bytes),
            "the carried ceremony bytes must survive the wire byte-for-byte"
        );
    }

    #[test]
    fn custody_receipt_body_round_trips_verbatim() {
        let bytes = vec![0xAEu8; 96];
        let m = ChannelMessageBody::CustodyReceipt(bytes.clone());
        let back: ChannelMessageBody =
            fauna_cbor::decode_strict(&fauna_cbor::encode_canonical(&m).unwrap()).unwrap();
        assert!(
            matches!(back, ChannelMessageBody::CustodyReceipt(b) if b == bytes),
            "the attestation's signed bytes must survive the wire byte-for-byte — \
             the owner re-verifies them, so a re-encode would break the signature"
        );
    }

    /// A receipt body is NOT a ceremony body: the two must stay distinguishable
    /// on the wire, or an older host's ceremony decoder would be handed
    /// attestations it is right to refuse.
    #[test]
    fn a_receipt_body_is_distinct_from_a_ceremony_body() {
        let bytes = vec![0x1Du8; 32];
        let ceremony =
            fauna_cbor::encode_canonical(&ChannelMessageBody::Custody(bytes.clone())).unwrap();
        let receipt =
            fauna_cbor::encode_canonical(&ChannelMessageBody::CustodyReceipt(bytes)).unwrap();
        assert_ne!(ceremony, receipt);
    }

    /// The older-client half of the additive-variant rule
    /// (`identity-succession.md` § Propagation → MLS groups;
    /// `version-compatibility.md` § Dim 2): a `GroupMetaMessage` variant this
    /// build does not know fails **that record's** decode — the error every
    /// shipped poll loop already skips (`poll_inbound_conv`: "records that fail
    /// to decode or decrypt are skipped, not fatal"). This build plays the
    /// older client against a future encoding, which is exactly the position a
    /// shipped app is in when a newer member posts a variant added after it.
    #[test]
    fn an_unknown_group_meta_variant_fails_only_that_records_decode() {
        // A future build's enums: the same wire names plus one this build has
        // never heard of. Serde's external tagging makes the variant name the
        // map key, so this encodes byte-identically to what that future build
        // would post.
        #[derive(Serialize)]
        enum FutureGroupMeta {
            #[allow(dead_code)]
            NameChanged(String),
            Frobnicate {
                knob: u64,
            },
        }
        #[derive(Serialize)]
        enum FutureBody {
            #[allow(dead_code)]
            Text(String),
            GroupMeta(FutureGroupMeta),
        }
        #[derive(Serialize)]
        struct FutureChannelMessage {
            sender: ActorId,
            sequence: u64,
            channel_epoch: u64,
            body: FutureBody,
            timestamp: Timestamp,
        }

        let future = FutureChannelMessage {
            sender: ActorId([7u8; 32]),
            sequence: 1,
            channel_epoch: 3,
            body: FutureBody::GroupMeta(FutureGroupMeta::Frobnicate { knob: 9 }),
            timestamp: Timestamp(1_700_000_000_000_000),
        };
        let bytes = fauna_cbor::encode_canonical(&future).unwrap();
        assert!(
            fauna_cbor::decode_strict::<ChannelMessage>(&bytes).is_err(),
            "an unknown variant must fail the record's decode (which the poll \
             loop skips) — not decode into something misleading"
        );

        // The control: the same future build's *known* variant still decodes,
        // so the failure above is the unknown variant, not the twin encoding.
        let known = FutureChannelMessage {
            body: FutureBody::GroupMeta(FutureGroupMeta::NameChanged("still fine".into())),
            ..FutureChannelMessage {
                sender: ActorId([7u8; 32]),
                sequence: 2,
                channel_epoch: 3,
                body: FutureBody::Text(String::new()),
                timestamp: Timestamp(1_700_000_000_000_000),
            }
        };
        let bytes = fauna_cbor::encode_canonical(&known).unwrap();
        let cm: ChannelMessage = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(matches!(
            cm.body,
            ChannelMessageBody::GroupMeta(GroupMetaMessage::NameChanged(ref l)) if l == "still fine"
        ));
    }
}
