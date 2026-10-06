//! User-facing WS-RPC payload types for the **fauna-native inbox** — the
//! durable per-actor store-and-forward delivery queue for fauna-native
//! social / federation payloads (contact-requests, knocks, group
//! invites/messages, MLS Welcomes, security notices, cross-nest DMs).
//!
//! Two **drain** kinds, both caller-scoped (the reading actor is the
//! authenticated caller — there is deliberately **no `actor_id` request
//! field**, so a caller can only ever drain its own queue, caller-scoping
//! by construction, mirroring `fauna.email.inbox.fetch`):
//!
//! - `fauna.inbox.fetch` — return the caller's **undelivered** items
//!   without changing their delivery status (a peek-drain). The push
//!   event `PushEvent::InboxItem` is the best-effort prompt; this fetch
//!   is the durable backstop. Idempotent read.
//! - `fauna.inbox.ack` — the client reports the delivery-link ids it has
//!   **durably applied** (a contact-request became a contact, a Welcome
//!   joined a group); the nest marks them delivered so a re-fetch no
//!   longer returns them. Splitting fetch from ack fixes the latent
//!   data-loss bug in the old HTTP `GET /api/v1/inbox/{actor}` — which
//!   marked items delivered the instant they were *read*, dropping them
//!   if the client died before applying.
//!
//! And one **send** kind (the outbound counterpart — a write, *not*
//! caller-scoped; it names an explicit recipient):
//!
//! - `fauna.inbox.send` — the authed client hands its **home nest** a
//!   signed `(ContactRequest, Post)` tuple + the recipient actor (and the
//!   recipient's home-nest URL when cross-nest). The home nest does
//!   local-deliver-or-originate: same-nest recipients route through
//!   `routes::deliver_inbox_payload_core` directly, cross-nest recipients
//!   over the `fauna.federation.inbox.deliver` channel kind. This replaces
//!   the unauthenticated `POST /api/v1/inbox/{actor}` HTTP twin (clients
//!   reach remote actors *through* their home nest under Spec Y2, never by
//!   POSTing the remote nest directly). Mirrors the `fauna.events.remote_rsvp`
//!   → home-nest-originates precedent (`federation.md` § Federation residue
//!   surface). Because the leg is authed, the handler can bind
//!   `cr.sender == caller` — a property the unauthenticated twin can't.
//!
//! Ack marks delivered (status flip), it does **not** hard-delete: the
//! delivered rows are still read by `list_inbox_all` (the account-export
//! surface), so consume-by-deletion would strand it. (`poll_inbox_peek` is
//! currently vestigial — its former event-inbox-invitations consumer moved to
//! `fauna.events.inbox_invitations`.) The undelivered→delivered flip is what
//! drains the queue for `fauna.inbox.fetch` (which reads undelivered only).
//!
//! Kind registry entries live in `kind.rs::register_inbox_kinds`. Nest
//! handlers + permission gating live in `bins/fauna-nest/src/
//! inbox_handlers.rs`. Float-free: every field is an int or `bstr`.

