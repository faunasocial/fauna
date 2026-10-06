//! User-facing WS-RPC payload types for the conversations surface — the
//! end-user channel/keypackage/welcome/room plane. T1 ships the channel
//! cluster (`fauna.conversations.channel.{send,fetch,list_for_actor}`);
//! T2 adds the keypackage cluster
//! (`fauna.conversations.keypackage.{upload,fetch,count}`); T3 adds
//! same-nest welcome (`fauna.conversations.welcome.deliver`); the
//! `fauna.conversations.room.*` family carries the operations of
//! `conversation-rooms.md` § The room, as part of the WS-RPC conversations
//! migration (tracked internally). (The nine `fauna.conversations.group.*`
//! kinds that once sat beside the room family were retired under the alpha
//! carve-out — § The group plane's fate, step 3.)
//!
//! Push counterparts (`fauna.conversations.channel.message` +
//! `fauna.conversations.welcome.received`) live in `push_events.rs`; this
//! module owns the request/reply shapes only.
//!
//! `channel_id` / `actor_id` are hex-encoded `String` (matches the
//! existing `ChannelMessagePayload.channel_id` shape on the push side);
//! ciphertext envelopes and key-package bytes are CBOR `bstr`
//! (`Vec<u8>` / `ByteBuf` with `serde_bytes`).
//!
//! Kind registry entries live in
//! `kind.rs::register_conversations_{channel,keypackage,welcome,room}_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use fauna_cbor::Value;

// ── The channel-post envelope (the bytes `ChannelSendRequest.envelope` carries) ──