use crate::{ByteBuf, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request for `fauna.inbox.fetch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxFetchRequest {
    /// Max items to return in this page. `0` (the `Default`) selects the
    /// handler default; the handler clamps to a small cap. Re-call until
    /// `more` is false.
    pub limit: u32,
    /// Skip cursor: return only undelivered items whose delivery-link id is
    /// **greater than** this. `None` (the `Default`) starts at the oldest.
    ///
    /// This is a *skip*, not an ack — the passed-over items stay undelivered
    /// and are returned again by the next cursor-less fetch. It exists so an
    /// item the client cannot apply (a kind with no surface yet, or an
    /// `InboxKind::Unknown` from a newer nest) stops shadowing everything
    /// behind it once un-ackable items fill a whole page; without it the
    /// missed-push backstop silently stops delivering. See
    /// `fauna_client_inbox::drain`.
    ///
    /// Additive + optional: omitted on the first page. A nest that ignores it
    /// (non-conforming) is caught by the drain, which stops on a non-advance
    /// rather than re-reading the same head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_id: Option<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.inbox.fetch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxFetchReply {
    /// Undelivered items, oldest first (delivery-link-id order).
    pub items: Vec<InboxItem>,
    /// `true` when more undelivered items remain past this page (detected
    /// via a `limit+1` sentinel fetch). Ack the applied items, then
    /// re-call `fauna.inbox.fetch`.
    pub more: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A single undelivered inbox item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxItem {
    /// Server-assigned delivery-link id — the cursor the client returns
    /// in `fauna.inbox.ack` once it has durably applied this item.
    pub id: i64,
    /// Opaque delivered payload bytes — a canonical fauna-native
    /// social/federation payload the client decodes (the nest holds no
    /// opening key in encrypted mode). Blob-backed payloads are resolved
    /// to their full bytes by the handler before send.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.inbox.ack`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxAckRequest {
    /// Delivery-link ids the client has durably applied. The nest marks
    /// each (that the caller owns and is still undelivered) delivered.
    pub ids: Vec<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.inbox.ack`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxAckReply {
    /// Number of items newly marked delivered. Already-delivered or
    /// not-owned ids are no-ops, so a replayed ack returns a smaller
    /// count — the ack is naturally idempotent.
    pub acked: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.inbox.send` — the client→home-nest bearer leg of
/// fauna-native social inbox delivery. The home nest verifies the caller
/// is the payload's sender, then local-delivers (same-nest) or originates
/// `fauna.federation.inbox.deliver` (cross-nest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxSendRequest {
    /// Recipient actor id, hex-encoded (32 bytes → 64 hex chars).
    pub recipient_actor_id: String,
    /// The recipient's home-nest URL. `None`/empty means **same nest as
    /// the caller's home nest** → the home nest delivers locally; a
    /// non-empty value routes the tuple to that peer over the federation
    /// channel. (Mirrors the `nest_url` axis of
    /// `fauna.conversations.welcome.deliver`.)
    pub recipient_nest_url: Option<String>,
    /// The canonical `(EmbedAsBytes-cr, EmbedAsBytes-post)` tuple — the
    /// signed `(ContactRequest, Post)` payload, the same bytes the HTTP
    /// twin received as its request body. `serde_bytes` (raw bstr — the
    /// per-actor client-kind convention, matching `InboxItem::payload`);
    /// the federation leg carries it as a byte string too.
    #[serde(with = "serde_bytes")]
    pub payload_bytes: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.inbox.send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxSendReply {
    /// The created inbox row id on delivery (`Some`), or `None` when a
    /// knock was stored (`allow_knock` mode, no prior contact). For a
    /// cross-nest send this is the *peer's* local inbox id. Mirrors the
    /// 201/202 split of the retiring HTTP twin and `FedInboxDeliverReply`.
    pub inbox_id: Option<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ───────────────────────────── Canonical inbox envelope ─────────────────────
//
// Every `push_inbox` write wraps its payload in **one** self-describing
// DAG-CBOR envelope carrying a `kind` discriminator, so the shared drain
// (`fauna-client-inbox`) dispatches by type instead of heuristically sniffing
// the bytes. Replaces the pre-2026-06-14 heterogeneous shapes — the cross-nest
// Welcome `serde_json` `{type:"welcome",…}`, the same-nest **raw** welcome
// bytes, the contact-request `(ContactRequest, Post)` tuple, the security-notice
// plain UTF-8 — onto this one shape (`federation.md` § residue: "one `kind` tag
// across all `push_inbox` writes"; `api-layers.md` § Inbox & Messaging, layer 1).
//
// Forward-compat (additive-everywhere, `version-compatibility.md`): a newer nest
// may write a `kind` an older client's drain doesn't know — `InboxKind` decodes
// any unknown discriminator to [`InboxKind::Unknown`], so the drain **skips** the
// item (leaves it un-acked, never crashes, never deletes), and processes it once
// the client updates. The two-level `{kind, payload-bytes}` shape (rather than an
// internally-tagged enum) is what makes that leniency possible and sidesteps
// serde's internally-tagged canonical-encoding pitfalls.

/// The self-describing discriminator on an [`InboxEnvelope`].
///
/// Serialized as its snake_case name (`"welcome"` / `"contact_request"` /
/// `"security_notice"` / `"room_invite"`) — DAG-CBOR text, float-free.
/// [`Unknown`] is the forward-compat catch-all: it is **never written** (writers
/// only construct known kinds) but any unrecognized discriminator decodes to it
/// so an older drain degrades gracefully against a newer nest.
///
/// [`Unknown`]: InboxKind::Unknown
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// An MLS Welcome (same-nest, cross-nest, or subscription-tier) — payload is
    /// [`WelcomeInbox`]. The drain feeds `welcome_bytes` to `ingest_welcome`.
    Welcome,
    /// A signed `(ContactRequest, Post)` tuple — payload is those canonical
    /// tuple bytes verbatim (the drain hands them to the contacts apply path,
    /// which already knows how to decode + verify the pair).
    ContactRequest,
    /// A server-originated security notice — payload is [`SecurityNoticeInbox`].
    SecurityNotice,
    /// An invitation into a room — payload is [`RoomInviteInbox`], carrying the
    /// inviter's signed act. This is how an invitee **discovers** the room it is
    /// being asked to accept into (`conversation-rooms.md` § Join rules and
    /// invites: an invite is "delivered to the invitee's home nest through the
    /// inbox plane"); the room plane mints no read kind of its own for it.
    ///
    /// Unlike the other three it is a **knock**: the drain never acks it, because
    /// every invitation is a decision only the user makes. It is consumed by the
    /// accept (which acks it) or by a decline (a bare ack) — the shape a staged
    /// folder share already has.
    RoomInvite,
    /// A discriminator this build does not recognize (a kind a newer nest
    /// introduced). The drain skips it, un-acked. Never serialized.
    #[serde(other)]
    Unknown,
}

/// The canonical inbox envelope stored as every `push_inbox` row payload and
/// returned as [`InboxItem::payload`]. `payload` is the kind-specific canonical
/// CBOR (see each [`InboxKind`] variant). The drain decodes this, matches
/// `kind`, then decodes `payload` into the per-kind struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxEnvelope {
    /// The payload discriminator.
    pub kind: InboxKind,
    /// Kind-specific canonical bytes (opaque at this layer).
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl InboxEnvelope {
    /// Wrap an MLS Welcome (any of same-nest / cross-nest / subscription-tier).
    pub fn welcome(w: &WelcomeInbox) -> Result<Self, fauna_cbor::EncodeError> {
        Ok(Self {
            kind: InboxKind::Welcome,
            payload: crate::encode_canonical(w)?.to_vec(),
            extra: BTreeMap::new(),
        })
    }

    /// Wrap a signed `(ContactRequest, Post)` tuple. The tuple bytes are already
    /// canonical CBOR, so they ride through as the envelope payload verbatim.
    pub fn contact_request(tuple_bytes: Vec<u8>) -> Self {
        Self {
            kind: InboxKind::ContactRequest,
            payload: tuple_bytes,
            extra: BTreeMap::new(),
        }
    }

    /// Wrap a server-originated security notice.
    pub fn security_notice(n: &SecurityNoticeInbox) -> Result<Self, fauna_cbor::EncodeError> {
        Ok(Self {
            kind: InboxKind::SecurityNotice,
            payload: crate::encode_canonical(n)?.to_vec(),
            extra: BTreeMap::new(),
        })
    }

    /// Wrap an invitation into a room.
    pub fn room_invite(i: &RoomInviteInbox) -> Result<Self, fauna_cbor::EncodeError> {
        Ok(Self {
            kind: InboxKind::RoomInvite,
            payload: crate::encode_canonical(i)?.to_vec(),
            extra: BTreeMap::new(),
        })
    }

    /// Encode this envelope to canonical DAG-CBOR bytes (the `push_inbox` row
    /// payload / the `fauna.inbox.fetch` item bytes).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        Ok(crate::encode_canonical(self)?.to_vec())
    }

    /// Strict-decode an envelope from a stored inbox-row / fetched-item payload.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        crate::decode_strict(bytes)
    }

    /// Decode the [`InboxKind::Welcome`] payload. Errors if `kind` isn't Welcome
    /// or the inner bytes don't decode.
    pub fn decode_welcome(&self) -> Result<WelcomeInbox, fauna_cbor::DecodeError> {
        crate::decode_strict(&self.payload)
    }

    /// Decode the [`InboxKind::SecurityNotice`] payload.
    pub fn decode_security_notice(&self) -> Result<SecurityNoticeInbox, fauna_cbor::DecodeError> {
        crate::decode_strict(&self.payload)
    }

    /// Decode the [`InboxKind::RoomInvite`] payload.
    pub fn decode_room_invite(&self) -> Result<RoomInviteInbox, fauna_cbor::DecodeError> {
        crate::decode_strict(&self.payload)
    }
}

/// Payload of an [`InboxKind::Welcome`] envelope — mirrors
/// [`crate::push_events::WelcomePayload`] so the drain backstop carries exactly
/// the metadata the best-effort push would have (same-nest welcomes historically
/// stored only raw bytes and lost the channel metadata on a missed push; the
/// envelope now carries it). `nest_url` stays a field for the cross-nest next
/// hop (`federation.md` § residue: "No server-side dereference of `nest_url`").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WelcomeInbox {
    /// The raw MLS Welcome message bytes the client feeds to `ingest_welcome`.
    #[serde(with = "serde_bytes")]
    pub welcome_bytes: Vec<u8>,
    /// The fauna channel id (hex), when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel_id: Option<String>,
    /// The inviter's home-nest URL, present for cross-nest welcomes — the
    /// client addresses its next hop here. The server never dereferences it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub nest_url: Option<String>,
    /// `"dm"` / `"group"` / `"scheduling"` — routes the channel to the right
    /// client surface.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel_type: Option<String>,
    /// The app-level group id, for group welcomes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub group_id: Option<String>,
    /// The sharing actor (hex ActorId), nest-stamped from the authenticated
    /// `welcome.deliver` caller. Populated for `channel_type == "folder"` so
    /// the recipient-side contact gate (`docs/goal/ui/folders.md` § Sharing)
    /// reads the sharer's contact-status without a roster round-trip, and so the
    /// "Shared by ‹handle›" surface can name them. Same-nest: the authenticated
    /// caller (unspoofable). Cross-nest: stamped by the sharer's nest into the
    /// federation relay (follow-on); absent ⇒ the gate treats the arrival as a
    /// stranger knock — the safe default.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by: Option<String>,
    /// The sharer's handle — the display name for the recipient's "Shared by
    /// ‹handle›" surface (`docs/goal/ui/folders.md` § Sharing). **Join rule**
    /// (`docs/goal/architecture/federation.md` § Cross-nest shared folders +
    /// channel append → *The cross-nest owner label*): **bare** (with
    /// [`Self::shared_by_domain`] absent) for a same-nest sharer, nest-resolved
    /// from [`Self::shared_by`]'s local `users` row; **paired** with
    /// [`Self::shared_by_domain`] for a cross-nest sharer, stamped by the
    /// sharer's home nest and forwarded by the recipient's nest only once it
    /// bound that domain to the origin's key. A bare handle therefore always
    /// means a local user. Absent for a handle-less sharer or an unverified
    /// cross-nest share. Cosmetic — never gates the contact decision.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by_handle: Option<String>,
    /// The cross-nest sharer's handle **domain**, the half of the pair
    /// [`Self::shared_by_handle`] is joined with at display
    /// (`fauna_core::format::qualified_handle` → `alice@example.com`). Set only
    /// beside a verified cross-nest [`Self::shared_by_handle`]; `None`
    /// same-nest (a bare handle means local). Wire-additive.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by_domain: Option<String>,
    /// The shared folder's **name**, nest-resolved from the claimed set's own
    /// row (never sender-asserted — the `welcome.deliver` home nest holds the
    /// authoritative `folders` row for the claimed channel). Populated for
    /// `channel_type == "folder"` so a **cross-nest** recipient — whose own
    /// nest holds no row for the set — can name the pending share and, on
    /// accept, record the foreign set's name in their folder-key custody
    /// (`docs/goal/ui/folders.md` § Sharing; Phase 2 client read-side).
    /// Same-nest recipients also get it (uniform stamp), though they could
    /// resolve it from their roster. Display-only — never a lookup key.
    /// Wire-additive: absent (post-scrub or seal-only share) ⇒ a name-less
    /// pending share.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name: Option<String>,
    /// [`Self::set_name`], sealed — see
    /// [`crate::folders::FolderSummary::name_sealed`]. Opaque to the nest;
    /// rendered client-side by `fauna_core::label_custody::render_set_name`.
    /// Resolved the same way `set_name` is, at both origination points (this
    /// nest's own `claimed_fs` row for a same-nest welcome; the ORIGIN nest's
    /// `claimed_fs` row, relayed via `FedWelcomeDeliverRequest`, for a
    /// cross-nest one) — a shared set's seal is under the M2 content key
    /// every roster member holds, so a joined cross-nest recipient can open
    /// it exactly as a same-nest member can. Path-sealing S5c-2.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name_sealed: Option<ByteBuf>,
    /// The convergent salt [`Self::set_name_sealed`] opens under
    /// (`fauna_core::path_crypto::set_name_hash`) — ships as a pair with the
    /// seal or not at all.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name_hash: Option<ByteBuf>,
    /// The recipient's **access grant** on the shared set (`"reader"` /
    /// `"writer"`), resolved nest-authoritatively from the home nest's
    /// `folder_member_access` row at `welcome_deliver_core` — exactly as
    /// [`Self::set_name`] is resolved, and never sender-asserted. Populated for
    /// `channel_type == "folder"`; carried so a **cross-nest** recipient can
    /// discover its own grant at all (its own nest holds no role row for a
    /// foreign set) and record it on accept.
    ///
    /// **Advisory-for-UI only — never an authorization input**
    /// (`docs/goal/architecture/federation.md` § Cross-nest → *Recipient-side
    /// access discovery*): it decides whether the client OFFERS a folder
    /// binding; the write kinds are gated on the home nest's own role row.
    /// Wire-additive: absent (a non-folder channel) ⇒ unknown ⇒ treated as reader.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub access: Option<String>,
    /// The home (= origin) nest's deployment `nest_actor_id` (hex 32-byte pubkey)
    /// for a cross-nest `folder` share — the byte-plane SPKI-pin trust root the
    /// recipient's agent graduates against (it holds no account on the home nest;
    /// `docs/goal/architecture/security.md` § Transport trust). `None` for a
    /// same-nest share or a relay-unaware origin ⇒ the byte plane keeps
    /// `RequireWebPki`. Wire-additive.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub home_nest_actor_id: Option<String>,
    /// The folder's content residency (`"metadata_only"` / `"full"`) for a
    /// cross-nest `folder` share, stamped by the home (= origin) nest off its
    /// own row beside [`Self::access`] (`docs/goal/architecture/federation.md`
    /// § Cross-nest shared folders + channel append → *Relay serving across
    /// nests*, the `residency` stamp) — what seeds the recipient's
    /// `ForeignFolder.residency`. `None` for a same-nest share (the recipient
    /// reads its row) ⇒ *not stated*, never *full*.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub residency: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Payload of an [`InboxKind::SecurityNotice`] envelope — a server-originated