/// Wire envelope for messages posted to a channel endpoint.
/// Distinguishes MLS application messages from protocol messages (commits).
///
/// Lives here (not `fauna-mls`) because it is a *wire* shape, not group crypto:
/// the nest strict-decodes it without an MLS engine (the commit-gate
/// classification in `segments::conv::append_locked` and
/// `EncryptedStorage::ingest_channel_envelope`), and mls-free clients construct
/// it (the folder owner's Remove-commit distribution in
/// `fauna-client-folders`). `fauna-mls::types` re-exports it, so engine-side
/// users are unaffected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelEnvelope {
    /// Encrypted application message (chat text, media, sync event).
    Application(#[serde(with = "serde_bytes")] Vec<u8>),
    /// MLS Commit — membership change or key update.
    Commit(#[serde(with = "serde_bytes")] Vec<u8>),
    /// A **community room's** application message: sealed under the room's
    /// generation key rather than an MLS message ratchet
    /// (`conversation-rooms.md` § The three classes → *Community*, the
    /// recipient-set scheme). The two classes share one storage shape and
    /// one read feed — this variant is what "differing only in which key the
    /// reader holds" looks like on the wire.
    ///
    /// `generation` is the 32-byte content-derived generation id, cleartext
    /// so a reader knows which of its held wraps to open, and bound into the
    /// AEAD's AAD by
    /// [`fauna_core::group_content::seal_group_content`] so relabelling it is
    /// a refusal rather than an opaque failure. The home nest is one wrap
    /// recipient, so — uniquely among conversation envelopes — it opens this
    /// one, for the derived views the class exists to allow.
    RoomSealed {
        #[serde(with = "serde_bytes")]
        generation: Vec<u8>,
        #[serde(with = "serde_bytes")]
        ciphertext: Vec<u8>,
    },
    /// A **community room's floor delete record**: an owner's or admin's
    /// delete of another member's message, as a signed record carried
    /// **unsealed** (`conversation-rooms.md` § Roles and authorization →
    /// *Delete any message — the mechanism* → *Community rooms*). The bytes are
    /// the canonical `fauna_mls::room_policy::SignedRoomFloorDelete` — opaque
    /// here for the reason this enum lives in this crate (a wire shape, not
    /// group crypto). Deliberately the one conversation envelope that is not
    /// an AEAD output: the room's home nest judges it *from the record alone*
    /// before storing it, because it may never open a send to decide whether
    /// to admit it, and the record names no content — a room, a log position,
    /// an author, a policy version.
    ///
    /// **Additive inside the major** (`version-compatibility.md`): an app that
    /// predates the variant fails this envelope's strict decode and skips the
    /// record without stalling its walk; a nest that predates it refuses the
    /// send at its own strict decode, loudly, storing nothing.
    RoomFloorDelete(#[serde(with = "serde_bytes")] Vec<u8>),
}

impl ChannelEnvelope {
    /// Encode the envelope to canonical dag-cbor bytes (the at-rest /
    /// channel-post wire shape; the nest strict-decodes it in
    /// `EncryptedStorage::ingest_channel_envelope`). The opaque
    /// ciphertext/commit bytes ride as a CBOR byte string (`serde_bytes`).
    pub fn to_bytes(&self) -> std::result::Result<Vec<u8>, String> {
        crate::codec::encode_canonical(self)
            .map(|b| b.to_vec())
            .map_err(|e| e.to_string())
    }

    /// Decode an envelope from canonical dag-cbor bytes (strict: rejects
    /// non-canonical input).
    pub fn from_bytes(data: &[u8]) -> std::result::Result<Self, String> {
        crate::codec::decode_strict(data).map_err(|e| e.to_string())
    }
}

// ── fauna.conversations.channel.send ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSendRequest {
    pub channel_id: String,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Optional **device-owned-epoch commit precondition** (cross-device MLS
    /// state sync, `docs/goal/behavior/devices.md` § Cross-device MLS
    /// group-state sync). When set to a channel `seq`, the nest rejects the send
    /// with `fauna.conversations.channel.stale` iff any `ChannelEnvelope::Commit`
    /// record has landed on the channel *after* that seq — serializing commits so
    /// two of a user's devices never advance the same MLS leaf's ratchet in one
    /// epoch. A client posting an MLS commit sets this to the highest seq it has
    /// locally processed; an application send omits it. Absent ⇒ today's blind
    /// append (optional — every application send omits it). `i64` to match the conv-seq domain
    /// (`ChannelFetchRequest::after`, `ChannelSendReply::seq`, `next_conv_seq`);
    /// wire-identical to a non-negative `u64` under dag-cbor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_no_commit_since: Option<i64>,
    /// **Plaintext content addresses of the sealed attachment blobs the
    /// (sealed) body of this envelope names** — the conversation kind's
    /// blob-reachability floor, ratified 2026-09-08
    /// (`docs/goal/architecture/encryption-at-rest.md` § Per-content-kind
    /// conformance → Conversation messages row; `docs/goal/ui/conversations.md`
    /// § Encryption at rest → *Attachment reachability*). Each entry is the
    /// 64-hex BLAKE3 of one sealed blob the sender already uploaded through
    /// `POST /api/v1/blob` (`ChannelAttachment::sealed_cid`). The nest cannot
    /// read the body, so without this list nothing on the box names those
    /// blobs and the blob GC sweeps them ~30 minutes after upload
    /// (`backup-restore.md` § 9 step 2). The nest stores the list beside the
    /// record's mirror row and pins every listed blob for as long as the record
    /// is live; it never opens, serves or relays the hashes.
    ///
    /// Additive both ways: empty on the wire for every non-attachment send
    /// (`skip_serializing_if`, so the key is absent); a message without it
    /// leaves its attachments unpinned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSendReply {
    pub seq: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.channel.send_remote ────────────────────────────

/// `fauna.conversations.channel.send_remote` — a **foreign member's** send: the
/// channel's log lives on another nest (learned from the Welcome's `nest_url`),
/// so the member's own nest relays this envelope there via
/// `fauna.federation.channel.append` (`direct-messages.md` § step 3b;
/// `federation.md` § Cross-nest shared folders + channel append).
///
/// A **distinct kind** — deliberately NOT an additive `nest_url` on
/// `channel.send` — per the ratified client-kind wire rule: an old,
/// relay-unaware nest ignoring an additive field would append to its own local
/// log, silently perpetuating the send blackhole; an unknown kind fails loud
/// (typed `unknown_kind` → "your nest needs an update"). The client picks
/// `send` vs `send_remote` by whether it holds a foreign `home_nest_url` for
/// the channel — the same signal that drives the `channel.fetch` relay.
///
/// Replies with the same [`ChannelSendReply`] (the home nest's assigned seq).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSendRemoteRequest {
    pub channel_id: String,
    /// The channel's home nest base URL (from the recorded Welcome `nest_url`).
    /// Required and non-empty — a same-nest channel uses `channel.send`.
    pub nest_url: String,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Rides through to the home nest's seq-locked append (the device-owned-epoch
    /// commit gate) exactly as on `channel.send`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_no_commit_since: Option<i64>,
    /// **Plaintext content addresses of the sealed attachment blobs the
    /// (sealed) body of this envelope names** — the conversation kind's
    /// blob-reachability floor, ratified 2026-09-08
    /// (`docs/goal/architecture/encryption-at-rest.md` § Per-content-kind
    /// conformance → Conversation messages row; `docs/goal/ui/conversations.md`
    /// § Encryption at rest → *Attachment reachability*). Each entry is the
    /// 64-hex BLAKE3 of one sealed blob the sender already uploaded through
    /// `POST /api/v1/blob` (`ChannelAttachment::sealed_cid`). The nest cannot
    /// read the body, so without this list nothing on the box names those
    /// blobs and the blob GC sweeps them ~30 minutes after upload
    /// (`backup-restore.md` § 9 step 2). The nest stores the list beside the
    /// record's mirror row and pins every listed blob for as long as the record
    /// is live; it never opens, serves or relays the hashes.
    ///
    /// Additive both ways: empty on the wire for every non-attachment send
    /// (`skip_serializing_if`, so the key is absent); a message without it
    /// leaves its attachments unpinned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.blob.write_token.get ───────────────────────────

/// `fauna.conversations.blob.write_token.get` — the wire name of the
/// cross-nest attachment upload's mint relay (the client twin of
/// `fauna.federation.conversation.write_token.mint`).
pub const KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET: &str =
    "fauna.conversations.blob.write_token.get";

/// `fauna.conversations.blob.write_token.get` — a **foreign member** asks its
/// own nest to relay a short-lived, write-only bulk-byte token from the
/// channel's HOME nest, so its sealed attachment bytes can be POSTed DIRECT to
/// that home nest's `POST /api/v1/blob` (bulk bytes never ride the federation
/// channel — `transport.md` § carve-out). The blob twin of
/// [`ChannelSendRemoteRequest`]: the room's attachment bytes rest on the room's
/// home nest, beside the record and the plaintext `attachment_refs` that pin
/// them (`conversation-rooms.md` § The home nest → *Attachment bytes*, ratified
/// 2026-09-09; `conversations.md` § Encryption at rest → *Attachment
/// reachability*). Always a relay — a same-nest member uploads under its own
/// session bearer — so `nest_url` is required and non-empty, exactly as on
/// `send_remote`. Mirrors `fauna.folders.write_token.get` one rail over; the
/// home nest applies the structural foreign-member gate (`require_foreign_member`)
/// before minting, with no `access == 'writer'` arm because a conversation has
/// no claimant and every member posts.
///
/// A distinct kind, per the ratified client-kind wire rule: an old, relay-unaware
/// own nest fails loud (`unknown_kind` → "your nest needs an update") rather than
/// minting a token for its own store, where the bytes would serve nobody.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationBlobWriteTokenGetRequest {
    pub channel_id: String,
    /// The channel's home nest base URL (from the recorded Welcome `nest_url`).
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.conversations.blob.write_token.get` reply — the opaque write-only
/// bulk token + its absolute expiry (Unix seconds). The client POSTs the sealed
/// attachment to the home nest URL it already holds, bearing this token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationBlobWriteTokenGetReply {
    pub token: String,
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.channel.fetch ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelFetchRequest {
    pub channel_id: String,
    /// Cursor; the reply contains messages with `seq > after`. Matches
    /// the HTTP twin's `?after=N` query.
    #[serde(default)]
    pub after: i64,
    /// Caller-supplied page size; the handler clamps to `[1, 500]`.
    #[serde(default = "default_fetch_limit")]
    pub limit: i64,
    /// Spec Y2 cross-nest relay (mirrors `KeypackageFetchRequest.nest_url` /
    /// `WelcomeDeliverRequest.nest_url`): when set (and non-empty), the channel's
    /// **home** nest URL — the nest that holds the channel's message log (the group
    /// creator's nest). The caller's home nest originates
    /// `fauna.federation.channel.fetch` there; the home nest serves it only if the
    /// caller is a recorded member of the channel and the request arrives over the
    /// federation channel verified to the caller's home nest
    /// (`docs/goal/behavior/direct-messages.md` § Technical Flow — Cross-Nest, step 3;
    /// `docs/goal/architecture/federation.md` § Federation residue surface). Absent /
    /// empty ⇒ same-nest fetch from the local log.
    #[serde(default)]
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

fn default_fetch_limit() -> i64 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelFetchReply {
    pub messages: Vec<ChannelFetchEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChannelFetchEntry {
    pub seq: i64,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Present iff this message has been taken down under a legal obligation
    /// (the conversation twin of `PostGetReply.legal_takedown`; `moderation.md`
    /// § Categories & enforcement item 1 — the narrow, appealable legal-
    /// compulsion carve-out). When `Some`, the nest has **withheld** the sealed
    /// `envelope` (it is empty) from this relay fetch and the client renders the
    /// tombstone in its place via the shared `legalTakedownTombstone(reference)`
    /// (`fauna_core::obligation::legal_takedown_tombstone`) — no client hand-
    /// rolls the string. **Additive** `Option`: an older client that doesn't
    /// know the field still receives an empty envelope (never the illegal
    /// content) and simply doesn't render the tombstone. Best-effort at the
    /// relay only — the nest cannot recall a message already delivered to a
    /// device that synced before the takedown (E2E; `moderation.md`
    /// § Implementation status today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown: Option<crate::posts::LegalTakedownMarker>,
    /// A **community room's** category verdicts for this message — what the
    /// labelers the room names produced when its home nest opened the message
    /// to index it (`community-rooms.md` § The three classes → *What the home
    /// nest does with its read*, purpose 2). One entry per category, the
    /// highest-confidence one, exactly the per-row unit `FeedPostItem.labels`
    /// carries (`moderation.md` § Per-row badge data path); the app merges
    /// them into the message's own labels, a server entry winning its
    /// category.
    ///
    /// **Filled only for a live floor member**, although the envelope beside
    /// it is served to any routing-roster caller: sealed bytes reveal nothing
    /// the class does not already accept, but a verdict is derived from the
    /// plaintext. A member homed on another nest receives them too, on the
    /// fetch its own nest relays with `nest_url` set: the room's home nest
    /// fills them for that member on `fauna.federation.channel.fetch`, and the
    /// relaying nest forwards them and stores none. Empty for every other
    /// class and caller, for a message no labeler labelled, and beside a
    /// withheld envelope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The same message's **factor rows** — one tier-3 `labeler:<id>` score
    /// per named labeler that ran, `wasm` and `text-model` alike (a
    /// `text-model` scores without naming a category, so this is the only
    /// place its output reaches a member). Same gate and same absences as
    /// [`Self::labels`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<fauna_core::scoring::ScoreEntry>,
    /// The record's **nest-attested author** — 64-hex id of the actor the
    /// channel's home nest authenticated as the poster of this record (its
    /// `channel.send` caller; for the MDA scheduling gateway the organizer the
    /// nest caller-scoped the bridge to). The envelope is sealed, so this is a
    /// statement *by the home nest*, only as honest as that nest: a reader
    /// binds it together with the channel's home, never alone
    /// (`caldav-server.md` § Who may mutate an existing event over the inbound
    /// rail — its one consumer today). Relayed unchanged on
    /// `fauna.federation.channel.fetch`. **Additive** `Option`: absent for a
    /// record a nest-side writer (compaction, a restore) appended with no
    /// authenticated sender, and for a page whose authors read faulted
    /// (`page_authors`, fail-closed), which a reader treats as *no answer* —
    /// never as permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.channel.list_for_actor ─────────────────────────

/// Empty — the WS-RPC channel knows its caller. The HTTP twin's path
/// param (`/api/v1/channels/{actor_id}`) and bearer-match check are
/// implicit on the WS-RPC plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelListForActorRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelListForActorReply {
    /// Hex-encoded channel IDs the caller is registered on.
    pub channels: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.channel.actors ─────────────────────────────────

/// The inverse of [`ChannelListForActorRequest`]: *which actors* are on one
/// channel's routing roster (`actor_channels`). Member-scoped — the caller must
/// themselves be on the roster (the handler's own authz check), which leaks
/// nothing new: every MLS member already reads the full membership off the
/// group's ratchet tree.
///
/// The roster is written at Welcome delivery (and on a member's first channel
/// post), so **its absence** is the owner-readable signal for **"this member's
/// Welcome never landed"** — the discriminator that separates a healthy member from
/// a post-crash *phantom leaf* (in the MLS group, never welcomed) in
/// `add_participant`'s heal. See `mls-group-key-material.md` § M2 *Admitting a
/// member*.
///
/// ⚠ **Only the absence direction is exact.** Presence is *not* proof a Welcome
/// landed: on an **unclaimed** channel `channel.fetch`/`send` auto-register any
/// authenticated local caller, so on such channels `actor_channels` is an
/// **activity log**, not a Welcome witness. A phantom-leaf actor who fetches the
/// channel therefore auto-registers themselves and the heal's on-roster arm takes
/// its idempotent no-op, leaving the phantom unhealed. Declared residual: it needs a client to fetch a channel it
/// holds no Welcome for, costs no confidentiality, cannot be induced by a third
/// party (auto-register writes a row for *the caller*), and remove-then-add
/// recovers it. Restoring the exact equivalence needs a Welcome-specific witness
/// (a `welcomed_at`/origin column) — captured, not built.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelActorsRequest {
    /// Hex-encoded channel id (32 bytes) to read the roster of.
    pub channel_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelActorsReply {
    /// Hex-encoded actor IDs registered on the channel.
    pub actors: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.channel.actors_remote ──────────────────────────

/// `fauna.conversations.channel.actors_remote` — a **foreign member's** roster
/// read: the channel's authoritative roster lives on its home nest (learned
/// from the Welcome's `nest_url`), so the member's own nest relays the read
/// there via `fauna.federation.channel.actors` (`federation.md` § Cross-nest).
///
/// A **distinct kind** — deliberately NOT an additive `nest_url` on
/// `channel.actors` — for a sharper reason than `send_remote`'s: an old
/// member-nest ignoring an additive field would answer its own **partial**
/// roster as a clean success, and the add-participant heal consuming it would
/// read a healthy member homed elsewhere as a phantom and **evict** them under
/// supported version skew. An unknown kind fails loud, and the client's
/// error→"roster unreadable" mapping lands in the heal's refuse arm — the
/// correct degradation on every skew pairing (`federation.md` § Cross-nest,
/// the fetch-vs-actors dividing line: fetch's old-nest degrade is a benign
/// stale read; the actors read feeds a membership-mutating heal).
///
/// Replies with the same [`ChannelActorsReply`] (the home nest's union of
/// `actor_channels` and `channel_foreign_members` — hex actor ids only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelActorsRemoteRequest {
    /// Hex-encoded channel id (32 bytes) to read the roster of.
    pub channel_id: String,
    /// The channel's home nest base URL (from the recorded Welcome `nest_url`).
    /// Required and non-empty — a same-nest channel uses `channel.actors`.
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.welcome.deliver ────────────────────────────────

/// What sort of conversation the MLS Welcome opens. Replaces the HTTP
/// twin's loose `?channel_type=&group_id=` query pair with a closed
/// tagged enum — the dispatcher knows at decode time whether
/// `group_id` is required.
///
/// Wire-tag values match the legacy strings in `WelcomePayload.channel_type`
/// ("dm" / "group") so push-event consumers don't need a translation table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum WelcomeKind {
    Dm,
    Group {
        /// Hex-encoded group id (32 bytes).
        group_id: String,
    },
    /// A one-off MLS channel carrying a CalDAV scheduling iMIP
    /// (`REQUEST`/`REPLY`/`CANCEL`) to a **mailbox-less** Fauna attendee (CalDAV
    /// enabled, email disabled — `caldav-server.md` § Server-side auto-schedule).
    /// Wire tag `"scheduling"`. Like [`Dm`](Self::Dm) it carries no `group_id`:
    /// the channel is a 1:1 organizer→attendee delivery addressed by
    /// `channel_id`. The tag tells the recipient's receive loop to route this
    /// channel's application messages to the calendar-apply path
    /// (`fauna_client_caldav::CalDavClient::apply_inbound_*`), **not** the
    /// conversation UI — so a scheduling delivery never surfaces as a chat
    /// thread (no new standing-key surface; the iMIP rides the existing MLS
    /// welcome transport). Producer: the organizer's scheduling sender (Slice 3);
    /// consumer: the recipient receive-loop scheduling branch (Slice 4).
    Scheduling,
    /// A Welcome admitting the recipient to a **cross-user shared folder**'s
    /// MLS group (`docs/goal/ui/folders.md` § Sharing a folder). Carries the
    /// hex-encoded raw group id like [`Group`](Self::Group). Wire tag
    /// `"folder"`. Like [`Scheduling`](Self::Scheduling), the tag tells the
    /// recipient's receive loop to route this Welcome to the **folder
    /// pending-share surface**, NOT the chat UI — a shared folder is not a
    /// conversation, so a `Group` welcome here would surface a phantom chat
    /// thread. Producer: `fauna_client_folders::FoldersAuthor::share_set`;
    /// consumer: the recipient receive-loop folder arm (the contact-status
    /// gate + Welcome-staging is a separate slice — see `folders.md` §
    /// Sharing, *Recipient routing + gate*).
    Folder {
        /// Hex-encoded group id (32 bytes).
        group_id: String,
    },
}

/// Same-nest only. Cross-nest welcomes continue to arrive over the
/// HTTP twin `POST /api/v1/welcome/{actor_id}?nest_url=…` (federation
/// residue per `docs/goal/architecture/transport.md` § HTTP residue).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WelcomeDeliverRequest {
    /// Hex-encoded recipient actor id (32 bytes). Same-nest only —
    /// cross-nest fan-out lives on the HTTP twin.
    pub recipient_actor_id: String,
    /// Hex-encoded MLS channel id (32 bytes). The handler auto-registers
    /// the recipient on this channel, matching the HTTP twin.
    pub channel_id: String,
    /// Raw MLS Welcome bytes — fed to `MlsManager::process_welcome` on
    /// the recipient side.
    #[serde(with = "serde_bytes")]
    pub welcome_bytes: Vec<u8>,
    pub kind: WelcomeKind,
    /// Spec Y2 cross-nest relay: when set (and non-empty), the recipient's
    /// foreign nest URL. The home nest signs and forwards the Welcome there;
    /// absent / empty ⇒ same-nest delivery (the local `push_inbox` path).
    /// Defaulted for forward-compat.
    #[serde(default)]
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WelcomeDeliverReply {
    /// Inbox row id assigned by `CacheDb::push_inbox`. Same shape as
    /// the HTTP twin's `{"id": <i64>}` reply.
    pub inbox_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.keypackage.upload ──────────────────────────────

/// Self-upload one or more MLS key packages. The caller is implicit
/// (the connection's actor); the HTTP twin's path-param-vs-bearer match
/// check is implicit on the WS-RPC plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageUploadRequest {
    /// Raw key-package bytes (one entry per KP). Hex-decoded out of the
    /// HTTP twin's `Vec<{data: String}>` shape; on the WS-RPC plane the
    /// CBOR `bstr` carries the bytes natively.
    pub packages: Vec<ByteBuf>,
    /// Spec Y2: when `true`, these are reusable **last-resort** key packages
    /// (standard MLS) — the nest stores them so `take_key_package` falls back to
    /// one once the one-time pool drains, keeping the actor reachable. Every
    /// actor publishes exactly one at onboarding. Defaulted `false` (one-time)
    /// for forward-compat. See `docs/goal/architecture/federation.md` § Key
    /// packages.
    #[serde(default)]
    pub last_resort: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageUploadReply {
    pub stored: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.keypackage.fetch ───────────────────────────────

/// Fetch (consume — FIFO oldest non-expired) one key package belonging to
/// `actor_id`. WS-RPC kind is same-nest only; the cross-nest unauthed
/// bare-HTTP twin lives on as federation residue per
/// `docs/goal/architecture/transport.md` § HTTP residue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageFetchRequest {
    pub actor_id: String,
    /// Spec Y2 cross-nest relay: when set (and non-empty), the foreign nest URL
    /// that holds `actor_id`. The home nest signs and forwards the fetch there
    /// (the receiving nest authenticates the nest-signature). Absent / empty ⇒
    /// same-nest fetch. Defaulted for forward-compat.
    #[serde(default)]
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageFetchReply {
    /// `None` ⇔ no non-expired key package available (the HTTP twin's
    /// `404 NOT_FOUND`). Empty `Vec<u8>` would be ambiguous, so `Option`
    /// is the wire signal.
    #[serde(default, with = "serde_bytes")]
    pub key_package: Option<Vec<u8>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.keypackage.count ───────────────────────────────

/// Non-destructive count of remaining non-expired key packages for
/// `actor_id`. Used by senders to gauge whether the FIFO consume on
/// `keypackage.fetch` will succeed before initiating a chat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageCountRequest {
    pub actor_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeypackageCountReply {
    pub count: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.roster_report ─────────────────────────────

/// One principal on a reported floor roster.
///
/// A **user** principal today: the report comes from a member device of an
/// end-to-end room, whose member set is user principals by construction
/// (`conversation-rooms.md` § The three classes — any nest or bridge member
/// would make the room a different class, and neither reports). The kind
/// column exists on the nest's table for the classes that follow; the wire
/// does not carry it, so a report can never assert a principal kind that
/// would change a room's derived class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRosterEntryWire {
    /// Hex-encoded actor id (32 bytes).
    pub actor: String,
    /// `owner` / `admin` / `member` — the three-role vocabulary of
    /// `conversation-rooms.md` § Roles and authorization. **Absent on a
    /// policy-less room** (a 1:1, or a group whose context carries no
    /// policy): it has no roles, and the report says so rather than
    /// inventing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Report an end-to-end room's membership to its home nest — the
/// **member-reported floor roster** (`conversation-rooms.md` § The floor
/// roster → *End-to-end rooms*).
///
/// The MLS group is the membership authority and the nest cannot see inside
/// it, so after every membership commit it authors on a governed room the
/// committing device reports the resulting roster, each member with the
/// role the room policy gives them. The nest **stores the report as the
/// room's floor roster**, replacing the previous one wholesale, and uses it
/// for what a nest legitimately decides about membership: routing fan-out,
/// the custody serve door
/// (`account-replica-posture.md` § Shared-audience carve-out), the
/// cross-nest relay gate and succession targets.
///
/// **It decides nothing about confidentiality.** A wrong report cannot make
/// a non-member read a message, because reading is MLS's — which is exactly
/// why the nest can accept a self-reported roster at all. What the two
/// authorization gates buy is narrower and worth stating. The reporter must
/// be on the channel's **routing roster** (`actor_channels`), which proves
/// it is a live participant of this channel on this nest rather than a
/// caller that guessed a channel id; and — the **ratchet** — it must be a
/// live member of the roster it is replacing. The reporter of a membership
/// commit is by construction a member of the roster that commit replaced:
/// true of an add, of a remove, and of a departing member's final report
/// alike. Naming yourself in the *new* roster is deliberately not enough —
/// that is the self-assertion the record exists to exclude, and its concrete
/// shape is a departed member walking back in past a routing row that
/// survives departure. A room with no stored roster yet has nothing to
/// ratchet against; that bootstrap bound is declared in
/// `conversation-rooms.md` § Implementation status today.
///
/// Wholesale replacement is the contract, not an optimization: a membership
/// commit produces a complete roster, and a delta protocol between an
/// authority the nest cannot read and a mirror it can would have no way to
/// resynchronize after a single missed report.
///
/// `forbid_replay=false` — the report is idempotent by construction
/// (replacing a roster with the same roster is a no-op) and carries no
/// externally-visible side effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRosterReportRequest {
    /// Hex-encoded room id (32 bytes). For an end-to-end room this is the
    /// MLS channel id (`conversation-rooms.md` § The room — `room_id` is
    /// the channel id, already derived from the group's own state).
    pub room_id: String,
    /// The roster as the committing device now holds it. One entry per
    /// principal; a duplicate or an empty roster is refused.
    pub members: Vec<RoomRosterEntryWire>,
    /// The policy version the roles were read under; `None` on a
    /// policy-less room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<u64>,
    /// The room log's `seq` of the commit this roster follows — what the
    /// commit's own `channel.send` answered. It is what **orders** reports
    /// (`conversation-rooms.md` § The floor roster): the home nest refuses a
    /// position above the newest commit its log has carried and does not
    /// apply one at or below the position its floor already holds, so a
    /// report that arrives late can never roll membership or roles back.
    /// `policy_version` cannot do this — an ordinary add or remove does not
    /// change it.
    ///
    /// Absent on a report that follows no commit — a leave, the birth report,
    /// the floor backfill (`fauna_conversations::backends::fauna_mls`) — which
    /// applies unordered and leaves the stored position untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack. The stored count is echoed so a reporter learns the roster landed
/// whole without a read-back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRosterReportReply {
    /// How many principals the floor roster now holds live for this room.
    pub members: u32,
    /// Set when this report was **not applied** because the floor already
    /// holds the roster of a later commit — the value is that commit's
    /// position ([`RoomRosterReportRequest::commit_seq`]). Not a failure: the
    /// floor is at least as new as what was reported. Absent on an applied
    /// report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.create ────────────────────────────────────

/// Found a room — the **birth ceremony** of the community class
/// (`conversation-rooms.md` § The room; § The home nest — "a room is born on
/// its creating member's home nest").
///
/// The request carries no room id and no class. The id is *derived* from the
/// birth record (`fauna_mls::room_policy::derive_room_id` over the creator's
/// key and `salt`), so a founder cannot name a room somebody else's key
/// commits to; and the class is derived from the member set the ceremony
/// seats (§ Architectural rules, rule 1 — "class is derived, never stored,
/// never chosen"). The ceremony seats two principals: the creator as the
/// room's one **owner**, and the home nest — the nest this call reached — as
/// an ordinary **member**, which is what makes the derived class
/// `community`. That is § Don't do these' first bullet read literally: the
/// nest's readable position is a membership fact on the roster, never a
/// "make this room readable" flag, and it exists from the room's first
/// instant rather than being granted over an existing transcript.
///
/// End-to-end rooms are **not** born here. They are born in MLS
/// (`FaunaMlsBackend::bootstrap_group`) and their membership reaches the
/// nest through [`RoomRosterReportRequest`], the mirror door. The two
/// provenances are exclusive: a room founded by this ceremony is
/// floor-authoritative and the report door refuses it.
///
/// `forbid_replay=false` — the id is content-derived and the seating is an
/// upsert, so replaying the byte-identical birth record re-derives the same
/// room and changes nothing. There is no externally-visible side effect: no
/// invite is fanned out, no peer is contacted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomCreateRequest {
    /// Hex-encoded 32-byte salt. Half the birth record; it is what keeps the
    /// derived id unguessable and lets one principal found many rooms.
    pub salt: String,
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomPolicy` — the
    /// room's initial policy, **signed by the creator**, whose `owner` field
    /// is the other half of the birth record. Version 1, and its admin set
    /// is necessarily empty: an admin is a member with a rank, and at birth
    /// the only user principal on the floor is the owner.
    #[serde(with = "serde_bytes")]
    pub policy: Vec<u8>,
    /// The creator's **group-reception public key** — its wrap target in the
    /// recipient-set scheme (`account-data-taxonomy.md` § The recipient-set
    /// scheme; `key-material-hierarchy.md` § Audience: a storage group).
    /// Recorded on the founding roster row so the first generation mint has
    /// somebody to wrap to. Additive and optional: empty seats the owner with
    /// no wrap target, which is exactly the state a member top-up heals.
    #[serde(
        default,
        with = "serde_bytes",
        skip_serializing_if = "Vec::is_empty",
        rename = "reception_pubkey"
    )]
    pub reception_pubkey: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The founded room's derived id, echoed so the creator learns it without
/// re-deriving — though it can, and the conformance test does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomCreateReply {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.list_roster ───────────────────────────────

/// One principal as the floor roster renders it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRosterMemberWire {
    /// Hex-encoded principal id (32 bytes). A user principal's is its actor
    /// id; the home nest's is its `nest_id` (`fauna.nest.info`).
    pub principal: String,
    /// `user` / `nest` / `bridge` (`conversation-rooms.md` § The room →
    /// *Principals*). The class is derived from these
    /// (`fauna_conversations::room::derive_room_class`), which is why the
    /// read carries them rather than a class word the nest computed.
    pub kind: String,
    /// `owner` / `admin` / `member`; absent on a policy-less room, which carries
    /// no roles at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// When this principal joined, epoch millis.
    pub joined_at: i64,
    /// The principal's public handle, when this nest knows it. `Some` for a
    /// local user with a handle set, and for a member homed on **another**
    /// nest whose own home nest has announced it (below); `None` for a
    /// foreign member not yet announced, for a handle-less local user, and
    /// for every non-user principal. The `fauna.contacts.list` shape
    /// ([`crate::contacts::ContactItem`]), plus the foreign arm.
    ///
    /// ⚠ **Joined nest-side, never taken from a roster report.** A local
    /// principal's comes from this nest's own `users.handle`. A foreign
    /// principal's is the `handle@domain` its home nest — the authority for
    /// handles at its domain — volunteered on the member's own relayed drain,
    /// which this nest recorded only after binding that domain to the
    /// announcing nest's key through discovery (`federation.md` § Cross-nest
    /// shared folders + channel append, the id→handle bullet). No nest ever
    /// *asks* another what handle an actor wears. For an end-to-end room the
    /// floor roster is a *member-reported mirror* (`conversation-rooms.md`
    /// § The floor roster), so a handle riding the report would be
    /// attacker-chosen — the hazard
    /// `fauna_conversations::address::TypedAddress::same_participant`'s doc is
    /// written about. It is **display only**: every membership decision keys
    /// on `principal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The handle's domain — this nest's handle domain for a local principal,
    /// the announced-and-verified domain for a foreign one, `None` whenever
    /// `handle` is (a handle-less local user still carries this nest's
    /// domain). Pairs with `handle` to form the canonical `handle@domain` a
    /// resolved Fauna address displays (`FaunaMlsBackend::resolve_address`'s
    /// "canonical `localpart@domain` display, regardless of which form was
    /// typed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Hex-encoded roster entry id (32 bytes) — the slot a generation wrap is
    /// bound to, and half the AAD its open re-derives
    /// ([`RoomGenerationWire::entry_id`], the same value from the holder's
    /// side). `None` for a keyless seat (the succession-seated successor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    /// The group-reception public key this principal handed the room when it
    /// was seated — the X-Wing wrap target a mint addresses to it.
    ///
    /// ⚠ **This pair is what makes minting reachable from an app at all.**
    /// A mint is admissible only when its wrap set covers the room's Enrolled
    /// roster (`account-data-taxonomy.md` § The recipient-set scheme, the
    /// generations bullet's roster-coverage rule), so a minter
    /// must read the roster and wrap to all of it. Serving the pair is the
    /// roster entry's own defined value — "the member's actor key, the
    /// reception pubkey observed at add, `Enrolled | Removed`, stamps" (same
    /// § → *The roster kind*) — not a widening of the read: a public wrap
    /// target is exactly what co-members are meant to hold, and it is what
    /// lets a *member* check that a mint covered it rather than take the
    /// nest's word.
    ///
    /// `None` for a principal seated without one; an **empty** vector never
    /// occurs here (the nest stores no such row as a wrap target — it is
    /// "unkeyable", which the coverage check skips), which is why this is an
    /// `Option` rather than a possibly-empty `Vec`.
    #[serde(default, with = "serde_bytes")]
    pub reception_pubkey: Option<Vec<u8>>,
    /// Whether the room's **current** generation carries a wrap bound to this
    /// principal's roster entry — the one fact about a room's key material a
    /// member needs and `room.generations` cannot give it, since that door
    /// serves each caller its own wraps only.
    ///
    /// Two readers, one field. The **home nest's** row is whether the nest
    /// reads the room: a mint that leaves the nest out is the members
    /// withdrawing the materialization grant, and "whether a nest reads is the
    /// members' standing choice, visible on the roster"
    /// (`encryption-at-rest.md` § Conversation message envelopes) — so every
    /// member, not only the one that minted, can see it, and a later rotation
    /// can keep that choice instead of wrapping to every target it sees. A
    /// **user's** row is whether an owner or admin still owes it a key-in:
    /// acceptance seats a member with a wrap target and no wrap
    /// (`conversation-rooms.md` § Join rules and invites).
    ///
    /// `None` when the room has no generation yet, or for a seating with no
    /// roster entry. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_wrapped: Option<bool>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Read a room's floor roster — the membership read of
/// `conversation-rooms.md` § The floor roster.
///
/// Membership is not public: the read is admitted only to a live member of
/// the room it names. A room id is a 32-byte capability-shaped secret
/// (§ The room — content-derived from the birth record), but the roster is
/// the room's *social* record and one leaked id must not turn into a
/// membership list.
///
/// Pure read — replay-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomListRosterRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// **Also serve the signed policy the room held at this version** — a
    /// superseded one, or the current — as
    /// [`RoomListRosterReply::policy_at_version`]. It is how a member judges a
    /// community room's floor delete record against the policy *of the version
    /// the record names* (`conversation-rooms.md` § Roles and authorization →
    /// *Delete any message — the mechanism* → *Community rooms*).
    ///
    /// Additive, and safe to be ignored: a reply without
    /// `policy_at_version` (the room never held that version on this nest)
    /// leaves the member fails closed (paints no
    /// tombstone) — never a silently wrong answer, because the served bytes
    /// are a *signed* policy that states its own version, which the reader
    /// checks against the number it asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_policy_version: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The room's live floor, and the policy version its roles were read under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomListRosterReply {
    /// Live principals only — a `Removed`-absorbed row is history the roster
    /// read does not render (`conversation-rooms.md` § The home nest → *The
    /// succession axis*).
    pub members: Vec<RoomRosterMemberWire>,
    /// The room's derived class: `end_to_end` / `community` /
    /// `transport_only`.
    pub class: String,
    /// The policy version the roles were read under; `None` on a policy-less room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<u64>,
    /// The **signed policy** [`Self::policy_version`] names — canonical
    /// dag-cbor `fauna_mls::room_policy::SignedRoomPolicy`, exactly the bytes
    /// its author signed.
    ///
    /// The roles on [`Self::members`] are this policy's *projection*, which is
    /// what the nest enforces against; this is the record members **render and
    /// verify** for themselves, and the only way a client can learn a room's
    /// name, join rule and history policy at all. It is also what a policy
    /// change is authored *from*: `set_policy` and `transfer_ownership` both
    /// send a replacement at `version + 1`, so a client without these bytes
    /// would silently reset every field it did not mean to touch.
    ///
    /// Served here rather than behind a `room.get_policy` kind because it is
    /// the same read: this door already answers the version, and adding the
    /// bytes that version names widens nothing — the roster read is admitted
    /// only to a live member, and a member is exactly who renders a policy.
    ///
    /// `None` on a policy-less room (which carries no policy). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ByteBuf>,
    /// The room's **signed labeler set** — canonical dag-cbor
    /// `fauna_mls::room_policy::SignedRoomLabelers`, exactly the bytes its
    /// author signed: which transparent labelers the home nest applies to
    /// this room (`conversation-rooms.md` § The three classes → *What the
    /// home nest does with its read*, purpose 2).
    ///
    /// Served for the reason [`Self::policy`] is: it is what members verify
    /// and render (the consent surface must say what reads the room), and
    /// what the next `room.set_labelers` is authored from, at `version + 1`.
    /// `None` when the room never named a labeler. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labelers: Option<ByteBuf>,
    /// The signed policy the room held at the version the request's
    /// [`RoomListRosterRequest::at_policy_version`] named — canonical dag-cbor
    /// `SignedRoomPolicy`, exactly as its author signed it, still unverified.
    /// `None` when the request named none, or when the room never held that
    /// version on this nest. A field of
    /// its own rather than a swap of [`Self::policy`], which stays the policy
    /// [`Self::policy_version`] and the roles above are read under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_at_version: Option<ByteBuf>,
    /// The room's **birth salt** (32 bytes) — served beside
    /// [`Self::policy_at_version`], and only when the request named a version.
    /// It is the other half of the birth record the room id commits to
    /// (`fauna_mls::room_policy::derive_room_id`), which is what lets a member
    /// prove version 1 is the founder's and so anchor every later version it
    /// fetches (`conversation-rooms.md` § Roles and authorization → *Delete any
    /// message — the mechanism* → *Members verify what they paint*). Nothing a
    /// member may not hold: it names the room already, and the salt only ever
    /// kept the id unguessable to those who did not.
    ///
    /// `None` on a room no birth ceremony founded (a policy-less room) — the member then anchors nothing and fails closed.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_salt: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.list_roster_remote ────────────────────────

/// The same read as [`RoomListRosterRequest`], for a room homed on **another
/// nest**: the caller's own nest originates
/// `fauna.federation.conversation.roster.fetch` to `nest_url` and hands back
/// the room home's answer, unchanged and unstored (`conversation-rooms.md`
/// § The home nest — "a member on a foreign nest reaches the room only through
/// their own home nest").
///
/// Without it a foreign-homed member has **no** roster read at all: a room's
/// floor roster lives on its home nest alone, so the member's own nest holds
/// no room record and answers `permission_denied`, and every co-member this
/// device has not met renders as an elided actor id — including the members
/// the id→handle announce names for everyone on the room's home nest
/// (`../architecture/federation.md` § Cross-nest shared folders + channel
/// append, the id→handle bullet).
///
/// A **distinct kind** rather than an additive `nest_url` on
/// `room.list_roster`, for the `channel.actors_remote` reason
/// (`../behavior/direct-messages.md` § step 3b): an old own-nest that ignored
/// an additive field would answer from its *own* room plane, where the caller
/// is not a member — a clean `permission_denied` the seam cannot tell apart
/// from "you were removed from this room", so the members would stay silently
/// elided with no signal that a newer nest could have named them. An unknown
/// kind fails loud.
///
/// The reply is [`RoomListRosterReply`], the same shape the same-nest door
/// returns, because it is literally the room home's reply forwarded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomListRosterRemoteRequest {
    /// Hex-encoded room id (32 bytes). A room id *is* its channel id.
    pub room_id: String,
    /// The room's home nest — where the canonical log, the floor roster and
    /// the members' handles live.
    pub nest_url: String,
    /// [`RoomListRosterRequest::at_policy_version`], forwarded to the room's
    /// home unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_policy_version: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.roster_report_remote ──────────────────────

/// The same report as [`RoomRosterReportRequest`], for a room homed on
/// **another nest**: the reporter's own nest originates
/// `fauna.federation.conversation.roster.report` to `nest_url` and hands back
/// the room home's ack, unchanged and unstored (`conversation-rooms.md` § The
/// home nest — "a member on a foreign nest reaches the room only through their
/// own home nest"). The write twin of [`RoomListRosterRemoteRequest`].
///
/// Without it a foreign-homed member's membership commits never reach the
/// floor they are owed to: § The floor roster has the committing device report
/// to the room's **home** nest, and the member's own nest holds no room record
/// for a room homed elsewhere.
///
/// A **distinct kind** rather than an additive `nest_url` on
/// `room.roster_report`, for the `channel.actors_remote` reason
/// (`../architecture/federation.md` § Federation residue surface): an old
/// own-nest that ignored an additive field would run its same-nest door on a
/// room it does not home — and, the reporter sitting on its routing roster
/// after the relayed Welcome, would bootstrap a **stray floor** there as a
/// clean success while the real home stayed stale: a silently wrong answer
/// about membership. An unknown kind fails loud, and the seam tallies the
/// report undelivered.
///
/// The reply is [`RoomRosterReportReply`], the room home's ack forwarded.
/// `forbid_replay=false` on the same grounds as the same-nest report: a
/// wholesale replace is idempotent on both hops.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRosterReportRemoteRequest {
    /// Hex-encoded room id (32 bytes). A room id *is* its channel id.
    pub room_id: String,
    /// The room's home nest — where the floor roster this report replaces
    /// lives.
    pub nest_url: String,
    /// The roster as the committing device now holds it — the
    /// [`RoomRosterReportRequest::members`] contract verbatim.
    pub members: Vec<RoomRosterEntryWire>,
    /// The policy version the roles were read under; `None` on a
    /// policy-less room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<u64>,
    /// The room home's log position of the commit this roster follows — the
    /// [`RoomRosterReportRequest::commit_seq`] contract verbatim. The member's
    /// commit rode the same relay to the home's log, so the position it got
    /// back is the home's own. The relaying nest forwards it verbatim; absent
    /// (a report that follows no commit), the home applies it unordered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.invite / accept_invite ────────────────────

/// Invite a principal into a room — "the inviter's signed act"
/// (`conversation-rooms.md` § Join rules and invites).
///
/// **An invite does not seat a member; acceptance does.** The nest records
/// the invitation and the invitee's own `room.accept_invite` writes the
/// roster row, so a room's floor never names someone who has not agreed to
/// be there. (The retired group plane's `group.invite` added the member
/// outright; the room plane deliberately does not.)
///
/// Who may invite is the room's join rule (§ Join rules and invites):
/// `invite` — owner and admins; `member-invite` — any member; `request` —
/// owner and admins invite, and the knock door strangers use is a later
/// slice. The invitee's own **reach policy** gates the invitation exactly as
/// it gates a group Welcome: an invite is initiation, and initiation is what
/// the recipient's inbox mode mediates ([`direct-messages.md`] § Reach
/// policy).
///
/// `forbid_replay=true` — an invite is externally visible (it notifies the
/// invitee), so a recovered connection must not auto-retry it. Re-inviting
/// the same principal is nonetheless idempotent server-side: the pending
/// invitation is refreshed, never duplicated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomInviteRequest {
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomInvite`. It
    /// names the room, the invitee, the role and the policy version the
    /// inviter read the join rule under; the nest binds its signer to the
    /// authenticated caller.
    #[serde(with = "serde_bytes")]
    pub invite: Vec<u8>,
    /// The invitee's home nest, as the inviter knows it — a base URL the
    /// room's home can dial (the glue derives it from the chip's handle
    /// domain exactly as it derives a Welcome relay's `nest_url`). Empty
    /// means this nest. Non-empty, the home nest delivers the invitation to
    /// that nest over `fauna.federation.conversation.room.invite` instead of
    /// its own inbox plane, and stores the URL on the roster row when the
    /// invitation is accepted, so the room knows which nest a foreign member
    /// is reached through (`conversation-rooms.md` § Join rules and invites
    /// → *A cross-nest invitation*).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invitee_node: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the invitation is recorded and pending the invitee's acceptance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomInviteReply {
    /// The role the invitee will hold once they accept.
    pub role: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Issue an invitation into a room homed on **another nest** — the relayed
/// twin of [`RoomInviteRequest`], for an inviter homed elsewhere: this nest
/// originates `fauna.federation.conversation.room.invite_issue` to `nest_url`,
/// the room's home, which binds the signed act to the requesting actor this
/// nest authenticated, gates it on the inviter's foreign-member binding and
/// then runs the same-nest invite body — the join-rule judgement, the
/// already-a-member refusal and the delivery, to the invitee's own nest or its
/// own inbox plane (`conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*, the foreign-inviter leg). The reply is
/// [`RoomInviteReply`], the home's ack forwarded.
///
/// A **distinct kind** rather than an additive `nest_url` on
/// [`RoomInviteRequest`], for the `channel.actors_remote` reason
/// (`../architecture/federation.md` § Federation residue surface): an old
/// own-nest that ignored an additive field would run its same-nest door on a
/// room it does not home and answer "no such room" — an `invalid_params` the
/// seam cannot tell apart from a mistyped room id. An unknown kind fails loud.
///
/// `invitee_node` keeps [`RoomInviteRequest`]'s meaning from the INVITER's
/// side — the invitee's home nest as the inviter knows it, **empty for the
/// inviter's own nest** (the relaying one). The room's home resolves an empty
/// node to the relaying nest's verified identity and a node naming the home
/// itself to a same-nest delivery, so the three deliveries — an invitee on the
/// inviter's nest, on the room's home, or on a third nest — are one door's
/// three arms.
///
/// `forbid_replay=true`, as the same-nest door is: the home's issue door
/// records a row and delivers a knock per call (it is not idempotent), so a
/// recovered connection must not auto-retry one — the inviter re-issues, and
/// the pending invitation is refreshed rather than duplicated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomInviteRemoteRequest {
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomInvite`, as on
    /// the same-nest door. It names the room; the room's home binds its
    /// signer to the actor this nest relays for.
    #[serde(with = "serde_bytes")]
    pub invite: Vec<u8>,
    /// The room's home nest — the recorded `ChannelHome` of the room.
    pub nest_url: String,
    /// The invitee's home nest as the inviter knows it; empty for the
    /// inviter's own nest.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invitee_node: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Accept an invitation into a room — the act that **seats** the member
/// (`conversation-rooms.md` § Join rules and invites: "acceptance seats the
/// member ... a community room's home nest writes the roster row on
/// acceptance").
///
/// Only the invitee may accept their own invitation, and only while it is
/// pending: an accept that arrives after the inviter's room removed the
/// invitee is refused rather than re-seating them.
///
/// `forbid_replay=true` — accepting changes the room's membership and the
/// reply is not the whole effect; a replayed accept must not re-seat a
/// principal the room has since removed. (The handler refuses it anyway, on
/// the pending-invitation read; the flag keeps a recovered connection from
/// producing the attempt at all.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomAcceptInviteRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// The accepting member's **group-reception public key** — its wrap
    /// target. Acceptance is where it belongs: the roster row and the wrap
    /// target are one fact, so a member cannot be seated without the room
    /// knowing how to key it, and re-admission gets a fresh entry (and so a
    /// fresh wrap slot) by construction. Additive and optional.
    #[serde(
        default,
        with = "serde_bytes",
        skip_serializing_if = "Vec::is_empty",
        rename = "reception_pubkey"
    )]
    pub reception_pubkey: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Accept an invitation into a room homed on **another nest** — the relayed
/// twin of [`RoomAcceptInviteRequest`]: this nest originates
/// `fauna.federation.conversation.room.accept` to `nest_url`, the room's home,
/// which runs the same-nest accept body behind a gate of its own — the
/// invitation it recorded names the relaying nest as the invitee's home — and
/// then binds the new seat as a foreign member so every relayed room door
/// opens (`conversation-rooms.md` § Join rules and invites → *A cross-nest
/// invitation*). The reply is [`RoomAcceptInviteReply`], the home's ack
/// forwarded.
///
/// A **distinct kind** rather than an additive `nest_url` on
/// [`RoomAcceptInviteRequest`], for the `channel.actors_remote` reason
/// (`../architecture/federation.md` § Federation residue surface): an old
/// own-nest that ignored an additive field would run its same-nest door on a
/// room it does not home and answer "no invitation to this room is pending"
/// — a `permission_denied` the seam cannot tell from a genuine lapse, so the
/// invitee would believe the offer had been withdrawn while it still stood.
/// An unknown kind fails loud.
///
/// **`forbid_replay=false`, where the same-nest door is `true`** — the
/// `leave_remote` divergence for the same reason: a relayed call carries a
/// §4.D auto re-send the same-nest door does not have, so the home's federated
/// door is idempotent (a member the recorded invitation already seated from
/// this nest is answered with its role and its binding re-asserted) and a
/// re-sent frame reports the seating that landed rather than a failure that
/// did not happen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomAcceptInviteRemoteRequest {
    /// Hex-encoded room id (32 bytes). A room id *is* its channel id.
    pub room_id: String,
    /// The room's home nest — the `room_node` the invitation arrived with.
    pub nest_url: String,
    /// The accepting member's wrap target, as on the same-nest door.
    #[serde(
        default,
        with = "serde_bytes",
        skip_serializing_if = "Vec::is_empty",
        rename = "reception_pubkey"
    )]
    pub reception_pubkey: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the accepting principal is now on the room's floor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomAcceptInviteReply {
    /// The role now held.
    pub role: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.list_invites / revoke_invite ──────────────

/// List a community room's **pending** invitations
/// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
/// are visible to whoever may withdraw them*).
///
/// One predicate governs this door and [`RoomRevokeInviteRequest`]'s: the
/// owner and admins are served every pending invitation of the room, any
/// other seated member the ones it issued, and a caller off the floor is
/// refused. Accepted invitations are never served — that history is the
/// roster's `invited_by`.
///
/// `forbid_replay=false` — a pure read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomListInvitesRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One pending invitation, as [`RoomListInvitesReply`] serves it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomPendingInviteWire {
    /// Hex-encoded actor id (32 bytes) of the invitee — what
    /// [`RoomRevokeInviteRequest::invitee`] names.
    pub invitee: String,
    /// The invitee's handle, when this nest knows it — joined nest-side, the
    /// [`RoomRosterMemberWire::handle`] contract. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitee_handle: Option<String>,
    /// Hex-encoded actor id (32 bytes) of the identity that issued it. Never
    /// re-pointed by a succession: the inviter is attribution.
    pub inviter: String,
    /// The inviter's handle, when this nest knows it. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter_handle: Option<String>,
    /// The domain both handles are local parts of — this nest's handle
    /// domain. `None` when neither handle is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// `admin` or `member` — the rank on offer; never `owner`.
    pub role: String,
    /// When the invitation was (last) issued, epoch millis.
    pub invited_at: i64,
    /// Whether the accept door would still seat this invitee — its own
    /// judgement, run as the list is served. `false` is an invitation that
    /// has lapsed in waiting: its inviter could not issue it today (demoted,
    /// departed, succeeded), or the policy no longer names an admin invitee.
    /// It seats nobody and will be consumed the moment it is tried; listing it
    /// lets an owner clear it instead.
    pub still_acceptable: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The pending invitations the caller may withdraw, oldest first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomListInvitesReply {
    pub invites: Vec<RoomPendingInviteWire>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Withdraw a pending invitation — the row, its standing inbox envelope and
/// the envelope's quota charge, as one act. The invitee is told nothing: the
/// invitation simply stops standing, the way a decline tells the room
/// nothing.
///
/// Who may: [`RoomListInvitesRequest`]'s predicate — whoever is served an
/// invitation may withdraw it.
///
/// `forbid_replay=true` — it consumes another account's inbox envelope. A
/// repeat is harmless all the same: withdrawing an invitation that is not
/// pending is answered `revoked: false`, not refused.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRevokeInviteRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Hex-encoded actor id (32 bytes) of the invitee whose pending
    /// invitation is withdrawn.
    pub invitee: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRevokeInviteReply {
    /// `true` when a pending invitation was consumed; `false` when none was
    /// pending (already accepted, declined-and-lapsed, or never issued).
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.remove / leave ────────────────────────────

/// Remove a member from a room (`conversation-rooms.md` § Roles and
/// authorization — owner and admins remove; a plain member does not).
///
/// The rule is the end-to-end class's, applied at the floor rather than in
/// a commit verdict, so the two classes enforce one table: an owner or admin
/// may remove any member whose role is not `owner`, and **the owner's
/// membership is not removable by anyone until ownership is transferred** —
/// an owner-less roster would strand invite and remove forever.
///
/// `forbid_replay=true` — a removal is externally visible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRemoveRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Hex-encoded principal id (32 bytes) to remove.
    pub principal: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomRemoveReply {
    /// How many principals the floor roster now holds live.
    pub members: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Leave a room — remove yourself (`conversation-rooms.md` § Roles and
/// authorization: leave is an admin's and a member's, and the **owner must
/// transfer ownership first**).
///
/// A separate door from [`RoomRemoveRequest`] rather than a self-addressed
/// remove, because the authorization is different in kind: removing someone
/// else is a rank the roles table grants, leaving is a right every member
/// but the owner has. Folding them would make a plain member's departure
/// look like a rank check that happens to pass.
///
/// `forbid_replay=true` — a departure is externally visible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomLeaveRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomLeaveReply {
    /// How many principals the floor roster now holds live.
    pub members: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.leave_remote ──────────────────────────────

/// The same departure as [`RoomLeaveRequest`], for a room homed on **another
/// nest**: the leaver's own nest originates
/// `fauna.federation.conversation.room.leave` to `nest_url` and hands back the
/// room home's ack, unchanged and unstored (`conversation-rooms.md` § The home
/// nest — "a member on a foreign nest reaches the room only through their own
/// home nest"). The self-scoped twin of [`RoomRosterReportRemoteRequest`].
///
/// Without it a member homed elsewhere has **no leave at all**: § Roles and
/// authorization grants *leave (remove self)* to every role but owner with no
/// homing carve-out, yet [`RoomLeaveRequest`] is a same-nest door and a
/// leaver's own nest holds no room record for a room homed elsewhere, so it
/// answers "no such room". What a departing foreign member could reach instead
/// was the generic `fauna.federation.channel.leave`, which drops the relay
/// binding and never touches the floor — so the seat outlived the departure,
/// the room's floor never converged, every later generation mint went on
/// wrapping to the departed seat's reception key (the roster-coverage gate
/// *requires* a live floor entry carrying one to be wrapped, so the mints do
/// not merely leak, they cannot be made without it), and the ghost seat blocked
/// re-admission outright, `room.invite` refusing a principal that is already a
/// member.
///
/// A **distinct kind** rather than an additive `nest_url` on
/// [`RoomLeaveRequest`], for the `channel.actors_remote` reason
/// (`../architecture/federation.md` § Federation residue surface): an old
/// own-nest that ignored an additive field would run its same-nest door on a
/// room it does not home and answer "no such room" — an `invalid_params` the
/// seam cannot tell apart from a mistyped room id, leaving the leaver believing
/// a departure failed for a reason it could fix while the real home kept them
/// seated. An unknown kind fails loud.
///
/// The reply is [`RoomLeaveReply`], the room home's ack forwarded.
/// **`forbid_replay=false`, where the same-nest door is `true`** — the one
/// place the twin deliberately diverges from it, and the relay is the reason: a
/// relayed call carries a §4.D auto re-send the same-nest door does not have,
/// so the home's door is idempotent (a caller already off the floor is a
/// converged success, not a refusal) and a re-sent frame reports the departure
/// that landed rather than a failure that did not happen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomLeaveRemoteRequest {
    /// Hex-encoded room id (32 bytes). A room id *is* its channel id.
    pub room_id: String,
    /// The room's home nest — where the floor this departure leaves lives.
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.set_policy ────────────────────────────────

/// Replace a room's signed policy — its name, join rule, history policy and
/// admin set (`conversation-rooms.md` § Roles and authorization).
///
/// **The nest stores it and cannot author it** (rule 6). What the nest
/// enforces is that the signature covers the change: an alteration to the
/// **admin set** needs the *owner's* signature, and the rest — name, join
/// rule, history policy — takes an owner's or an admin's. The version is a
/// strict ratchet, exactly `stored + 1`, so a replayed older policy can
/// never be installed over a newer one.
///
/// The **owner field is not settable here**: ownership moves through
/// [`RoomTransferOwnershipRequest`], which is its own operation in § Roles
/// and has to move the roster row and the room's home with it.
///
/// Storing a policy **reconciles the floor roster's roles to it**, because
/// the signed policy is the authority for who is an admin and the roster's
/// `role` column is its projection — the column every nest-side gate reads.
/// A member the new admin set names becomes `admin` on the floor; one it
/// drops becomes `member`. Without that the nest would enforce a rank the
/// policy members verify does not grant.
///
/// `forbid_replay=true` — a policy change is externally visible (every
/// member renders it). A replay is refused by the version ratchet anyway;
/// the flag keeps a recovered connection from producing the attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetPolicyRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomPolicy` at
    /// version `stored + 1`.
    #[serde(with = "serde_bytes")]
    pub policy: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the version now stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetPolicyReply {
    pub policy_version: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.transfer_ownership ────────────────────────

/// Transfer a room's ownership — the owner's own act
/// (`conversation-rooms.md` § Roles and authorization; § The home nest →
/// *Transfer by succession*).
///
/// Its own door rather than a field of `set_policy` because it is its own
/// operation in the roles table and does more than store bytes: it seats the
/// incoming owner and, like `set_policy`, reconciles every other live user
/// member's roster role to the new policy's admin set — the outgoing owner
/// included, who becomes an ordinary member or an admin if the new policy
/// names them — and, in target state, re-homes the room when the new owner
/// is homed on another nest.
///
/// The new policy is signed by the **outgoing** owner: the signer's role in
/// the *previous* version is what decides whether it may have changed the
/// owner field, and at signing time that is still the owner.
///
/// An owner-less room is unrepresentable — the roles table makes the owner's
/// membership unremovable until ownership is transferred — so the new owner
/// must already be a live **user** member of the room. Transferring to the
/// home nest, or to somebody who has not joined, is refused.
///
/// `forbid_replay=true`; the version ratchet refuses a replay regardless.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomTransferOwnershipRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Canonical dag-cbor `SignedRoomPolicy` at version `stored + 1`, whose
    /// `owner` names the incoming owner and whose signer is the outgoing
    /// one.
    #[serde(with = "serde_bytes")]
    pub policy: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the room's new owner and policy version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomTransferOwnershipReply {
    /// Hex-encoded principal id (32 bytes) of the new owner.
    pub owner: String,
    pub policy_version: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.publish_generation ────────────────────────

/// Publish one **room generation mint** — the recipient-set scheme's keying
/// act, applied to a community room (`conversation-rooms.md` § The three
/// classes → *Community*; mechanics `account-data-taxonomy.md` § The
/// recipient-set scheme).
///
/// The nest **never mints** (§ Don't do these): key authority is the room's
/// owner and admins, and this kind is how their act reaches the floor. The
/// nest's job is admission — signer role, parent, and **roster coverage** —
/// plus storing the wraps so every member (and the home nest itself) can
/// fetch its own.
///
/// The scheme's authority seam is filled here by the **floor roster**: the
/// mint's `minter` must be the authenticated caller, and that caller must
/// hold `owner` or `admin` on the floor. That is the room plane's answer to
/// "membership authority in v1 is the initiator's device fleet" — the same
/// deliberate seam T19 fills with the box's admin set — so the mint carries
/// no `DeviceAuthorization` chain and none is resolved.
///
/// `forbid_replay=true` — a mint changes which key the room seals under and
/// can revoke the home nest's read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomPublishGenerationRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Canonical dag-cbor `fauna_core::group_generation::GroupGenerationMintRecord`
    /// as `fauna_mls::wrapped_blob::group_generation_wraps::build_group_mint`
    /// assembles it: the mint core (parents, member entry ids, minter,
    /// key commitment, stamp), the minter's signature, and one X-Wing wrap
    /// per roster entry.
    #[serde(with = "serde_bytes")]
    pub mint: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the generation is the room's tip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomPublishGenerationReply {
    /// Hex-encoded generation id (32 bytes), content-derived from the mint
    /// core, so a caller can check the nest resolved the same one it built.
    pub generation_id: String,
    /// How many live floor principals the mint wraps to. Coverage is the
    /// admissibility rule, so this is the number the caller can compare
    /// against the roster it read.
    pub covered: u64,
    /// True when this mint dropped the home nest's wrap: the materialization
    /// grant's revoke (`principles.md` § The user always controls their data).
    /// The nest deleted its derived views for this room in the same
    /// transaction, and reads nothing sealed under this generation or any
    /// later one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nest_read_revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.generations ───────────────────────────────

/// Read back the room's generations, each with **the caller's own wrap** —
/// how a member gets its keys, and how a joiner gets the retained bundle the
/// history policy allows (`conversation-rooms.md` § History for joiners:
/// "the retained generation bundle wrapped to the newcomer at admission").
///
/// A caller sees only wraps sealed to its own roster entry; the wraps of
/// other members are never served, so this kind cannot be used to enumerate
/// the room's key material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomGenerationsRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One generation as its holder sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomGenerationWire {
    /// Hex-encoded generation id (32 bytes).
    pub generation_id: String,
    /// The mint's key commitment — what the caller verifies the unwrapped
    /// key against, so a substituted wrap is refused at the reader
    /// (`group_generation_wraps::open_group_generation_key_as_entry`).
    #[serde(with = "serde_bytes")]
    pub key_commitment: Vec<u8>,
    /// The X-Wing wrap sealed to **this caller's** roster entry.
    #[serde(with = "serde_bytes")]
    pub wrap: Vec<u8>,
    /// Hex-encoded roster entry id (32 bytes) the wrap is bound to — half of
    /// the open's AAD, so the caller need not re-derive it.
    pub entry_id: String,
    pub minted_at_ms: i64,
    /// True for the generation new content seals under. Exactly one is the
    /// tip; the rest are retained for content already sealed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_tip: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomGenerationsReply {
    /// Oldest first, tip last.
    pub generations: Vec<RoomGenerationWire>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.generations_remote ────────────────────────

/// The same read as [`RoomGenerationsRequest`], for a room homed on **another
/// nest**: the caller's own nest originates
/// `fauna.federation.conversation.generations.fetch` to `nest_url` and hands
/// back the room home's answer, unchanged and unstored
/// (`conversation-rooms.md` § The home nest — "a member on a foreign nest
/// reaches the room only through their own home nest").
///
/// A **distinct kind** rather than an additive `nest_url` on
/// `room.generations`, for the `channel.actors_remote` reason
/// (`../behavior/direct-messages.md` § step 3b): an old own-nest that ignored
/// an additive field would answer from its *own* empty room plane as a clean
/// success, and the caller would read "this room has no generations" — a
/// silent wrong answer about key material. An unknown kind fails loud.
///
/// The reply is [`RoomGenerationsReply`], the same shape the same-nest door
/// returns, because it is literally the room home's reply forwarded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomGenerationsRemoteRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// The room's home nest — where the canonical log, the floor roster and
    /// the wraps live.
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.backfill_generations ──────────────────────

/// Wrap the room's **retained generation bundle** to one newly-seated member
/// — the recipient-set scheme's archival backfill, applied to a community
/// room (`conversation-rooms.md` § History for joiners, the community arm).
///
/// The scheme's rule is that an **add never mints**
/// (`account-data-taxonomy.md` § The recipient-set scheme → *Mint triggers*):
/// a new member is covered by wrapping generations that already exist to its
/// roster entry, not by rotating the room. So this is deliberately not a
/// second mint door — it carries only `GroupTopupRecord` wraps an
/// owner-or-admin device built with
/// `fauna_mls::wrapped_blob::group_generation_wraps::build_group_topup_wrap`,
/// and it can never change which generation the room seals under.
///
/// **What the history policy bounds.** Under `HistoryPolicy::Full` the whole
/// retained bundle is authorized. Under `HistoryPolicy::None` — every room's
/// default — only the **tip** is: a newcomer "sees the room from their
/// admission; nothing before", and the nest refuses to store a wrap for any
/// earlier generation (§ History for joiners: "in a community room the nest
/// refuses to store one"). The tip stays authorized under `none` because a
/// member seated after the last mint holds no wrap for the tip either, and
/// refusing it would leave a newcomer unable to read the room *at all* — that
/// is the admission/mint race the scheme's member top-up exists to heal, not
/// history.
///
/// `forbid_replay=true` — the act hands out key material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomBackfillGenerationsRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Hex-encoded actor id (32 bytes) of the member being covered. Its
    /// **current** roster entry is what every wrap must name: a re-admitted
    /// member returns on a fresh entry, and a backfill to the retired one
    /// would undo exactly the guarantee that fresh entry buys.
    pub target_actor_id: String,
    /// Canonical dag-cbor `fauna_core::group_generation::GroupTopupRecord`
    /// values, one per generation being covered. Each is verified at its own
    /// cell (`GroupTopupRecord::verifies_at`) before anything is stored, and
    /// the batch is all-or-nothing — a refused batch leaves no partial
    /// bundle behind.
    #[serde(with = "fauna_core::byte_array::vec_of_bufs")]
    pub wraps: Vec<Vec<u8>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — how much of the bundle the member now holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomBackfillGenerationsReply {
    /// How many wraps were newly stored. Authorization is still all-or-
    /// nothing for the batch — every wrap must be authorized or the whole
    /// batch is refused — but a wrap already stored for its `(generation,
    /// entry)` is skipped rather than written over, so `stored` can be less
    /// than the request's length even when nothing was refused.
    pub stored: u64,
    /// How many generations the room retains in total, so a caller can see
    /// whether the policy withheld any of them without a second read.
    pub retained: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.set_reception_key ─────────────────────────

/// Supply **this caller's own wrap target** for the seat it already holds on
/// a community room's floor (`community-rooms.md` § Implementation status
/// today, *A seat gains or rotates its wrap target*).
///
/// Every other writer of a seat's reception key is a *seating* act — the
/// birth ceremony and `room.accept_invite` carry the key with the row. One
/// seat exists without one: the successor an identity succession seats (the
/// ceremony deliberately writes no key, since the predecessor's derives from
/// the material the ceremony retires). It governs the room and can open
/// nothing minted after it was seated, because coverage skips a keyless seat
/// and a backfill refuses one. This door is how such a seat becomes keyable — and how a
/// keyed seat **rotates**: the recipient-set scheme's wrap target is the
/// member's *current* reception key, rotated on the member's own fleet
/// events (`account-data-taxonomy.md` § The recipient-set scheme → *Severance,
/// per axis*), so a key already set is replaced, never refused.
///
/// **The seat keeps its roster entry across a rotation.** A wrap is bound to
/// `(generation, entry)` and sealed to the key of that moment; the account
/// retains every reception key it ever held, so the wraps already stored at
/// the entry stay openable and nothing is re-sealed. A fresh entry is the
/// scheme's *re-admission* rule — a `Removed`-then-`Enrolled` transition —
/// and a rotation is neither. A seat with **no** entry at all (one seated
/// before the sealing plane) is given one here, derived from this moment.
///
/// The door binds a key and mints nothing (the nest never mints). What the
/// seat still owes is answered in the reply: the room's current generation,
/// when this seat holds no wrap for it — an owner or admin's device covers a
/// plain member by the ordinary top-up, and a seat that itself ranks owner or
/// admin mints a fresh generation parented on that tip, which is also the
/// severance mint a succession's `Removed` row calls for.
///
/// Only a **user** principal's own seat: the home nest's read is a grant the
/// members make at a mint, never a key it sets for itself, and a bridge is
/// seated by its own enrollment.
///
/// `forbid_replay=true` — a replayed older request would roll a rotated seat
/// back to a key its owner retired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetReceptionKeyRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// The caller's group-reception public key — its X-Wing wrap target, in
    /// the wire form every roster row carries. Never empty.
    #[serde(with = "serde_bytes")]
    pub reception_pubkey: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the seat's wrap target is bound, and what it still owes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetReceptionKeyReply {
    /// Hex-encoded roster entry id (32 bytes) the seat holds — unchanged by a
    /// rotation, minted here for a seat that had none.
    pub entry_id: String,
    /// True when the seat already held a *different* key and this call
    /// replaced it — a rotation. False for a first key, and for a call that
    /// named the key already bound (a no-op the door answers rather than
    /// refuses).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rotated: bool,
    /// Hex-encoded id of the room's **current** generation when this seat
    /// holds no wrap for it — the generation an owner or admin's top-up
    /// covers, or a fresh mint by this seat parents on. Absent when the seat
    /// is covered, and when the room has no generation yet.
    ///
    /// Carried here because `room.generations` serves a caller only the
    /// generations it holds a wrap for, so a seat that holds none cannot
    /// learn the tip it must name as a mint's parent from that read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncovered_tip: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.conversations.room.search ────────────────────────────────────

/// Search a community room's log through the derived view its home nest
/// built from the sealed bytes (`conversation-rooms.md` § The three classes →
/// *What the home nest does with its read* — the community's search index).
///
/// The reason this door exists at all is the class's *unboundedness*: a
/// member's own device holds whatever slice of the room it has fetched, and
/// for a community that may be a vanishing fraction of the log. Searching a
/// room is therefore the one search a member cannot run for itself, which is
/// exactly the "purpose that needs a reader the members are not" the read
/// position was granted for.
///
/// A member of any rank may ask — reading the room is what every member
/// does — and only a live floor member: the room's floor is the authority
/// for a community room (§ The floor roster).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSearchRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// The FTS query, in the same dialect `fauna.search.query` takes.
    pub query: String,
    /// How many hits to return, clamped nest-side to
    /// [`ROOM_SEARCH_MAX_LIMIT`]; absent means [`ROOM_SEARCH_DEFAULT_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Also answer with hits on the room's **room-restricted posts** — posts
    /// addressed to the room that this nest stores and indexed at reception
    /// (`conversation-rooms.md` § The three classes → *What the home nest does
    /// with its read*, purpose 3; `ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*, ruling 7).
    ///
    /// Additive (2026-09-10), and **opt-in on purpose**: a post hit names no
    /// log position, and a caller built before post hits existed decodes a
    /// [`RoomSearchHit`] whose `seq` it requires — so it must never be sent
    /// one. Unset, the reply is exactly the message-only answer it always was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_posts: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What a [`RoomSearchHit`] points at — the additive kind discriminator
/// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
/// ruling 7).
///
/// Serialized as its snake_case name. [`Self::Message`] is the default and is
/// **omitted on the wire**, so a message hit is byte-identical to every hit a
/// nest sent before the discriminator existed. [`Self::Unknown`] is the rule-3
/// fallthrough — never written, decoded from any discriminator a newer nest
/// introduced, and skipped by the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomSearchHitKind {
    /// A message in the room's log — [`RoomSearchHit::seq`] names it.
    #[default]
    Message,
    /// A room-restricted post addressed to the room —
    /// [`RoomSearchHit::post_id`] names it.
    Post,
    /// A kind this build does not recognize. Never serialized.
    #[serde(other)]
    Unknown,
}

impl RoomSearchHitKind {
    /// The `skip_serializing_if` predicate that keeps a message hit
    /// byte-identical to a pre-discriminator one.
    pub fn is_message(&self) -> bool {
        matches!(self, Self::Message)
    }
}

/// One hit — **a reference to a message, never its text.**
///
/// ⚠ The absence of a snippet is the load-bearing part of this kind, not an
/// economy. The home nest's index is built from the *tip* generation, while a
/// member holds a wrap only for the generations minted while it sat on the
/// floor: a member seated after the current mint holds no wrap for the tip
/// and, by the scheme's own rule, cannot read what is said until the next one
/// covers it (`conversation-rooms.md` § Implementation status today — the
/// admission/mint race the member top-up heals). A snippet would hand that
/// member the very plaintext the sealing plane withholds, with the nest's
/// read as the leak — so the door answers *where*, and the member opens the
/// sealed bytes through `fauna.conversations.channel.fetch` with the wrap it
/// holds.
///
/// ⚠ And a hit is only ever sent for a position the receiver **could** open: a position is an answer too, so hits a member holds no wrap for
/// would make the door a chosen-plaintext oracle over exactly the history its
/// room's `history_policy` withholds. Every page the nest sends is bounded by
/// the receiver's own wraps, which is why a page shorter than `limit` never
/// means the corpus is exhausted.
///
/// A **post** hit (`kind: post`, only ever sent to a caller that set
/// [`RoomSearchRequest::include_posts`]) is the same rule applied to a post:
/// its id and a rank, never its text, and only for a post whose generation the
/// member holds — it fetches the post through `fauna.posts.get` and opens it
/// with the key it holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSearchHit {
    /// The channel sequence number of the matching message — what
    /// `fauna.conversations.channel.fetch` reads it back by. Always present on
    /// a message hit; absent on a post hit, because a room-restricted post is
    /// its author's post and never enters the room's log. (Optional since
    /// 2026-09-10; a message hit's bytes are unchanged.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// Relevance, best first: BM25 negated and scaled to fixed-point
    /// micro-units, the unit and convention `search::SearchResult::rank`
    /// already uses (the dag-cbor wire forbids floats). Message and post hits
    /// rank on one scale — they come from one corpus.
    pub rank: i64,
    /// What the hit points at; absent on the wire for a message.
    #[serde(default, skip_serializing_if = "RoomSearchHitKind::is_message")]
    pub kind: RoomSearchHitKind,
    /// A post hit's post id, hex — what `fauna.posts.get` reads it back by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Best first. An **empty list is a real answer** and covers three states a
/// caller cannot tell apart, deliberately: nothing matched, the room was
/// never keyed to this nest, and the members rotated the nest's wrap out and
/// the revoke deleted every view it had built. The last is not an error —
/// withdrawing the materialization grant is a member's act, and a room whose
/// search went quiet because of it is behaving exactly as designed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSearchReply {
    pub hits: Vec<RoomSearchHit>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Page size when the caller names none.
pub const ROOM_SEARCH_DEFAULT_LIMIT: u32 = 20;

/// The ceiling the nest clamps to. Owned here rather than nest-side so a
/// client can page against the same number.
pub const ROOM_SEARCH_MAX_LIMIT: u32 = 100;

// ── fauna.conversations.room.set_labelers ──────────────────────────────

/// Name the transparent labelers a **community** room's home nest applies to
/// the room's messages (`conversation-rooms.md` § The three classes → *What
/// the home nest does with its read*, purpose 2) — the room-level twin of a
/// user's own labeler subscription.
///
/// The set is a signed record of its own
/// (`fauna_mls::room_policy::SignedRoomLabelers`), not a field of the room
/// policy, so a policy change can never reset it. Rule 6 governs it as "the rest" of the policy: the
/// **owner or an admin** signs it, the version is a strict ratchet (exactly
/// `stored + 1`, the first set at 1), and the nest stores it and cannot
/// author it.
///
/// The nest additionally refuses any id it would not run: every named
/// labeler must be one this nest's registry holds as a `wasm` or
/// `text-model` artifact. A `list` is keyed by post id and has nothing to say
/// about a message; an id the registry does not hold names nothing
/// transparent — a user's own (tier-1) model is sealed under its owner's key
/// and is never published there. An **empty** set is admitted: it is how an
/// admin stops the nest labelling the room.
///
/// A new set applies to messages sent after it lands; what the nest already
/// labelled stays until the members rotate the nest out, which deletes every
/// derived view of the room at once.
///
/// `forbid_replay=true` — the ratchet refuses a replay on its own merits; the
/// flag is the honest declaration, as for `set_policy`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetLabelersRequest {
    /// Hex-encoded room id (32 bytes).
    pub room_id: String,
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomLabelers` at
    /// version `stored + 1`.
    #[serde(with = "serde_bytes")]
    pub labelers: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Ack — the labeler-set version now stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomSetLabelersReply {
    pub labelers_version: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod room_search_hit_kind_tests {
    //! The room-search reply's post-hit discriminator is additive in both
    //! directions: a message hit and an unflagged request keep their bytes,
    //! and a post hit — which only a flagged caller is ever sent — names its
    //! post and no log position.
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// `RoomSearchHit` as it shipped: a required `seq`, no discriminator.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct ShippedHit {
        seq: i64,
        rank: i64,
    }

    #[test]
    fn a_message_hit_is_byte_identical_to_a_shipped_one() {
        let shipped = encode_canonical(&ShippedHit { seq: 3, rank: 7 }).unwrap();
        let hit = RoomSearchHit {
            seq: Some(3),
            rank: 7,
            kind: RoomSearchHitKind::Message,
            post_id: None,
            extra: BTreeMap::new(),
        };
        assert_eq!(encode_canonical(&hit).unwrap(), shipped);
        let back: RoomSearchHit = decode(&shipped).unwrap();
        assert_eq!(back, hit);
    }

    #[test]
    fn a_post_hit_names_its_post_and_no_position() {
        let hit = RoomSearchHit {
            seq: None,
            rank: 5,
            kind: RoomSearchHitKind::Post,
            post_id: Some("ab".repeat(32)),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&hit).unwrap();
        let back: RoomSearchHit = decode(&bytes).unwrap();
        assert_eq!(back, hit);
        // The reason post hits are opt-in: a shipped decoder requires `seq`.
        assert!(
            decode::<ShippedHit>(&bytes).is_err(),
            "a post hit must never reach a caller that did not ask for one"
        );
    }

    #[test]
    fn a_hit_kind_a_newer_nest_introduced_decodes_as_unknown() {
        #[derive(Serialize)]
        struct FutureHit {
            rank: i64,
            kind: &'static str,
        }
        let bytes = encode_canonical(&FutureHit {
            rank: 1,
            kind: "event",
        })
        .unwrap();
        let back: RoomSearchHit = decode(&bytes).unwrap();
        assert_eq!(back.kind, RoomSearchHitKind::Unknown);
    }

    #[test]
    fn an_unflagged_request_is_byte_identical_to_a_shipped_one() {
        #[derive(Serialize)]
        struct ShippedRequest {
            room_id: String,
            query: String,
        }
        let shipped = encode_canonical(&ShippedRequest {
            room_id: "cc".repeat(32),
            query: "hedgehog".into(),
        })
        .unwrap();
        let req = RoomSearchRequest {
            room_id: "cc".repeat(32),
            query: "hedgehog".into(),
            limit: None,
            include_posts: false,
            extra: BTreeMap::new(),
        };
        assert_eq!(encode_canonical(&req).unwrap(), shipped);
        let back: RoomSearchRequest = decode(&shipped).unwrap();
        assert!(!back.include_posts, "an unflagged caller asks for no posts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_send_request() -> ChannelSendRequest {
        ChannelSendRequest {
            channel_id: "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            envelope: vec![0x01, 0x02, 0x03, 0x04, 0x05],
            expect_no_commit_since: None,
            attachment_refs: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Discriminating red (CBOR Layer 6, Domain L): the at-rest
    /// `ChannelEnvelope` wire bytes the client posts (and the nest
    /// strict-decodes in `EncryptedStorage::ingest_channel_envelope`)
    /// MUST be canonical dag-cbor. serde_bare (BARE) output fails strict
    /// canonical validation, so this is RED pre-flip and GREEN post-flip;
    /// a plain `to_bytes`→`from_bytes` round-trip would not discriminate.
    /// (Moved with the type from `fauna-mls::types`, 2026-07-08.)
    #[test]
    fn channel_envelope_at_rest_is_canonical_dagcbor() {
        let env = ChannelEnvelope::Application(vec![1, 2, 3, 4, 5]);
        let bytes = env.to_bytes().expect("encode ChannelEnvelope");
        let decoded: ChannelEnvelope =
            decode(&bytes).expect("at-rest ChannelEnvelope bytes must be canonical dag-cbor");
        assert!(matches!(decoded, ChannelEnvelope::Application(b) if b == vec![1, 2, 3, 4, 5]));
    }

    /// The floor delete variant's version skew, pinned at the wire
    /// (`conversation-rooms.md` § Roles and authorization → *Version skew,
    /// declared*): a build that predates the variant — modelled by the enum as
    /// it stood before it — **fails the decode** rather than mistaking the
    /// record for something it knows. That one fact is both halves of the
    /// skew: an older app's walk takes its undecodable-record skip, and an
    /// older nest's strict ingest decode refuses the send, storing nothing.
    #[test]
    fn a_build_that_predates_the_floor_delete_variant_fails_its_decode() {
        #[derive(Debug, Serialize, Deserialize)]
        enum ChannelEnvelopeBeforeFloorDelete {
            Application(#[serde(with = "serde_bytes")] Vec<u8>),
            Commit(#[serde(with = "serde_bytes")] Vec<u8>),
            RoomSealed {
                #[serde(with = "serde_bytes")]
                generation: Vec<u8>,
                #[serde(with = "serde_bytes")]
                ciphertext: Vec<u8>,
            },
        }
        let bytes = ChannelEnvelope::RoomFloorDelete(vec![0xa1, 0x61, 0x61, 0x01])
            .to_bytes()
            .expect("encode");
        assert!(matches!(
            ChannelEnvelope::from_bytes(&bytes).expect("this build decodes it"),
            ChannelEnvelope::RoomFloorDelete(b) if b == vec![0xa1, 0x61, 0x61, 0x01]
        ));
        assert!(
            decode::<ChannelEnvelopeBeforeFloorDelete>(&bytes).is_err(),
            "an older build must refuse the variant, never misread it"
        );
        // And the variants it does know are byte-identical across the skew.
        let sealed = ChannelEnvelope::RoomSealed {
            generation: vec![7; 32],
            ciphertext: vec![9; 40],
        }
        .to_bytes()
        .expect("encode");
        assert!(decode::<ChannelEnvelopeBeforeFloorDelete>(&sealed).is_ok());
    }

    #[test]
    fn channel_send_request_round_trips() {
        let req = sample_send_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelSendRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn channel_send_request_canonical_re_encodes_identically() {
        let req = sample_send_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: ChannelSendRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn channel_send_request_commit_precondition_round_trips() {
        // A gated send (an MLS commit) carries `expect_no_commit_since = Some(seq)`.
        let req = ChannelSendRequest {
            expect_no_commit_since: Some(7),
            ..sample_send_request()
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelSendRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.expect_no_commit_since, Some(7));
        assert_eq!(req, decoded);
        // Canonical re-encode is stable with the field present.
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes, bytes2);
    }

    #[test]
    fn channel_send_request_ungated_omits_the_precondition_key() {
        // Additive-everywhere floor: an ungated send (`None`) MUST NOT emit the
        // `expect_no_commit_since` key at all — `skip_serializing_if` keeps the
        // common-path wire free of the key, so no stray
        // `null` leaks into other members' `extra` maps.
        let bytes = encode_canonical(&sample_send_request()).unwrap();
        let decoded_map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(decoded_map.contains_key("channel_id"));
        assert!(decoded_map.contains_key("envelope"));
        assert!(
            !decoded_map.contains_key("expect_no_commit_since"),
            "an ungated send must omit the precondition key entirely, got keys {:?}",
            decoded_map.keys().collect::<Vec<_>>()
        );
    }

    /// The conversation kind's blob-reachability floor rides beside the
    /// sealed envelope as a plaintext hash list (`encryption-at-rest.md`
    /// § Per-content-kind conformance → Conversation messages row,
    /// 2026-09-08): present it round-trips and re-encodes stably.
    #[test]
    fn channel_send_request_attachment_refs_round_trip() {
        let req = ChannelSendRequest {
            attachment_refs: vec!["ab".repeat(32), "cd".repeat(32)],
            ..sample_send_request()
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelSendRequest = decode(&bytes).unwrap();
        assert_eq!(
            decoded.attachment_refs,
            vec!["ab".repeat(32), "cd".repeat(32)]
        );
        assert_eq!(req, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes, bytes2);
    }

    /// Additive-everywhere floor, the other direction: a send with no
    /// attachments MUST NOT emit the `attachment_refs` key at all, so the
    /// common-path wire carries no `attachment_refs` key — and no `extra` map
    /// grows a stray empty list.
    #[test]
    fn channel_send_request_without_attachments_omits_the_refs_key() {
        let bytes = encode_canonical(&sample_send_request()).unwrap();
        let decoded_map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(
            !decoded_map.contains_key("attachment_refs"),
            "a send with no attachments must omit attachment_refs entirely, got keys {:?}",
            decoded_map.keys().collect::<Vec<_>>()
        );
        // And a wire without the key decodes to the empty list.
        let decoded: ChannelSendRequest = decode(&bytes).unwrap();
        assert!(decoded.attachment_refs.is_empty());
    }

    #[test]
    fn channel_send_reply_round_trips() {
        let reply = ChannelSendReply {
            seq: 42,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ChannelSendReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn channel_fetch_request_round_trips() {
        let req = ChannelFetchRequest {
            channel_id: "deadbeef".repeat(8),
            after: 100,
            limit: 50,
            nest_url: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn channel_fetch_request_uses_default_limit_when_absent() {
        // Encode with limit field present but at the default, decode, verify.
        let req = ChannelFetchRequest {
            channel_id: "abcd".repeat(16),
            after: 0,
            limit: default_fetch_limit(),
            nest_url: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelFetchRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.limit, 100);
    }

    fn sample_fetch_reply() -> ChannelFetchReply {
        ChannelFetchReply {
            messages: vec![
                ChannelFetchEntry {
                    seq: 7,
                    envelope: vec![0xaa, 0xbb, 0xcc],
                    ..Default::default()
                },
                ChannelFetchEntry {
                    seq: 8,
                    envelope: vec![0xdd, 0xee, 0xff],
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn channel_fetch_reply_round_trips() {
        let reply = sample_fetch_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ChannelFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn channel_fetch_reply_canonical_re_encodes_identically() {
        let reply = sample_fetch_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ChannelFetchReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn channel_fetch_entry_legal_takedown_is_additive_and_round_trips() {
        use crate::posts::LegalTakedownMarker;
        // A normal entry omits `legal_takedown` entirely: adding the field
        // leaves a live message byte-identical to before it existed
        // (additive-everywhere, version-compatibility.md).
        let normal = ChannelFetchEntry {
            seq: 3,
            envelope: vec![1, 2, 3],
            ..Default::default()
        };
        assert!(normal.legal_takedown.is_none());
        let bytes_normal = encode_canonical(&normal).unwrap();
        assert_eq!(
            encode_canonical(&ChannelFetchEntry {
                seq: 3,
                envelope: vec![1, 2, 3],
                extra: BTreeMap::new(),
                legal_takedown: None,
                labels: Vec::new(),
                scores: Vec::new(),
                author: None,
            })
            .unwrap(),
            bytes_normal,
            "an omitted legal_takedown, labels or scores must not perturb the wire"
        );

        // A taken-down entry: the sealed envelope is withheld (empty) and the
        // marker carries the reference the client renders in its place.
        let taken_down = ChannelFetchEntry {
            seq: 4,
            envelope: vec![],
            legal_takedown: Some(LegalTakedownMarker {
                reference: "EU-DSA-2024/12345".into(),
                extra: BTreeMap::new(),
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&taken_down).unwrap();
        let decoded: ChannelFetchEntry = decode(&bytes).unwrap();
        assert_eq!(taken_down, decoded);
        assert!(decoded.envelope.is_empty());
        assert_eq!(
            decoded.legal_takedown.unwrap().reference,
            "EU-DSA-2024/12345"
        );
    }

    #[test]
    fn a_community_messages_labels_ride_beside_its_envelope_and_round_trip() {
        // `conversation-rooms.md` § The three classes → *What the home nest
        // does with its read*, purpose 2: what the room's labelers derived,
        // served beside the sealed envelope as metadata.
        let labelled = ChannelFetchEntry {
            seq: 5,
            envelope: vec![9, 9],
            labels: vec![fauna_core::content_category::ContentLabelEntry {
                category: "spam".into(),
                confidence_per_mille: 900,
            }],
            scores: vec![fauna_core::scoring::ScoreEntry {
                factor: "labeler:ab".into(),
                score: 900,
                tier: fauna_core::scoring::TIER_COMMUNITY,
                scorer_version: 1,
            }],
            ..Default::default()
        };
        let decoded: ChannelFetchEntry = decode(&encode_canonical(&labelled).unwrap()).unwrap();
        assert_eq!(decoded, labelled);
    }

    #[test]
    fn channel_list_for_actor_request_round_trips() {
        let req = ChannelListForActorRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChannelListForActorRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn channel_list_for_actor_reply_round_trips() {
        let reply = ChannelListForActorReply {
            channels: vec!["00".repeat(32), "ff".repeat(32)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ChannelListForActorReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn channel_list_for_actor_reply_canonical_re_encodes_identically() {
        let reply = ChannelListForActorReply {
            channels: vec!["aa".repeat(32), "bb".repeat(32), "cc".repeat(32)],
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ChannelListForActorReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    // ── fauna.conversations.keypackage.* ───────────────────────────

    fn sample_upload_request() -> KeypackageUploadRequest {
        KeypackageUploadRequest {
            packages: vec![
                ByteBuf::from(vec![0xaa, 0xbb, 0xcc]),
                ByteBuf::from(vec![0xdd, 0xee]),
            ],
            last_resort: false,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn keypackage_upload_request_round_trips() {
        let req = sample_upload_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: KeypackageUploadRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn keypackage_upload_request_canonical_re_encodes_identically() {
        let req = sample_upload_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: KeypackageUploadRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn keypackage_upload_reply_round_trips() {
        let reply = KeypackageUploadReply {
            stored: 2,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KeypackageUploadReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn keypackage_fetch_request_round_trips() {
        let req = KeypackageFetchRequest {
            actor_id: "ab".repeat(32),
            nest_url: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: KeypackageFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn keypackage_fetch_reply_some_round_trips() {
        let reply = KeypackageFetchReply {
            key_package: Some(vec![0x01, 0x02, 0x03, 0x04]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KeypackageFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn keypackage_fetch_reply_none_round_trips() {
        let reply = KeypackageFetchReply {
            key_package: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KeypackageFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn keypackage_fetch_reply_canonical_re_encodes_identically() {
        let reply = KeypackageFetchReply {
            key_package: Some(vec![0x10, 0x20, 0x30]),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: KeypackageFetchReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn keypackage_count_request_round_trips() {
        let req = KeypackageCountRequest {
            actor_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: KeypackageCountRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    // ── fauna.conversations.welcome.deliver ────────────────────────

    fn sample_welcome_dm_request() -> WelcomeDeliverRequest {
        WelcomeDeliverRequest {
            recipient_actor_id: "11".repeat(32),
            channel_id: "22".repeat(32),
            welcome_bytes: vec![0xaa, 0xbb, 0xcc, 0xdd],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: BTreeMap::new(),
        }
    }

    fn sample_welcome_group_request() -> WelcomeDeliverRequest {
        WelcomeDeliverRequest {
            recipient_actor_id: "33".repeat(32),
            channel_id: "44".repeat(32),
            welcome_bytes: vec![0x11, 0x22, 0x33],
            kind: WelcomeKind::Group {
                group_id: "55".repeat(32),
            },
            nest_url: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn welcome_deliver_dm_request_round_trips() {
        let req = sample_welcome_dm_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn welcome_deliver_dm_request_canonical_re_encodes_identically() {
        let req = sample_welcome_dm_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn welcome_deliver_group_request_round_trips() {
        let req = sample_welcome_group_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    fn sample_welcome_scheduling_request() -> WelcomeDeliverRequest {
        WelcomeDeliverRequest {
            recipient_actor_id: "66".repeat(32),
            channel_id: "77".repeat(32),
            welcome_bytes: vec![0x9a, 0x9b, 0x9c],
            kind: WelcomeKind::Scheduling,
            nest_url: Some("https://calonly.example".to_string()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn welcome_deliver_scheduling_request_round_trips() {
        let req = sample_welcome_scheduling_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn welcome_kind_scheduling_is_distinct_from_dm_and_group() {
        // The scheduling tag must not alias an existing kind — a Dm/Group on the
        // wire never decodes as Scheduling (the recipient routes on this).
        let dm = encode_canonical(&WelcomeKind::Dm).unwrap();
        let group = encode_canonical(&WelcomeKind::Group {
            group_id: "88".repeat(32),
        })
        .unwrap();
        let sched = encode_canonical(&WelcomeKind::Scheduling).unwrap();
        assert_ne!(sched, dm);
        assert_ne!(sched, group);
        assert_eq!(
            decode::<WelcomeKind>(&sched).unwrap(),
            WelcomeKind::Scheduling
        );
    }

    fn sample_welcome_folder_request() -> WelcomeDeliverRequest {
        WelcomeDeliverRequest {
            recipient_actor_id: "aa".repeat(32),
            channel_id: "bb".repeat(32),
            welcome_bytes: vec![0xf1, 0x1e, 0x5e, 0x70],
            kind: WelcomeKind::Folder {
                group_id: "cc".repeat(32),
            },
            nest_url: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn welcome_deliver_folder_request_round_trips() {
        let req = sample_welcome_folder_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn welcome_kind_folder_is_distinct_and_carries_group_id() {
        // The folder tag must not alias Group — both carry a group id, but the
        // recipient routes on the tag (folder pending-share vs. chat thread), so
        // a `Group` on the wire never decodes as `Folder` and vice versa.
        let group = encode_canonical(&WelcomeKind::Group {
            group_id: "cc".repeat(32),
        })
        .unwrap();
        let folder = encode_canonical(&WelcomeKind::Folder {
            group_id: "cc".repeat(32),
        })
        .unwrap();
        assert_ne!(folder, group);
        assert_eq!(
            decode::<WelcomeKind>(&folder).unwrap(),
            WelcomeKind::Folder {
                group_id: "cc".repeat(32),
            }
        );
    }

    #[test]
    fn welcome_deliver_group_request_canonical_re_encodes_identically() {
        let req = sample_welcome_group_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: WelcomeDeliverRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn welcome_deliver_reply_round_trips() {
        let reply = WelcomeDeliverReply {
            inbox_id: 42,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: WelcomeDeliverReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn keypackage_count_reply_round_trips() {
        let reply = KeypackageCountReply {
            count: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KeypackageCountReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn room_roster_report_request_round_trips() {
        let req = RoomRosterReportRequest {
            room_id: "ab".repeat(32),
            members: vec![
                RoomRosterEntryWire {
                    actor: "11".repeat(32),
                    role: Some("owner".into()),
                    extra: Default::default(),
                },
                RoomRosterEntryWire {
                    actor: "22".repeat(32),
                    role: Some("member".into()),
                    extra: Default::default(),
                },
            ],
            policy_version: Some(7),
            commit_seq: Some(42),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RoomRosterReportRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    /// The report's log position and the ack's `superseded_by` are
    /// **additive**. A leave or birth report carries no position and must
    /// decode as "unordered" rather than as position 0 — which the home nest
    /// would read as older than anything it holds and drop. An applied
    /// report's ack omits `superseded_by`, and that must read as "applied".
    /// Neither absence may leak into the forward-compat catch-all.
    #[test]
    fn a_reports_position_and_its_acks_supersession_are_additive() {
        let older_client = RoomRosterReportRequest {
            room_id: "ab".repeat(32),
            members: vec![RoomRosterEntryWire {
                actor: "11".repeat(32),
                role: Some("owner".into()),
                extra: Default::default(),
            }],
            policy_version: Some(2),
            commit_seq: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&older_client).unwrap();
        let decoded: RoomRosterReportRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.commit_seq, None, "absent is unordered, never 0");
        assert!(decoded.extra.is_empty());

        let older_nest = RoomRosterReportReply {
            members: 2,
            superseded_by: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&older_nest).unwrap();
        let decoded: RoomRosterReportReply = decode(&bytes).unwrap();
        assert_eq!(decoded.superseded_by, None, "absent reads as applied");
        assert!(decoded.extra.is_empty());

        let superseded = RoomRosterReportReply {
            members: 2,
            superseded_by: Some(12),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&superseded).unwrap();
        let decoded: RoomRosterReportReply = decode(&bytes).unwrap();
        assert_eq!(decoded, superseded);
    }

    /// A policy-less room's report carries neither roles nor a policy version.
    /// Both are `skip_serializing_if = "Option::is_none"`, so the absent
    /// form must survive the round trip as `None` rather than decoding to
    /// a defaulted role — the difference between "no roles" and "role
    /// member" is the whole policy-less/governed distinction
    /// (`conversation-rooms.md` § Implementation status today).
    #[test]
    fn a_policy_less_rooms_report_omits_role_and_policy_version() {
        let req = RoomRosterReportRequest {
            room_id: "cd".repeat(32),
            members: vec![RoomRosterEntryWire {
                actor: "33".repeat(32),
                role: None,
                extra: Default::default(),
            }],
            policy_version: None,
            commit_seq: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RoomRosterReportRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.members[0].role.is_none());
        assert!(decoded.policy_version.is_none());
        assert!(
            decoded.members[0].extra.is_empty(),
            "an omitted role must not land in the forward-compat catch-all"
        );
    }

    #[test]
    fn room_roster_report_remote_request_round_trips() {
        let req = RoomRosterReportRemoteRequest {
            room_id: "cd".repeat(32),
            nest_url: "https://home.example".into(),
            members: vec![RoomRosterEntryWire {
                actor: "ab".repeat(32),
                role: Some("owner".into()),
                extra: Default::default(),
            }],
            policy_version: Some(4),
            commit_seq: Some(9),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RoomRosterReportRemoteRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    /// The relayed leave names its room AND the home to reach — a leave
    /// carrying no `nest_url` is the same-nest kind, and the two must not be
    /// decodable as one another (the distinct-kind ruling: an own-nest that
    /// silently ignored the field would leave the member seated).
    #[test]
    fn room_leave_remote_request_round_trips_and_names_its_home() {
        let req = RoomLeaveRemoteRequest {
            room_id: "ef".repeat(32),
            nest_url: "https://home.example".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RoomLeaveRemoteRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
        assert_eq!(decoded.nest_url, "https://home.example");

        // A same-nest leave does not decode as a relayed one: the home is
        // required, not defaulted to this nest.
        let same_nest = encode_canonical(&RoomLeaveRequest {
            room_id: "ef".repeat(32),
            extra: Default::default(),
        })
        .unwrap();
        assert!(
            decode::<RoomLeaveRemoteRequest>(&same_nest).is_err(),
            "a leave with no home must not relay to nowhere"
        );
    }

    /// The relayed invite names its room's home AND carries the signed act
    /// verbatim — an invite carrying no `nest_url` is the same-nest kind, and
    /// the two must not be decodable as one another (the distinct-kind
    /// ruling: an own-nest that silently ignored the field would answer "no
    /// such room" for a room it does not home).
    #[test]
    fn room_invite_remote_request_round_trips_and_names_its_home() {
        let req = RoomInviteRemoteRequest {
            invite: vec![0xa1, 0x62, 0x69, 0x64],
            nest_url: "https://home.example".into(),
            invitee_node: String::new(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RoomInviteRemoteRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
        assert_eq!(decoded.nest_url, "https://home.example");
        assert!(
            decoded.invitee_node.is_empty(),
            "an empty invitee node is the inviter's own nest, and rides as absent"
        );

        let with_node = RoomInviteRemoteRequest {
            invitee_node: "https://third.example".into(),
            ..req.clone()
        };
        let decoded: RoomInviteRemoteRequest =
            decode(&encode_canonical(&with_node).unwrap()).unwrap();
        assert_eq!(decoded.invitee_node, "https://third.example");

        // A same-nest invite does not decode as a relayed one: the home is
        // required, not defaulted to this nest.
        let same_nest = encode_canonical(&RoomInviteRequest {
            invite: req.invite.clone(),
            invitee_node: String::new(),
            extra: Default::default(),
        })
        .unwrap();
        assert!(
            decode::<RoomInviteRemoteRequest>(&same_nest).is_err(),
            "an invite with no home must not relay to nowhere"
        );
    }

    #[test]
    fn room_roster_report_reply_round_trips() {
        let reply = RoomRosterReportReply {
            members: 3,
            superseded_by: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RoomRosterReportReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    /// The roster read's per-principal handle is **additive**, in both
    /// directions of the same-major compatibility rule
    /// (`version-compatibility.md`): a member the nest holds no user row for has
    /// both fields omitted and a client must read that as "no handle known",
    /// not as a decode failure; and an absent handle must not surface as an
    /// empty name, which is why the client treats `None` and `Some("")` alike
    /// at the boundary.
    #[test]
    fn a_roster_member_carries_an_optional_nest_joined_handle() {
        let named = RoomRosterMemberWire {
            principal: "aa".repeat(32),
            kind: "user".into(),
            role: Some("owner".into()),
            joined_at: 1_700_000_000_000,
            handle: Some("alice".into()),
            domain: Some("nest.test".into()),
            entry_id: Some("cc".repeat(32)),
            reception_pubkey: Some(vec![9u8; 4]),
            tip_wrapped: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&named).unwrap();
        let decoded: RoomRosterMemberWire = decode(&bytes).unwrap();
        assert_eq!(named, decoded);
        assert!(
            decoded.extra.is_empty(),
            "the two named fields must not land in the forward-compat catch-all"
        );

        // A member the serving nest holds no user row for — one homed on
        // another nest — carries neither field, and neither is invented.
        let elided = RoomRosterMemberWire {
            handle: None,
            domain: None,
            ..named.clone()
        };
        let bytes = encode_canonical(&elided).unwrap();
        let decoded: RoomRosterMemberWire = decode(&bytes).unwrap();
        assert_eq!(elided, decoded);
        assert!(decoded.handle.is_none() && decoded.domain.is_none());
        assert!(
            decoded.extra.is_empty(),
            "an omitted handle must not land in the forward-compat catch-all"
        );

        // And the shape a nest older than the join emits — the same bytes it
        // always emitted — still decodes, with the handle simply unknown.
        #[derive(Serialize)]
        struct PreJoinMemberWire {
            principal: String,
            kind: String,
            role: Option<String>,
            joined_at: i64,
        }
        let old = PreJoinMemberWire {
            principal: "bb".repeat(32),
            kind: "user".into(),
            role: None,
            joined_at: 7,
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&old).unwrap()).unwrap();
        assert_eq!(decoded.principal, "bb".repeat(32));
        assert!(
            decoded.handle.is_none() && decoded.domain.is_none(),
            "an older nest's roster reads as handle-unknown, never as a decode failure"
        );
    }

    /// **The wrap-target pair is additive in both directions** — the same
    /// same-major rule the handle join above is written about
    /// (`version-compatibility.md`), applied to the two fields that make a
    /// community room mintable from an app.
    ///
    /// The direction that actually bites: a client new enough to mint reading
    /// a roster reply that omits this field pair. It must read "this nest does not
    /// tell me the wrap targets" — and then decline to mint, because a mint
    /// it cannot address to the whole roster is one the nest will refuse on
    /// coverage. What it must NOT do is decode-fail, and it must not mistake
    /// the absence for "the roster has no wrap targets", which would look
    /// like a room whose members are all unkeyable.
    #[test]
    fn a_roster_member_carries_the_optional_wrap_target_pair() {
        let keyed = RoomRosterMemberWire {
            principal: "aa".repeat(32),
            kind: "user".into(),
            role: Some("member".into()),
            joined_at: 1_700_000_000_000,
            handle: None,
            domain: None,
            entry_id: Some("dd".repeat(32)),
            reception_pubkey: Some(vec![1, 2, 3, 4]),
            tip_wrapped: None,
            extra: BTreeMap::new(),
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&keyed).unwrap()).unwrap();
        assert_eq!(keyed, decoded);
        assert!(
            decoded.extra.is_empty(),
            "the wrap-target pair must not land in the forward-compat catch-all"
        );

        // A principal seated before the keyed ceremony carries neither, and
        // neither is invented.
        let unkeyed = RoomRosterMemberWire {
            entry_id: None,
            reception_pubkey: None,
            ..keyed.clone()
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&unkeyed).unwrap()).unwrap();
        assert_eq!(unkeyed, decoded);
        assert!(decoded.entry_id.is_none() && decoded.reception_pubkey.is_none());

        // The shape a nest older than this pair emits — every field it always
        // had, and nothing else — still decodes, with the wrap targets simply
        // unknown rather than known-absent.
        #[derive(Serialize)]
        struct PreWrapTargetMemberWire {
            principal: String,
            kind: String,
            role: Option<String>,
            joined_at: i64,
            handle: Option<String>,
            domain: Option<String>,
        }
        let old = PreWrapTargetMemberWire {
            principal: "bb".repeat(32),
            kind: "user".into(),
            role: Some("owner".into()),
            joined_at: 7,
            handle: Some("bob".into()),
            domain: Some("nest.test".into()),
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&old).unwrap()).unwrap();
        assert_eq!(decoded.handle.as_deref(), Some("bob"));
        assert!(
            decoded.entry_id.is_none() && decoded.reception_pubkey.is_none(),
            "an older nest's roster reads as wrap-targets-unknown, never as a decode failure"
        );
        assert!(
            decoded.extra.is_empty(),
            "an older nest's roster must not spill into the catch-all either"
        );
    }

    /// **`tip_wrapped` is additive, and its absence is "unknown", never
    /// "false".** The distinction is the whole field: a client reading a
    /// reply without it must not conclude the home nest's read was revoked (it
    /// would paint the grant as withdrawn) or that every member owes a key-in
    /// (it would re-wrap to all of them on every poll).
    #[test]
    fn a_roster_member_carries_an_optional_tip_wrap_answer() {
        let covered = RoomRosterMemberWire {
            principal: "aa".repeat(32),
            kind: "nest".into(),
            role: Some("member".into()),
            joined_at: 1_700_000_000_000,
            handle: None,
            domain: None,
            entry_id: Some("ee".repeat(32)),
            reception_pubkey: Some(vec![5u8; 4]),
            tip_wrapped: Some(false),
            extra: BTreeMap::new(),
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&covered).unwrap()).unwrap();
        assert_eq!(covered, decoded);
        assert!(
            decoded.extra.is_empty(),
            "the answer must not land in the forward-compat catch-all"
        );

        let unknown = RoomRosterMemberWire {
            tip_wrapped: None,
            ..covered
        };
        let decoded: RoomRosterMemberWire = decode(&encode_canonical(&unknown).unwrap()).unwrap();
        assert_eq!(
            decoded.tip_wrapped, None,
            "a row without the key reads as unknown"
        );
    }
}