/// notice the client surfaces (display-only; not a contact/welcome to apply).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SecurityNoticeInbox {
    /// Short subject line.
    pub subject: String,
    /// Notice body.
    pub body: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Payload of an [`InboxKind::RoomInvite`] envelope — one invitation into a
/// room, as it reaches the invitee (`conversation-rooms.md` § Join rules and
/// invites).
///
/// It carries the inviter's **signed act verbatim** and adds only what the
/// invitee needs in order to *reach* the room, for one reason: the signed record
/// already names the room, the invitee, the role and the policy version, and it
/// is the thing that survives the nest boundary. Re-stating any of those beside
/// it would create a second, unsigned copy an invitee could be shown instead of
/// the one it verified — so this struct deliberately restates nothing. The
/// reader decodes `signed_invite` as a `fauna_mls::room_policy::SignedRoomInvite`
/// and runs `verify_signature` before naming anybody (the nest binds the signer
/// to the authenticated caller on the way in, but *this record* is what crossed
/// the boundary, which is the whole reason it is signed).
///
/// This layer holds no MLS vocabulary, so the bytes ride through opaque — the
/// same treatment [`InboxKind::ContactRequest`]'s tuple gets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RoomInviteInbox {
    /// Canonical DAG-CBOR `fauna_mls::room_policy::SignedRoomInvite` bytes,
    /// exactly as the inviter signed them.
    #[serde(with = "serde_bytes")]
    pub signed_invite: Vec<u8>,
    /// The room's home nest, when it is not the invitee's own — the next hop an
    /// acceptance addresses (`fauna.conversations.room.accept_invite_remote`,
    /// which the invitee's own nest relays to it). Absent for a room homed on
    /// this nest. On a cross-nest delivery the invitee's nest writes it from
    /// the delivering peer's **verified** identity, never from the request
    /// body (`conversation-rooms.md` § Join rules and invites → *A cross-nest
    /// invitation*). The server never dereferences it on the invitee's behalf
    /// beyond that relay (`federation.md` § residue).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub room_node: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_reply() -> InboxFetchReply {
        InboxFetchReply {
            items: vec![
                InboxItem {
                    id: 1,
                    payload: vec![0xde, 0xad, 0xbe, 0xef],
                    extra: Default::default(),
                },
                InboxItem {
                    id: 7,
                    payload: vec![],
                    extra: Default::default(),
                },
            ],
            more: true,
            extra: Default::default(),
        }
    }

    #[test]
    fn fetch_request_round_trips() {
        let req = InboxFetchRequest {
            limit: 32,
            after_id: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn fetch_request_default_is_zero_limit() {
        let req = InboxFetchRequest::default();
        assert_eq!(req.limit, 0);
        assert_eq!(req.after_id, None, "default starts at the oldest row");
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn fetch_request_skip_cursor_round_trips_and_is_omitted_when_absent() {
        let with_cursor = InboxFetchRequest {
            limit: 32,
            after_id: Some(4096),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&with_cursor).unwrap();
        let decoded: InboxFetchRequest = decode(&bytes).unwrap();
        assert_eq!(with_cursor, decoded);

        // Additive-everywhere: an absent cursor must not appear on the wire, so
        // a client that never sets it encodes byte-identically to one built
        // before the field existed — an older nest sees exactly what it always
        // saw (`version-compatibility.md`).
        let without = InboxFetchRequest {
            limit: 32,
            after_id: None,
            extra: Default::default(),
        };
        let without_bytes = encode_canonical(&without).unwrap();
        assert!(
            !String::from_utf8_lossy(&without_bytes).contains("after_id"),
            "absent cursor must be omitted, not encoded as null"
        );
        assert!(bytes.len() > without_bytes.len());
    }

    #[test]
    fn fetch_reply_round_trips() {
        let reply = sample_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn fetch_reply_canonical_re_encodes_identically() {
        let reply = sample_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: InboxFetchReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn fetch_reply_default_is_empty() {
        let reply = InboxFetchReply::default();
        assert!(reply.items.is_empty());
        assert!(!reply.more);
    }

    #[test]
    fn ack_request_round_trips() {
        let req = InboxAckRequest {
            ids: vec![1, 7, 42],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxAckRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn ack_reply_round_trips() {
        let reply = InboxAckReply {
            acked: 3,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxAckReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn send_request_round_trips_cross_nest() {
        let req = InboxSendRequest {
            recipient_actor_id: "aa".repeat(32),
            recipient_nest_url: Some("https://peer.example".into()),
            payload_bytes: vec![0xca, 0xfe, 0xba, 0xbe],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxSendRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn send_request_round_trips_same_nest() {
        // None recipient_nest_url is the same-nest case.
        let req = InboxSendRequest {
            recipient_actor_id: "bb".repeat(32),
            recipient_nest_url: None,
            payload_bytes: vec![],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxSendRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.recipient_nest_url.is_none());
    }

    #[test]
    fn send_request_canonical_re_encodes_identically() {
        let req = InboxSendRequest {
            recipient_actor_id: "cd".repeat(32),
            recipient_nest_url: Some("https://b.example".into()),
            payload_bytes: vec![1, 2, 3, 4, 5],
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: InboxSendRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn send_reply_round_trips() {
        for inbox_id in [Some(42_i64), None] {
            let reply = InboxSendReply {
                inbox_id,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: InboxSendReply = decode(&bytes).unwrap();
            assert_eq!(reply, decoded);
        }
    }

    #[test]
    fn send_reply_default_is_none() {
        let reply = InboxSendReply::default();
        assert!(reply.inbox_id.is_none());
    }

    // ── Canonical inbox envelope ────────────────────────────────────────────

    #[test]
    fn welcome_envelope_round_trips() {
        let w = WelcomeInbox {
            welcome_bytes: vec![0x01, 0x02, 0x03],
            channel_id: Some("ab".repeat(32)),
            nest_url: Some("https://peer.example".into()),
            channel_type: Some("folder".into()),
            group_id: Some("grp-1".into()),
            shared_by: Some("cd".repeat(32)),
            shared_by_handle: Some("alice".into()),
            shared_by_domain: Some("example.com".into()),
            set_name: Some("Holiday Photos".into()),
            set_name_sealed: Some(ByteBuf::from(vec![0xEDu8; 40])),
            set_name_hash: Some(ByteBuf::from(vec![0x5Au8; 32])),
            access: Some("writer".into()),
            home_nest_actor_id: Some("ef".repeat(32)),
            residency: Some("metadata_only".into()),
            extra: Default::default(),
        };
        let env = InboxEnvelope::welcome(&w).unwrap();
        assert_eq!(env.kind, InboxKind::Welcome);

        let bytes = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, env);
        assert_eq!(decoded.decode_welcome().unwrap(), w);
    }

    #[test]
    fn welcome_envelope_same_nest_minimal_round_trips() {
        // Same-nest welcome: no nest_url, all-optional metadata absent except
        // channel_id — the optional fields are omitted from the encoding.
        let w = WelcomeInbox {
            welcome_bytes: vec![0xaa; 5],
            channel_id: Some("cc".repeat(32)),
            channel_type: Some("dm".into()),
            ..Default::default()
        };
        let env = InboxEnvelope::welcome(&w).unwrap();
        let bytes = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        let w2 = decoded.decode_welcome().unwrap();
        assert_eq!(w2, w);
        assert!(w2.nest_url.is_none());
        assert!(w2.shared_by.is_none());
        assert!(w2.shared_by_handle.is_none());
    }

    #[test]
    fn contact_request_envelope_passes_tuple_bytes_through() {
        let tuple_bytes = vec![0xde, 0xad, 0xbe, 0xef];
        let env = InboxEnvelope::contact_request(tuple_bytes.clone());
        assert_eq!(env.kind, InboxKind::ContactRequest);
        assert_eq!(env.payload, tuple_bytes);

        let bytes = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, env);
        // The tuple bytes ride through verbatim — the drain hands them to the
        // existing contact-request decode path.
        assert_eq!(decoded.payload, tuple_bytes);
    }

    #[test]
    fn security_notice_envelope_round_trips() {
        let n = SecurityNoticeInbox {
            subject: "New device added".into(),
            body: "A device joined your account.".into(),
            extra: Default::default(),
        };
        let env = InboxEnvelope::security_notice(&n).unwrap();
        assert_eq!(env.kind, InboxKind::SecurityNotice);

        let bytes = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.decode_security_notice().unwrap(), n);
    }

    #[test]
    fn envelope_canonical_re_encodes_identically() {
        let env = InboxEnvelope::welcome(&WelcomeInbox {
            welcome_bytes: vec![1, 2, 3, 4, 5],
            channel_id: Some("ee".repeat(32)),
            channel_type: Some("group".into()),
            group_id: Some("g".into()),
            ..Default::default()
        })
        .unwrap();
        let bytes1 = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes1).unwrap();
        let bytes2 = decoded.to_canonical_bytes().unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn kind_serializes_as_snake_case_text() {
        // Pin the on-wire discriminator strings so a rename can't silently
        // change the stored shape.
        assert_eq!(
            encode_canonical(&InboxKind::Welcome).unwrap().to_vec(),
            encode_canonical(&"welcome").unwrap().to_vec()
        );
        assert_eq!(
            encode_canonical(&InboxKind::ContactRequest)
                .unwrap()
                .to_vec(),
            encode_canonical(&"contact_request").unwrap().to_vec()
        );
        assert_eq!(
            encode_canonical(&InboxKind::SecurityNotice)
                .unwrap()
                .to_vec(),
            encode_canonical(&"security_notice").unwrap().to_vec()
        );
        assert_eq!(
            encode_canonical(&InboxKind::RoomInvite).unwrap().to_vec(),
            encode_canonical(&"room_invite").unwrap().to_vec()
        );
    }

    #[test]
    fn room_invite_envelope_round_trips() {
        let i = RoomInviteInbox {
            signed_invite: vec![0xa1, 0xb2, 0xc3],
            room_node: None,
            extra: Default::default(),
        };
        let env = InboxEnvelope::room_invite(&i).unwrap();
        assert_eq!(env.kind, InboxKind::RoomInvite);

        let bytes = env.to_canonical_bytes().unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.decode_room_invite().unwrap(), i);
    }

    #[test]
    fn room_invite_carries_the_signed_bytes_verbatim() {
        // The reader verifies the signature over exactly these bytes, so any
        // re-encoding between the nest and the invitee would break it. Pin that
        // the payload survives a full envelope round trip untouched.
        let signed: Vec<u8> = (0u8..=255).collect();
        let env = InboxEnvelope::room_invite(&RoomInviteInbox {
            signed_invite: signed.clone(),
            room_node: Some("https://other.example".into()),
            extra: Default::default(),
        })
        .unwrap();
        let bytes = env.to_canonical_bytes().unwrap();
        let back = InboxEnvelope::from_canonical_bytes(&bytes)
            .unwrap()
            .decode_room_invite()
            .unwrap();
        assert_eq!(back.signed_invite, signed);
        assert_eq!(back.room_node.as_deref(), Some("https://other.example"));
    }

    #[test]
    fn unknown_kind_decodes_to_unknown_forward_compat() {
        // A newer nest writes a `kind` this build doesn't know. Mirror the
        // envelope map shape with a String discriminator and assert it decodes
        // to `Unknown` rather than erroring — the drain must skip, not crash.
        #[derive(Serialize)]
        struct RawEnvelope {
            kind: String,
            #[serde(with = "serde_bytes")]
            payload: Vec<u8>,
        }
        let raw = RawEnvelope {
            kind: "cross_nest_dm".into(),
            payload: vec![0x09, 0x09],
        };
        let bytes = encode_canonical(&raw).unwrap();
        let decoded = InboxEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.kind, InboxKind::Unknown);
        assert_eq!(decoded.payload, vec![0x09, 0x09]);
    }
}
