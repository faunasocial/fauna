//! Federation channel serving handlers (Spec Y2 slice 4 §4.E / hub Track D).
//!
//! These are the `fauna.federation.*` kinds a **verified peer nest** may invoke
//! over the long-lived federation channel ([`crate::federation_channel`]). Each
//! maps onto the **same DB work** its HTTP twin does today
//! (`docs/goal/architecture/federation.md` § Federation residue surface), minus
//! the per-request signature / skew / nonce — the channel authenticated the peer
//! once at handshake time (§4.B), so the handler trusts the connection's verified
//! peer `nest_id` (handed in as the handler's `[u8; 32]` subject, where the
//! per-actor router passes an actor).
//!
//! Registered into the `FederationRouter` (the kind allowlist) in
//! `lib.rs::build_app` via [`register_federation_handlers`]. The handler shape is
//! the per-actor `RpcHandler` verbatim (re-exported through `federation_router`).
//!
//! **Migration status (slice 4):** the conversations pair (KP-fetch + Welcome), nest-sync ×4 (pull/push/mls_pull/mls_ack), post
//! forward/get and feed.query are registered. Each maps
//! onto the **same DB op** its HTTP twin does (reusing the shared cores where
//! they exist — `routes::get_post_core`, `feed_routes::remote_query_feed_core`).
//! The legacy event legs (`calendar.invite_deliver` +
//! `event.rsvp_deliver`) were retired with the Decision-B § 4c legacy-calendar
//! cleanup, and reputation exchange/export with the federation
//! reputation leg (2026-10-02; `federation.md` § Federation residue surface).
//!
//! **Post-slice-5 addition:** `fauna.federation.inbox.deliver` (the social
//! cross-nest inbox delivery, 2026-06-06) — a 14th kind not in the original §4.E
//! 13, migrating the `POST /api/v1/inbox/{actor}` twin (reuses the shared
//! `routes::deliver_inbox_payload_core`). Its twin is the lone un-retired residue
//! twin (stays until the client→home-nest leg lands; `federation.md` § residue).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use fauna_core::data::{
    ArrivalOrigin, InboxMode, ReachVerdict, SupervisedReach, unauthenticated_reach_verdict,
};
use fauna_protocol::posts::LegalTakedownMarker;
use fauna_protocol::wrapped_blob::{BulkByteAccess, BulkByteMintPurpose};
use fauna_protocol::{RpcError, decode_strict, encode_canonical};

use crate::api_error::ApiError;
use crate::config::SubmissionPolicy;
use crate::db::now_epoch_secs;
use crate::federation_router::{FederationRouterBuilder, RpcHandler, RpcKindMeta};
use crate::feed_routes::{RemoteQueryRequest, RemoteQueryResponse, remote_query_feed_core};
use crate::routes::parse_32_bytes;
use crate::routes::{
    AppState, GetPostOutcome, InboxDeliveryOutcome, InboxRejection, InboxRejectionDisposition,
    deliver_inbox_payload_core, get_post_core,
};

// ── helpers ────────────────────────────────────────────────────────────────

fn malformed(err: impl std::fmt::Display) -> RpcError {
    tracing::debug!("federation handler malformed payload: {err}");
    RpcError::new("fauna.protocol.malformed", "error.protocol.malformed")
}

use crate::rpc_errors::internal;

/// Federation-namespaced not-found (e.g. an absent post / event on `post.get`,
/// `event.rsvp_deliver`). Distinct from `unauthenticated` (off the kind
/// allowlist) and `forbidden` (a failed `is_paired` gate).
fn not_found(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::not_found_ns("federation", reason)
}

/// A failed structural authz gate inside a handler — the verified peer nest is
/// not paired for the requested actor (`is_paired`), or a submission-policy
/// rejection. (The kind allowlist + per-nest throttle gate run *before* dispatch;
/// this is the per-handler residue authz the HTTP twin also applied.)
fn forbidden(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::forbidden_ns("federation", reason)
}

/// Named `invalid_request` but wire code is `invalid_params` (pre-existing;
/// preserved as-is — renaming the code would be a wire-visible change).
fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_params_ns("federation", reason)
}

fn decode<T: serde::de::DeserializeOwned>(payload: &Bytes) -> Result<T, RpcError> {
    decode_strict::<T>(payload).map_err(malformed)
}

fn encode_reply<T: Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    Ok(Bytes::from(
        encode_canonical(reply).map_err(internal)?.to_vec(),
    ))
}

fn parse_actor(actor_hex: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(actor_hex).ok_or_else(|| malformed("invalid actor_id hex"))
}

/// Parse a 32-byte hex id (actor / namespace / channel / post / event), with a
/// field-named `invalid_params` on failure.
fn parse_id32(hex_str: &str, what: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_request(format!("invalid {what} hex")))
}

/// Map a shared-core [`ApiError`] (feed.query) onto the federation error space.
fn map_api_error(api: ApiError) -> RpcError {
    crate::rpc_errors::rpc_error_from_api_ns("federation", api, |ns, reason| {
        crate::rpc_errors::forbidden_ns(ns, reason)
    })
}

// ── top-level registration (assembled in lib.rs::build_app) ──────────────────

/// Register every `fauna.federation.*` serving handler into the
/// [`FederationRouter`] — its registered kinds ARE the federation kind allowlist
/// (§4.C). One call per residue area, mirroring `rpc_router`'s per-area pattern.
pub fn register_federation_handlers(b: &mut FederationRouterBuilder) {
    register_conversations_federation_handlers(b);
    register_folder_federation_handlers(b);
    register_backup_federation_handlers(b);
    register_succession_federation_handlers(b);
    register_sync_federation_handlers(b);
    register_post_federation_handlers(b);
    register_feed_federation_handlers(b);
    register_reports_federation_handlers(b);
    crate::abuse_report_federation::register_abuse_report_federation_handlers(b);
    register_trends_federation_handlers(b);
}

// ── conversations: §4.E rows 1–2 ────────────────────────────────────────────

/// `fauna.federation.keypackage.fetch` request — the cross-nest KP fetch a peer
/// nest relays on its client's behalf. No `nest_url` (this nest IS the target)
/// and no signature (channel-authed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedKeypackageFetchRequest {
    pub target_actor_id: String,
}

/// `fauna.federation.keypackage.fetch` reply — the (consumed) key package, or
/// `None` when the target has no usable KP. Mirrors `channel_routes::fetch_key_package`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedKeypackageFetchReply {
    #[serde(default, with = "serde_bytes")]
    pub key_package: Option<Vec<u8>>,
}

/// `fauna.federation.welcome.deliver` request — a peer nest delivers an MLS
/// Welcome to a local recipient. `origin_nest_url` is the *originating* nest's
/// URL, so the recipient client can address its reply hop.
///
/// It is **optional, and a peer may decline to name one** — the handler's
/// no-declared-origin arm relays no home, and the client treats that as "no
/// datum" rather than as an assertion that the channel is same-nest. What a *declared* origin buys is that the nest resolves it
/// against the connection's verified `origin_nest_id`, so it can only ever name
/// the sender's own nest.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FedWelcomeDeliverRequest {
    pub recipient_actor_id: String,
    pub channel_id: Option<String>,
    #[serde(with = "serde_bytes")]
    pub welcome_bytes: Vec<u8>,
    pub channel_type: Option<String>,
    pub group_id: Option<String>,
    pub origin_nest_url: Option<String>,
    /// The shared folder's name, resolved by the ORIGIN nest from its own
    /// claimed `folders` row (never client-asserted) for
    /// `channel_type == "folder"` relays — the recipient's nest holds no row
    /// for the set, so without this the pending share (and the recipient's
    /// accept-time foreign-set record) is name-less. Display-only on the
    /// receiving side — never a lookup key, never trusted for authorization
    /// (it is peer-asserted text, same trust class as the welcome itself).
    /// Wire-additive: absent from an old peer ⇒ name-less (`#[serde(default)]`).
    #[serde(default)]
    pub set_name: Option<String>,
    /// [`Self::set_name`], sealed — resolved by the ORIGIN nest from its own
    /// `claimed_fs.name_sealed` (never client-asserted), exactly as
    /// [`Self::set_name`] is. A shared set's seal is under the M2 content key
    /// every roster member holds, so the receiving nest's local recipient —
    /// once they accept and join the group — can open it exactly as a
    /// same-nest member can (`mls-group-key-material.md` § M2). Forwarded
    /// verbatim into the local `WelcomeInbox`/`WelcomePayload` this nest
    /// constructs for its own recipient. Wire-additive: absent from an old
    /// peer ⇒ name-less, same as an unstamped set. Path-sealing S5c-2.
    #[serde(default)]
    pub set_name_sealed: Option<serde_bytes::ByteBuf>,
    /// The convergent salt [`Self::set_name_sealed`] opens under — mirrors
    /// [`Self::set_name`]'s resolution, ships as a pair with the seal or not
    /// at all.
    #[serde(default)]
    pub set_name_hash: Option<serde_bytes::ByteBuf>,
    /// The recipient's **access grant** on the shared set (`"reader"` /
    /// `"writer"`), resolved by the ORIGIN nest — which is the set's home — from
    /// its own `folder_member_access` row (never client-asserted), exactly as
    /// [`Self::set_name`] is resolved. Without it a cross-nest recipient cannot
    /// discover its own grant at all: its nest holds no role row for a foreign
    /// set. `federation.md` § Cross-nest → *Recipient-side access discovery*.
    ///
    /// **Advisory-for-UI only on the receiving side** — same trust class as
    /// `set_name` (peer-asserted text). It decides whether the recipient's
    /// client offers a folder binding; every write is still gated by the home
    /// nest's own `require_foreign_writer`, so a lying peer asserting `"writer"`
    /// gains only the duty to accept writes it cannot read.
    /// Wire-additive: absent from an old peer ⇒ unknown ⇒ treated as reader.
    #[serde(default)]
    pub access: Option<String>,
    /// The ORIGIN nest's own deployment `nest_actor_id` (hex-encoded 32-byte
    /// Ed25519 pubkey — the value its channel binding signs and `fauna.nest.info`
    /// advertises as `nest_id`), for `channel_type == "folder"` relays. The
    /// origin nest IS the set's home, so this is the identity root the recipient's
    /// agent needs to graduate an SPKI pin for the direct byte-plane dial (the
    /// recipient holds no account there). Resolved from the origin's own identity,
    /// never peer-asserted. Wire-additive: absent from an old peer ⇒ the byte
    /// plane falls back to `RequireWebPki` (never weaker). `security.md`
    /// § Transport trust, the federation-granted Axis-2 row.
    #[serde(default)]
    pub home_nest_actor_id: Option<String>,
    /// The folder's content residency, resolved by the ORIGIN nest — the
    /// set's home — off its own claimed row ([`residency_stamp`]), exactly as
    /// [`Self::access`] is: `"metadata_only"` / `"full"`, for
    /// `channel_type == "folder"` relays. Seeds the recipient's
    /// `ForeignFolder.residency`; every federated folder read reply refreshes
    /// it. `None` for a non-folder relay ⇒ *not stated*, never *full*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// The set's owner's handle, joined by the ORIGIN nest — the set's home —
    /// from its own `users` row ([`owner_label_stamp`]), for
    /// `channel_type == "folder"` relays. A folder share is owner-gated
    /// (`share_core` verifies the caller owns the set), so the sharer this
    /// Welcome names IS the owner. Both-or-neither with [`Self::owner_domain`].
    /// The receiving nest forwards the pair to its recipient only once it bound
    /// the domain to the connection's verified `origin_nest_id`
    /// ([`verified_owner_label`]); `federation.md` § Cross-nest shared
    /// folders + channel append → *The cross-nest owner label*. Wire-additive: absent
    /// from an old peer ⇒ an unnamed sharer, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<String>,
    /// The ORIGIN nest's handle domain (`handle_domain_if_set()`), the half of
    /// the pair [`Self::owner_handle`] is joined with at display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_domain: Option<String>,
}

/// `fauna.federation.welcome.deliver` reply — the inbox row id created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedWelcomeDeliverReply {
    pub inbox_id: i64,
}

/// `fauna.federation.channel.fetch` request — the cross-nest pull of a channel's
/// **application messages** a peer nest relays on a member's behalf (the
/// open-federation message delivery for unpaired nests, `direct-messages.md`
/// § Technical Flow — Cross-Nest, step 3). `requesting_actor_id` is the member
/// (on the originating peer nest) draining the channel; this nest authorizes the
/// pull by that member's recorded channel membership bound to the verified peer
/// `nest_id` (NOT by the signature alone — `federation.md` § Trust model). No
/// `nest_url` (this nest IS the channel's home) and no signature (channel-authed).
///
/// `requesting_handle` + `requesting_domain` carry the requesting member's
/// `handle@domain` **as its own home nest (the originator) joins it from its
/// `users` row** — the id→handle ruling's mechanism (`federation.md` § Cross-nest
/// shared folders + channel append, the id→handle bullet, ratified 2026-09-10):
/// no nest asks another what handle an actor wears; the member's home nest
/// volunteers it on the drain it already originates, and this nest records it
/// beside the binding after binding the domain to the announcing nest's key
/// through discovery ([`record_announced_handle`]). Additive on both sides —
/// an old home nest ignores the pair (the member stays elided, the shipped
/// fallback), an old member-nest omits it (same) — which is exactly the
/// `fetch`-vs-`actors` dividing line that row draws: a benign degrade rides an
/// additive field, a corrupting one needs a distinct kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelFetchRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
    pub after: i64,
    pub limit: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requesting_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requesting_domain: Option<String>,
}

/// One `(seq, envelope)` ciphertext entry of a channel's log — the relay never
/// decrypts it (MLS carries the end-to-end guarantee, `federation.md` § Security).
/// A CBOR byte string, as every federation byte field is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FedChannelFetchMessage {
    pub seq: i64,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Present iff the serving (home) nest has taken this message down under a
    /// legal obligation — the `envelope` is then withheld (empty) and this
    /// carries the tombstone `reference` the requesting nest relays to its
    /// member's client (conversation twin of the local
    /// `ChannelFetchEntry.legal_takedown`; `moderation.md` § Categories &
    /// enforcement item 1). Absent for every message not taken down; when
    /// present the envelope is already withheld, so a reader that ignores the
    /// marker still never receives the content — fail-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown: Option<LegalTakedownMarker>,
    /// A **community room's** category verdicts for this message — the
    /// relayed twin of `ChannelFetchEntry.labels`, so a member homed on
    /// another nest reads what the room's named labelers derived exactly as a
    /// member homed here does (`community-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2).
    ///
    /// Filled exactly when the **requesting actor** — the member the relay
    /// names, bound to the calling nest by [`require_foreign_member`] — is a
    /// live floor member of the room: the same gate the same-nest read keys on
    /// its caller, never on the peer. Empty for every other class and actor,
    /// for a message no labeler labelled, and beside a withheld envelope. The
    /// relaying nest forwards them to its member and stores none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The same message's factor rows — the relayed twin of
    /// `ChannelFetchEntry.scores`. Same gate and same absences as
    /// [`Self::labels`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<fauna_core::scoring::ScoreEntry>,
    /// The record's nest-attested author (64-hex) — the relayed twin of
    /// `ChannelFetchEntry.author`, stated by this (the channel's home) nest and
    /// forwarded unchanged by the relaying nest. Absent for a record a
    /// nest-side writer appended with no authenticated sender and for a page
    /// whose authors read faulted — *no answer*, never permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// `fauna.federation.channel.fetch` reply — the channel's ciphertext entries with
/// `seq > after`, oldest first, capped at the request's `limit` (clamped `[1,500]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelFetchReply {
    pub messages: Vec<FedChannelFetchMessage>,
}

/// `fauna.federation.channel.actors` request — the cross-nest roster read a peer
/// nest relays on a member's behalf (the roster-read twin of `channel.fetch`;
/// `federation.md` § Cross-nest). `requesting_actor_id` is the member (on the
/// originating peer nest) whose add-participant heal needs the channel home's
/// authoritative roster union; this nest authorizes by that member's recorded
/// channel membership bound to the verified peer `nest_id`, exactly as the
/// fetch relay does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelActorsRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.channel.actors` reply — the union of `actor_channels` and
/// `channel_foreign_members`, **hex actor ids only, never nest URLs**: members
/// already read the full membership off the MLS ratchet tree, so the ids
/// disclose nothing new — URLs would.
///
/// ⚠ That subset argument is **not quite exact, deliberately**: on an *unclaimed*
/// channel `channel.fetch`/`send` auto-register any authenticated local caller
/// (`conversations_handlers::register_actor_channel_gated`), so the union can be a
/// **superset** of the ratchet tree — a local actor who merely touched the channel
/// and holds no leaf. A cross-nest member therefore learns "this actor id touched
/// this channel", an *association* rather than new key material (ids are public).
/// That residue sits inside the append-only/by-activity roster lifetime ratified in
/// `direct-messages.md` § Security Properties; it is accepted, not overlooked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelActorsReply {
    pub actors: Vec<String>,
}

/// `fauna.federation.conversation.generations.fetch` request — a **community
/// room's** foreign member reading its own generation wraps through its home
/// nest (`../behavior/conversation-rooms.md` § The home nest: "a member on a
/// foreign nest reaches the room only through their own home nest, which
/// originates the leg to the room's home").
///
/// ⚠ **There is deliberately no `entry_id` field, and there never may be.**
/// The room home resolves which wraps to serve from `requesting_actor_id`'s
/// own live floor entry
/// ([`conversations_handlers::room_generations_relayed`]). If the relaying
/// nest could name an entry, it could name **its own** — the home nest of a
/// community room is itself a seated floor principal with a room-read entry —
/// and a relay is specified to carry ciphertext and hold no wrap. The
/// requesting actor is bound to the verified `origin_nest_id` by
/// [`require_foreign_member`] exactly as on `channel.fetch`, so the worst a
/// hostile relay can ask for is a wrap belonging to one of *its own* members,
/// sealed to that member's reception key and opaque to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomGenerationsRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
}

/// `fauna.federation.conversation.generations.fetch` reply — the requesting
/// member's own wraps, in the same
/// [`fauna_protocol::conversations::RoomGenerationWire`] shape the same-nest
/// door serves, so the relaying nest forwards rather than re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomGenerationsReply {
    pub generations: Vec<fauna_protocol::conversations::RoomGenerationWire>,
}

/// `fauna.federation.conversation.roster.fetch` request — a room's foreign
/// member reading the room's **floor roster** through its own home nest
/// (`../behavior/conversation-rooms.md` § The home nest: "a member on a
/// foreign nest reaches the room only through their own home nest, which
/// originates the leg to the room's home").
///
/// The `{ requesting_actor_id, room_id }` pair is the whole request — the
/// `generations.fetch` shape beside it, and for a related reason: the room
/// home answers *the room's* roster, resolved from its own records, so there
/// is nothing for the relay to select. The requesting actor is bound to the
/// verified `origin_nest_id` by [`require_foreign_member`] exactly as on
/// `channel.fetch`, so a nest can only ever ask on behalf of its own members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomRosterRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
    /// `RoomListRosterRequest::at_policy_version`, carried to the room's home
    /// unchanged: also serve the signed policy the room held at this version.
    /// Additive — a room home older than the field ignores it and the member
    /// fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_policy_version: Option<u64>,
}

/// `fauna.federation.conversation.roster.fetch` reply — the room's live floor
/// in the same [`fauna_protocol::conversations::RoomListRosterReply`] shape
/// the same-nest door serves, so the relaying nest forwards rather than
/// re-encodes and **a foreign member sees the same names a home member sees**
/// (`conversations_handlers::room_roster_reply` is literally the same body).
///
/// **Handles ride this reply, and that is the point.** The roster's names are
/// what every member of the room is already shown; the requester is a member
/// the home nest itself admitted and pinned. This is emphatically *not* the
/// refused id→handle oracle (`federation.md` § Cross-nest shared folders +
/// channel append, the id→handle bullet): that shape answered "what handle
/// does actor X wear?" to any nest holding an id, under a gate that could
/// verify nothing about the asker. Here the asker must prove — through the
/// same binding that lets it fetch the room's ciphertext — that one of its
/// own members sits on this room's floor, which is exactly the audience the
/// ratified announce names ("the rooms it is in").
///
/// The neighbouring `channel.actors` read serves **hex actor ids only**, and
/// the difference is its consumer, not a stricter rule: that reply feeds a
/// membership-mutating heal that needs identity and never names, so names
/// there would be disclosure buying nothing. Here naming *is* the read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomRosterReply {
    pub roster: fauna_protocol::conversations::RoomListRosterReply,
}

/// `fauna.federation.conversation.room_labels.fetch` request — a room's
/// foreign member reading what the room's named labelers derived for
/// **room-restricted posts**, through its own home nest
/// (`../behavior/conversation-rooms.md` § The home nest: "a member on a
/// foreign nest reaches the room only through their own home nest, which
/// originates the leg to the room's home").
///
/// `room_id` is carried where the same-nest door's request deliberately is
/// not, and that is the federation gate's doing rather than a shape
/// disagreement: the same-nest read is post-scoped precisely so a caller need
/// not know which room indexed a post, while [`require_foreign_member`] needs
/// the channel id before anything is resolved — as on `channel.fetch`,
/// `generations.fetch` and `roster.fetch`. The requester always holds it: a
/// room post names its room in its own `KeyAccess::Room` arm, which is what it
/// opened the post by.
///
/// Naming the room also **narrows** the read: the home answers only for posts
/// its own reception-pass map assigns to *that* room, so one request — gated
/// on exactly one room's binding — cannot fish across rooms, and a relay
/// cannot learn which of a bag of post ids belong to rooms it has no member
/// in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomLabelsRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
    pub post_ids: Vec<String>,
}

/// `fauna.federation.conversation.room_labels.fetch` reply — the requesting
/// member's verdicts in the same
/// [`fauna_protocol::posts::PostRoomLabelsReply`] shape the same-nest door
/// serves, so the relaying nest forwards rather than re-encodes and **a
/// foreign member reads exactly what a member homed here reads**
/// (`posts_handlers::room_labels_for` is literally the same body).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomLabelsReply {
    pub labels: fauna_protocol::posts::PostRoomLabelsReply,
}

/// `fauna.federation.conversation.roster.report` request — a room's foreign
/// member REPORTING the room's floor roster after a membership or policy
/// commit it authored, through its own home nest: the write twin of
/// [`FedRoomRosterRequest`] (`../behavior/conversation-rooms.md` § The floor
/// roster — "the committing device reports the resulting roster to the home
/// nest"; § The home nest — every other nest relays).
///
/// `{ requesting_actor_id, room_id }` bind the reporter exactly as on the
/// read: the room home admits the report only when the requesting actor is a
/// recorded foreign member of the channel whose recorded home is the verified
/// `origin_nest_id` ([`require_foreign_member`]), so a nest can report only on
/// behalf of its own members — never for a member it does not carry. The rest
/// is the same-nest `room.roster_report` body verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomRosterReportRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
    pub members: Vec<fauna_protocol::conversations::RoomRosterEntryWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<u64>,
    /// This (home) nest's log position of the commit the roster follows —
    /// [`fauna_protocol::conversations::RoomRosterReportRequest::commit_seq`]
    /// verbatim, and what orders the report here. Absent on a report that
    /// follows no commit, which applies unordered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,
}

/// `fauna.federation.conversation.roster.report` reply — the room home's ack
/// in the same [`fauna_protocol::conversations::RoomRosterReportReply`] shape
/// the same-nest door returns, so the relaying nest forwards rather than
/// re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomRosterReportReply {
    pub ack: fauna_protocol::conversations::RoomRosterReportReply,
}

/// `fauna.federation.conversation.room.leave` request — a room's foreign
/// member LEAVING, through its own home nest: the self-scoped twin of
/// [`FedRoomRosterReportRequest`] (`../behavior/conversation-rooms.md` § Roles
/// and authorization grants *leave (remove self)* with no homing carve-out;
/// § The home nest — every other nest relays).
///
/// `{ requesting_actor_id, room_id }` bind the leaver exactly as the report
/// does: the room home admits the departure only when the requesting actor is
/// a recorded foreign member of the channel whose recorded home is the
/// verified `origin_nest_id` ([`require_foreign_member`]), so a nest can retire
/// only its own members' seats — never a member it does not carry, and never
/// somebody else's seat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomLeaveRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
}

/// `fauna.federation.conversation.room.leave` reply — the room home's ack in
/// the same [`fauna_protocol::conversations::RoomLeaveReply`] shape the
/// same-nest door returns, so the relaying nest forwards rather than
/// re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomLeaveReply {
    pub ack: fauna_protocol::conversations::RoomLeaveReply,
}

/// `fauna.federation.conversation.room.invite` request — a room's home nest
/// delivers a **community-room invitation** to an invitee homed here
/// (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*). Carries the inviter's signed act verbatim — the
/// record names the room, the invitee, the role and the policy version, and
/// is what this nest verifies before delivering a word of it — plus the
/// origin's declared address, honoured only against the connection's verified
/// identity ([`resolve_origin_home_url`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomInviteRequest {
    pub recipient_actor_id: String,
    /// Canonical DAG-CBOR `fauna_mls::room_policy::SignedRoomInvite` bytes.
    /// A CBOR byte string, as every federation byte field is.
    #[serde(with = "serde_bytes")]
    pub signed_invite: Vec<u8>,
    /// The room home's self-declared base URL, as the Welcome relay declares
    /// its own; `None` from a home with no claimed domain.
    #[serde(default)]
    pub origin_nest_url: Option<String>,
}

/// `fauna.federation.conversation.room.invite` reply — the delivered knock's
/// inbox row id on the invitee's nest (its local detail; the home stores
/// nothing of it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomInviteReply {
    pub inbox_id: i64,
}

/// `fauna.federation.conversation.room.accept` request — a foreign invitee's
/// acceptance, relayed by its own home nest to the room's home
/// (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*). The self-scoped shape of [`FedRoomLeaveRequest`]
/// plus the wrap target the same-nest `room.accept_invite` carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomAcceptRequest {
    pub requesting_actor_id: String,
    pub room_id: String,
    /// The accepting member's group-reception public key — its wrap target;
    /// empty for a seat not keyed yet. Plain `Vec<u8>` (int array on the wire).
    #[serde(with = "serde_bytes", default)]
    pub reception_pubkey: Vec<u8>,
}

/// `fauna.federation.conversation.room.accept` reply — the room home's ack in
/// the same [`fauna_protocol::conversations::RoomAcceptInviteReply`] shape the
/// same-nest door returns, so the relaying nest forwards rather than
/// re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomAcceptReply {
    pub ack: fauna_protocol::conversations::RoomAcceptInviteReply,
}

/// `fauna.federation.conversation.room.invite_issue` request — a room's
/// foreign member ISSUING an invitation, through its own home nest: the
/// issuing twin of [`FedRoomAcceptRequest`]
/// (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*, the foreign-inviter leg; § Roles and authorization
/// grants *invite* under `member-invite` with no homing carve-out).
///
/// `{ requesting_actor_id, signed_invite }` bind the inviter exactly as the
/// leave binds the leaver: the signed act's signer must BE the requesting
/// actor — the member the relaying nest authenticated — and the room home
/// admits it only when that actor is a recorded foreign member of the room
/// whose recorded home is the verified `origin_nest_id`
/// ([`require_foreign_member`]), so a nest can issue only its own seated
/// members' invitations, never a stranger's and never in another's name.
///
/// `invitee_node` is the invitee's home nest as the inviter knows it —
/// **empty for the relaying nest itself**, which the home resolves from the
/// connection's verified identity ([`resolve_origin_home_url`]), with
/// `origin_nest_url` the relaying nest's self-declared address honoured only
/// against that identity. The home never dials a URL the inviter's nest
/// merely asserted for its own members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomInviteIssueRequest {
    pub requesting_actor_id: String,
    /// Canonical DAG-CBOR `fauna_mls::room_policy::SignedRoomInvite` bytes.
    /// A CBOR byte string, as every federation byte field is.
    #[serde(with = "serde_bytes")]
    pub signed_invite: Vec<u8>,
    /// The invitee's home nest as the inviter knows it; empty for the
    /// relaying nest.
    #[serde(default)]
    pub invitee_node: String,
    /// The relaying nest's self-declared base URL, as the Welcome relay
    /// declares its own; `None` from a nest with no claimed domain.
    #[serde(default)]
    pub origin_nest_url: Option<String>,
}

/// `fauna.federation.conversation.room.invite_issue` reply — the room home's
/// ack in the same [`fauna_protocol::conversations::RoomInviteReply`] shape
/// the same-nest door returns, so the relaying nest forwards rather than
/// re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedRoomInviteIssueReply {
    pub ack: fauna_protocol::conversations::RoomInviteReply,
}

/// `fauna.federation.inbox.deliver` request — a peer nest delivers a Fauna-native
/// signed `(ContactRequest, Post)` inbox payload to a local recipient (the social
/// cross-nest delivery formerly carried by the `POST /api/v1/inbox/{actor}` HTTP
/// twin). The receiver runs the recipient's `InboxMode` routing
/// (`routes::deliver_inbox_payload_core`) exactly as the twin did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedInboxDeliverRequest {
    pub recipient_actor_id: String,
    /// The canonical `(EmbedAsBytes-cr, EmbedAsBytes-post)` tuple — the same bytes
    /// the HTTP twin received as its request body; a CBOR byte string.
    #[serde(with = "serde_bytes")]
    pub payload_bytes: Vec<u8>,
}

/// `fauna.federation.inbox.deliver` reply — the created inbox row id when
/// delivered, or `None` for a stored knock (allow_knock, no prior contact),
/// mirroring the HTTP twin's 201 / 202 split.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedInboxDeliverReply {
    pub inbox_id: Option<i64>,
}

/// Conversations federation handlers (§4.E rows 1–2). Five kinds here set
/// `forbid_replay` (which, via `FederationRouter::{retry_safe, hint_registry}`,
/// is also what stops the originating side's §4.D auto re-send): KP-fetch is
/// destructive, and the four delivery kinds (`welcome.deliver`,
/// `inbox.deliver`, `room.invite`, `room.invite_issue`) append or deliver +
/// charge quota per call.
pub fn register_conversations_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.keypackage.fetch",
        RpcKindMeta {
            // Destructive consume (`take_key_package`): a re-run takes a SECOND
            // one-time key package out of the target's pool.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: keypackage_fetch_handler(),
        },
    );
    b.add(
        "fauna.federation.welcome.deliver",
        RpcKindMeta {
            // NOT idempotent — same verdict, same grounds as `fauna.inbox.send`
            // (the 2026-07-31 client-table audit): this handler lands in
            // `push_inbox_with_quota` (the F10 quota leg below), whose
            // `content_id` mixes a timestamp + a monotonic counter
            // (`db/inbox.rs::inbox_content_id`) so it is unique by
            // construction — a re-sent Welcome inserts a second inbox row and
            // charges `inbox_bytes_used` again. The old `false` leaned on the
            // per-connection idempotency cache, which does not survive the
            // redial the §4.D retry path takes.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: welcome_deliver_handler(),
        },
    );
    b.add(
        "fauna.federation.channel.fetch",
        RpcKindMeta {
            // Idempotent read: the cursor-paged channel pull is re-runnable, so a
            // mid-call channel disconnect re-dials + re-sends safely (§4.D).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: channel_fetch_handler(),
        },
    );
    b.add(
        "fauna.federation.inbox.deliver",
        RpcKindMeta {
            // NOT idempotent — same verdict, same grounds as `fauna.inbox.send`
            // (the 2026-07-31 client-table audit): both legs run
            // `deliver_inbox_payload_core` → `push_inbox_with_quota`, and the
            // inbox is an append log whose `content_id` is unique by
            // construction (`db/inbox.rs::inbox_content_id` — timestamp + a
            // monotonic nonce, deliberately so), so a re-sent delivery inserts
            // a second row and charges the recipient's `inbox_bytes_used`
            // again. The rationale that used to sit here leaned on the
            // per-connection idempotency cache — which cannot survive the
            // redial the §4.D retry path takes, so it protected nothing.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: inbox_deliver_handler(),
        },
    );
    b.add(
        "fauna.federation.channel.append",
        RpcKindMeta {
            // Redelivery-safe by design, never nest-side exactly-once (S3): a
            // same-key retry replays from the per-connection idempotency cache;
            // past that cache the MLS consumers quiet-skip duplicate ciphertext.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: channel_append_handler(),
        },
    );
    b.add(
        "fauna.federation.channel.leave",
        RpcKindMeta {
            // Idempotent self-scoped delete: an absent row is success.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: channel_leave_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.write_token.mint",
        RpcKindMeta {
            // An in-memory short-TTL token mint: a §4.D re-send is one more
            // entry until gc — no row, no quota, no counter (same posture as
            // `{folder,backup}.write_token.mint`).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: conversation_write_token_mint_handler(),
        },
    );
    b.add(
        "fauna.federation.channel.actors",
        RpcKindMeta {
            // Idempotent pure read (no cursor, no consume): a mid-call channel
            // disconnect re-dials + re-sends safely (§4.D).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: channel_actors_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.generations.fetch",
        RpcKindMeta {
            // Idempotent pure read, like the roster read beside it: the
            // wraps are already-minted rows, and serving them twice changes
            // nothing on either nest.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_generations_fetch_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.roster.fetch",
        RpcKindMeta {
            // Idempotent pure read: the floor roster is rows this nest
            // already holds, and serving them twice registers nothing.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_roster_fetch_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.room_labels.fetch",
        RpcKindMeta {
            // Idempotent pure read, like the two reads beside it: the
            // verdicts are already-derived rows, and serving them twice
            // changes nothing on either nest.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_labels_fetch_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.roster.report",
        RpcKindMeta {
            // Idempotent by construction: the report replaces the floor
            // wholesale, so a re-sent frame lands the same rows again and
            // nothing is fanned out — the same-nest `roster_report` posture.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_roster_report_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.room.leave",
        RpcKindMeta {
            // Idempotent by construction (the same-nest `room.leave` answers a
            // departed caller the same way; its replay flag differs): both
            // writes are self-scoped severances whose postcondition is "this
            // member is off this floor and off this relay", which a second
            // run re-establishes rather than violates. The flag is what lets
            // the §4.D auto re-send happen at all, and the re-send is the
            // point — a leave that landed must not come back to the leaver as
            // a failure because the channel dropped before the ack
            // (`fauna_protocol::conversations::RoomLeaveRemoteRequest`).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_leave_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.room.invite",
        RpcKindMeta {
            // NOT idempotent — `inbox.deliver`'s verdict on the same grounds:
            // a delivery appends an inbox row and charges the invitee's
            // quota per call, and the per-connection idempotency cache does
            // not survive the redial the §4.D retry path takes. The home
            // consumes its row on a failed delivery and the inviter retries
            // the whole invitation, which re-delivers exactly one knock.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_invite_deliver_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.room.accept",
        RpcKindMeta {
            // Idempotent by construction, `room.leave`'s posture: the body
            // answers a requester its recorded invitation already seated from
            // this nest with its role and re-asserts the binding, so the §4.D
            // re-send reports the seating that landed
            // (`conversations_handlers::room_accept_relayed`).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_accept_handler(),
        },
    );
    b.add(
        "fauna.federation.conversation.room.invite_issue",
        RpcKindMeta {
            // NOT idempotent — the same-nest `room.invite`'s verdict, and
            // `room.invite`'s above: the body records a row and delivers a
            // knock per call (to its own inbox plane or by a push to the
            // invitee's nest, charging quota either way). The inviter's nest
            // forbids replay on its client leg for the same reason, and the
            // inviter re-issues, which refreshes the pending invitation
            // rather than duplicating it.
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_invite_issue_handler(),
        },
    );
}

/// Maps onto `state.db.take_key_package` — the same destructive consume the HTTP
/// twin `channel_routes::fetch_key_package` does, returning a one-time KP if the
/// pool is non-empty else the last-resort KP without deleting it (§ Key packages).
fn keypackage_fetch_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedKeypackageFetchRequest = decode(&payload)?;
            let target = parse_actor(&req.target_actor_id)?;
            let key_package = state.db.take_key_package(&target).await.map_err(internal)?;
            encode_reply(&FedKeypackageFetchReply { key_package })
        })
    })
}

/// Maps onto `state.db.push_inbox` + the Welcome push + `register_actor_channel`.
/// (This was once described as mirroring an HTTP twin `channel_routes::post_welcome`;
/// that route was deleted in the WS-RPC-everywhere rip and this is now the only
/// door.) A
/// cross-nest Welcome is wrapped in the inbox envelope the recipient client
/// reads, carrying — as the channel's home `nest_url` for the drain's reply hop
/// — an address resolved from the connection's **verified** `origin_nest_id`,
/// never the per-request peer-declared `origin_nest_url`.
fn welcome_deliver_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedWelcomeDeliverRequest = decode(&payload)?;
            let recipient = parse_actor(&req.recipient_actor_id)?;

            // F10: only deliver to a real local recipient. `welcome.deliver` is
            // open-federation (any unauthenticated peer), so without this gate a
            // peer could push Welcome rows — plus a push fan-out and channel
            // auto-registration — to arbitrary 32-byte ids, polluting
            // `actor_channels`/the inbox and (with the disk-fill history) bloating
            // storage. A genuine invitee has an account on this nest (they
            // published the key package the inviter fetched), so a non-registered
            // recipient is always spurious.
            if !state
                .db
                .is_actor_registered(&recipient)
                .await
                .map_err(internal)?
            {
                return Err(not_found("recipient not found"));
            }

            // The reach floor, cross-nest. This kind is open-federation — *any
            // unauthenticated peer* reaches it — and its wire carries **no signed
            // sender**: `welcome_bytes` is opaque MLS ciphertext, and a peer
            // nest's assertion about which of its users is calling is attribution,
            // not authorization (`federation.md` § Trust model). So there is no
            // contact edge to look up, and the floor fails closed: a party this
            // nest cannot name can never be an *approved* contact.
            //
            // As same-nest, a recipient already on the channel is being re-Welcomed
            // (in-band traffic, e.g. an idempotent relay retry) and keeps flowing;
            // only channel *establishment* is gated.
            let recipient_established = match req.channel_id.as_deref().and_then(parse_32_bytes) {
                Some(ch) => state
                    .db
                    .is_actor_in_channel(&recipient, &ch)
                    .await
                    .map_err(internal)?,
                None => false,
            };
            if !recipient_established {
                let supervised = state
                    .db
                    .get_guardian_policy(&recipient)
                    .await
                    .map_err(internal)?
                    .map(|p| SupervisedReach {
                        contact_approval: p.contact_approval,
                        federation_contact: p.federation_contact,
                    });
                if unauthenticated_reach_verdict(supervised, ArrivalOrigin::Federation)
                    != ReachVerdict::Proceed
                {
                    return Err(forbidden("recipient is not accepting new conversations"));
                }

                // The recipient's own inbox mode — the `closed` arm ONLY
                // (`direct-messages.md` § Reach policy). The other three modes
                // stay declared cross-nest gaps because their verdicts turn on
                // a *contact edge* and this wire carries no signed sender:
                // refusing on `allow_knock`/`contacts_only` here would refuse
                // cross-nest contacts too, which is what that follow-on is
                // blocked on. `closed` needs no sender identity — it is a fact
                // about the recipient alone, which is why the sibling
                // open-federation door already enforces it
                // (`deliver_inbox_payload_core`'s `"closed"` arm, pinned by
                // `inbox_deliver_closed_inbox_is_forbidden`). Leaving it
                // unenforced here made the two doors disagree, and this is the
                // door that lands MORE: an inbox row against the recipient's
                // quota, a `PushEvent::Welcome` on their live socket, a device
                // push, and `register_actor_channel` seating them on the
                // channel — all while their setting reads "No new messages
                // accepted" (`inbox_privacy.closed_desc`), unqualified, in all
                // 7 apps.
                //
                // ⚠ Deliberately NOT keyed on `req.channel_type`: it is
                // peer-supplied and this handler already refuses to trust it
                // for authorization (see the folder-claim gate below). A gate
                // a hostile peer evades by relabelling one string is not a
                // gate, so under `closed` every claimed kind is refused —
                // including the folder shares and calendar invites the
                // *same-nest* door exempts (`welcome_mode_exemption`, which
                // reached the same conclusion from the other end: it stopped
                // keying on `req.kind` too, and now exempts only what nest
                // state vouches for). That
                // divergence is declared in § Reach policy: it costs a
                // `closed` recipient cross-nest folder shares, which is a
                // defensible reading of the setting they chose and is undone
                // the moment they leave `closed`.
                //
                // Reject-don't-guess on an unknown stored token, exactly as the
                // same-nest arm does.
                let mode = state
                    .db
                    .get_inbox_mode(&recipient)
                    .await
                    .map_err(internal)?;
                match InboxMode::from_wire(&mode) {
                    Some(InboxMode::Closed) | None => {
                        return Err(forbidden("recipient is not accepting new conversations"));
                    }
                    Some(_) => {}
                }
            }

            let body = Bytes::from(req.welcome_bytes.clone());

            // Bind the channel's home URL — the routing datum the recipient's
            // drain relays `channel.fetch` over — to the connection's
            // handshake-**verified** `origin_nest_id`, NOT the per-request,
            // peer-declared `req.origin_nest_url`. This is the same choice the
            // folder-relay branch below already makes (its set name derives from
            // the verified `origin_nest_id`, never a writer-declared name), and
            // it closes the pre-guard channel-home plant. `welcome.deliver` is open-federation and
            // carries no signed sender, so a hostile peer that can deliver a
            // Welcome (every Dm/Group Welcome is ungated on the client, as DM
            // initiation requires) could otherwise hand the recipient any URL,
            // silently redirecting the victim's channel drain — and its fetch
            // metadata — to a nest of the attacker's choosing.
            //
            // Prefer a dial-**proven** address of the verified origin: an
            // attacker cannot reach proven status without the honest nest's key.
            // The declared URL is honoured only when it is itself one of the
            // origin's proven addresses (a genuine migration between two proven
            // addresses — the origin's current self-claim wins among its own),
            // and when the origin has NO proven address yet (first cross-nest
            // contact, where distrusting the declaration would refuse every
            // honest first invite). Either way the declaration is also recorded
            // as an UNPROVEN sighting bound to the verified identity, so a later
            // handshake can promote it and the directory's addresses-only-join
            // invariant is preserved (`db/channels.rs` § record_nest_address).
            let home_nest_url =
                resolve_origin_home_url(&state, &origin_nest_id, req.origin_nest_url.as_deref())
                    .await?;

            // The cross-nest owner label (`federation.md` § Cross-nest shared
            // folders + channel append → *The cross-nest owner label*): the
            // origin — the set's home — stamped its owner's handle + its own
            // domain; this nest forwards the pair only once the domain binds
            // to the verified `origin_nest_id`, awaited inline under the shared
            // cap (any failure delivers unstamped — never a refusal). Folder
            // relays only: the label names a share's sharer and nothing else.
            let (shared_by_handle, shared_by_domain) =
                if req.channel_type.as_deref() == Some("folder") {
                    verified_owner_label(
                        &state,
                        &origin_nest_id,
                        req.owner_handle.as_deref(),
                        req.owner_domain.as_deref(),
                    )
                    .await
                } else {
                    (None, None)
                };

            // Cross-nest Welcome → canonical DAG-CBOR inbox envelope (layer 1).
            // The verified-origin-resolved `home_nest_url` rides as `nest_url` so
            // the recipient client can address its next hop back to the inviter's
            // nest; the server never dereferences it (`federation.md` § residue).
            // The envelope is what gets stored — both in the blob and inline — so
            // the drain decodes one self-describing shape regardless of storage
            // mode.
            let inbox_bytes = Bytes::from(
                fauna_protocol::inbox::InboxEnvelope::welcome(
                    &fauna_protocol::inbox::WelcomeInbox {
                        welcome_bytes: body.to_vec(),
                        channel_id: req.channel_id.clone(),
                        nest_url: home_nest_url.clone(),
                        channel_type: req.channel_type.clone(),
                        group_id: req.group_id.clone(),
                        // The sharer's actor id stays unstamped cross-nest, so
                        // the recipient gate reads every relayed folder welcome
                        // as a stranger knock — the label below names, never
                        // admits (folders.md § Sharing; federation.md § … *The
                        // cross-nest owner label*, trust rule).
                        shared_by: None,
                        // The domain-verified owner label, paired: a handle
                        // with a domain beside it is a foreign user this nest
                        // bound to that domain's key; never a bare handle.
                        shared_by_handle: shared_by_handle.clone(),
                        shared_by_domain: shared_by_domain.clone(),
                        // Origin-nest-resolved set name for a folder share —
                        // display-only text (never a lookup key or an authz
                        // input); this nest holds no row to resolve it from.
                        set_name: req.set_name.clone(),
                        // Origin-nest-resolved seal + salt pair, forwarded
                        // verbatim (opaque to this nest, same as it is to the
                        // origin) — path-sealing S5c-2.
                        set_name_sealed: req.set_name_sealed.clone(),
                        set_name_hash: req.set_name_hash.clone(),
                        // Origin-nest-resolved access grant, same trust class as
                        // `set_name`: advisory-for-UI only. It lets the recipient
                        // client offer a folder binding; enforcement stays on the
                        // origin (= home) nest's write-kind gate.
                        access: req.access.clone(),
                        // Origin (= home) nest's deployment identity + owner-chosen
                        // cadence, carried through from the relay `req`. The former
                        // is the byte-plane SPKI-pin trust root the recipient's
                        // agent graduates against; the latter is the cadence the
                        // recipient applies (its own nest holds no `folders` row).
                        home_nest_actor_id: req.home_nest_actor_id.clone(),
                        // The home nest's residency stamp, carried through
                        // verbatim like `access`.
                        residency: req.residency.clone(),
                        extra: Default::default(),
                    },
                )
                .map_err(internal)?
                .to_canonical_bytes()
                .map_err(internal)?,
            );

            let blob_hash = if let Some(ps) = &state.payload_store {
                ps.store(&inbox_bytes)
                    .await
                    .map_err(internal)?
                    .1
                    .map(|h| h.digest())
            } else {
                None
            };

            // F10: enforce the recipient's inbox tier quota (leg-parity with
            // `inbox.deliver`), so a Welcome flood can't grow the inbox past the
            // tier limit. `push_inbox_with_quota` accounts the bytes; `check_quota`
            // rejects an over-limit / suspended recipient first.
            let row_id = if *state.enforce_tier_quotas.read().await {
                if let Err(e) = state.db.check_quota(&recipient, inbox_bytes.len()).await {
                    return Err(forbidden(format!("inbox quota: {e}")));
                }
                state
                    .db
                    .push_inbox_with_quota(&recipient, &inbox_bytes, blob_hash.as_ref())
                    .await
                    .map_err(internal)?
            } else {
                state
                    .db
                    .push_inbox(&recipient, &inbox_bytes, blob_hash.as_ref())
                    .await
                    .map_err(internal)?
            };

            state.ws.notify_push(
                &recipient,
                fauna_protocol::PushEvent::Welcome(fauna_protocol::push_events::WelcomePayload {
                    welcome_bytes: body.to_vec(),
                    channel_id: req.channel_id.clone(),
                    // Same verified-origin-resolved home as the inbox envelope
                    // above — never the peer-declared string.
                    nest_url: home_nest_url.clone(),
                    channel_type: req.channel_type.clone(),
                    group_id: req.group_id.clone(),
                    // Unstamped cross-nest ⇒ the recipient gate treats it as
                    // a knock; the verified label rides beside it (see the
                    // inbox envelope above).
                    shared_by: None,
                    shared_by_handle,
                    shared_by_domain,
                    set_name: req.set_name.clone(),
                    set_name_sealed: req.set_name_sealed.clone(),
                    set_name_hash: req.set_name_hash.clone(),
                    access: req.access.clone(),
                    home_nest_actor_id: req.home_nest_actor_id.clone(),
                    residency: req.residency.clone(),
                    extra: std::collections::BTreeMap::new(),
                }),
            );

            // Best-effort push for offline recipients (mirrors the HTTP twin).
            crate::push::dispatch_offline_push(
                &state,
                &recipient,
                "Group invite",
                "You have been invited to a group",
                "/app/groups",
            );

            // Auto-register the recipient on the channel so subsequent ciphertext
            // fetches resolve (mirrors the HTTP twin) — EXCEPT a claimed folder
            // channel. This is the cross-nest twin of the same-nest
            // `conversations_handlers::folder_channel_claim` gate: a folder share always KNOCKS
            // cross-nest (`shared_by = None`, manual accept via
            // `join_folder_welcome`), so a relayed Welcome must NOT pre-register
            // the recipient onto the owner-managed folder roster — else an evicted
            // member colluding with an open-federation peer nest re-inserts
            // themselves and re-surfaces the set's discovery metadata (F1/OBS-1,
            // `mls-group-key-material.md` § Rotate-on-removal). The claim is THIS
            // nest's authoritative, unforgeable record (the peer-supplied
            // `channel_type` is not trusted); accept is the sole cross-nest
            // roster-add. Unclaimed DM/group/scheduling channels register as before —
            // their cross-nest delivery depends on it. Fail-**closed** on a
            // claim-read error: only a provably-`Unclaimed` read registers.
            if let Some(ch_hex) = &req.channel_id
                && let Some(ch_id) = parse_32_bytes(ch_hex)
            {
                let claim =
                    crate::conversations_handlers::folder_channel_claim(&state.db, &ch_id).await;
                if claim == crate::conversations_handlers::FolderChannelClaim::Unclaimed
                    && let Err(e) = state.db.register_actor_channel(&recipient, &ch_id).await
                {
                    tracing::warn!("register_actor_channel on federation welcome: {e}");
                }
            }

            encode_reply(&FedWelcomeDeliverReply { inbox_id: row_id })
        })
    })
}

/// `fauna.federation.channel.fetch` — serve a channel's application-message log to
/// a member on a peer nest (the open-federation cross-nest message pull for
/// unpaired nests, `direct-messages.md` § Technical Flow — Cross-Nest, step 3).
///
/// **Authorization** (the structural defense, since a nest signature is
/// attribution not authorization — `federation.md` § Trust model): the requesting
/// actor MUST be a recorded foreign member of this channel (registered when this
/// nest relayed the Welcome to them) AND the verified `origin_nest_id` MUST be that
/// member's recorded home nest — so a hostile signer cannot harvest a channel it
/// does not host. Confidentiality is independently guaranteed by MLS — the relay
/// returns ciphertext it cannot read (`federation.md` § Security). Read-only;
/// reuses the same `segments::conv::read_after_seq` the same-nest
/// `fauna.conversations.channel.fetch` handler does, and the same
/// `conversations_handlers::page_verdicts` for what a community room's
/// labelers derived — filled for the requesting actor exactly when it is a
/// live floor member ([`FedChannelFetchMessage::labels`]).
fn channel_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedChannelFetchRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;

            // The shared structural gate — this call site predates
            // [`require_foreign_member`] and carried an inlined copy of the
            // same check, which silently skipped the first-use pin the gate
            // now stamps: the fetch is the very call a
            // recipient's first drain makes, so the duplicate would have left
            // most bindings permanently unconfirmed.
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;

            // The member's home nest announced its handle on this drain
            // (the id→handle ruling). Verified and recorded AFTER the gate —
            // an announce never seats anyone — and best-effort: nothing about
            // the fetch waits on a name.
            record_announced_handle(
                &state,
                &channel_id,
                &requester,
                &origin_nest_id,
                req.requesting_handle.as_deref(),
                req.requesting_domain.as_deref(),
            )
            .await;

            // `limit <= 0` = the full page, and the assembled reply is
            // byte-budgeted to the 2 MiB frame exactly like the same-nest
            // twin (`conversations_handlers::channel_fetch_handler`) — close
            // early, never skip.
            let limit = crate::segments::effective_fetch_limit(req.limit);
            let rows = crate::segments::conv::read_after_seq(
                &state.conv_segments,
                &state.db,
                &channel_id,
                req.after,
                limit,
            )
            .await
            .map_err(internal)?;
            let (rows, rest) =
                crate::segments::take_page_within_budget(rows, |(_, envelope, _)| envelope.len());
            if rows.is_empty()
                && let Some((seq, envelope, _)) = rest.first()
            {
                tracing::error!(
                    channel = %hex::encode(channel_id),
                    seq,
                    record_bytes = envelope.len(),
                    "federation conv fetch: a single stored record exceeds \
                     the WS frame budget — the foreign member's drain cannot \
                     advance past it (transport.md § Max frame)"
                );
            }
            // A community room's verdicts, for the member this drain names —
            // the gate the same-nest read passes its own caller through, keyed
            // on the actor `require_foreign_member` just bound to the calling
            // nest, never on that nest (`community-rooms.md` § The three
            // classes → *What the home nest does with its read*, purpose 2).
            let verdicts = crate::conversations_handlers::page_verdicts(
                &state,
                &channel_id,
                &requester,
                &rows,
            )
            .await;
            let mut authors =
                crate::conversations_handlers::page_authors(&state, &channel_id, req.after, &rows)
                    .await;
            let messages = rows
                .into_iter()
                .zip(verdicts)
                .map(
                    |((seq, envelope, legal_ref), derived)| FedChannelFetchMessage {
                        seq,
                        envelope,
                        legal_takedown: LegalTakedownMarker::from_ref(legal_ref),
                        labels: derived.labels,
                        scores: derived.scores,
                        author: authors.remove(&seq),
                    },
                )
                .collect();
            encode_reply(&FedChannelFetchReply { messages })
        })
    })
}

/// `fauna.federation.channel.actors` — serve a channel's authoritative roster
/// union to a member on a peer nest (the roster-read twin of the `channel.fetch`
/// relay; `federation.md` § Cross-nest). This is the add-participant heal's
/// discriminator for a member whose channel is foreign-homed
/// (`mls-group-key-material.md` § M2 *Admitting a member*, chat bullet).
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim — **and** [`refuse_removed_room_member`] beside it, since this is
/// one of the three doors that read the binding *alone* and so must close with
/// an end-to-end room's removal too (that helper owns the reasoning; a folder
/// channel and a newest-seated member are both unaffected). **Strictly
/// read-only**: unlike the same-nest `channel.fetch`/`send` paths, no hop of
/// the roster read auto-registers its reader — a read that wrote a row would
/// make every phantom look healthy to the next caller. The reply carries hex
/// actor ids only, never nest URLs ([`FedChannelActorsReply`]'s disclosure
/// rule).
fn channel_actors_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedChannelActorsRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            refuse_removed_room_member(&state, &channel_id, &requester).await?;
            let actors = state
                .db
                .list_channel_actors_union(&channel_id)
                .await
                .map_err(internal)?;
            encode_reply(&FedChannelActorsReply {
                actors: actors.iter().map(hex::encode).collect(),
            })
        })
    })
}

/// `fauna.federation.conversation.generations.fetch` — serve a **community
/// room's** foreign member its own generation wraps, so that a member homed
/// elsewhere can open the room's sealed log
/// (`../behavior/conversation-rooms.md` § The home nest).
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim (a room id *is* its channel id), so the requesting actor must be
/// a recorded foreign member of this channel whose recorded home nest is the
/// verified `origin_nest_id`.
///
/// **What the gate does not do, and what does.** The federation gate binds
/// *which nest may ask for whom*; it says nothing about which wraps a member
/// may have. That second question is the room plane's, and it is answered in
/// exactly one place —
/// [`conversations_handlers::room_generations_relayed`], the same resolution
/// the same-nest door uses — which derives the roster entry from the
/// requesting actor. The request carries no entry id, so a relaying nest
/// cannot ask for a wrap it is not the recipient of, its own room-read entry
/// included ([`FedRoomGenerationsRequest`]).
///
/// **Read-only on every hop**, like the roster read beside it: serving a wrap
/// registers nothing and mints nothing.
fn room_generations_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomGenerationsRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            let generations = crate::conversations_handlers::room_generations_relayed(
                &state, &room_id, &requester,
            )
            .await?;
            encode_reply(&FedRoomGenerationsReply { generations })
        })
    })
}

/// `fauna.federation.conversation.roster.fetch` — serve a room's foreign
/// member the room's **floor roster**, so that a member homed elsewhere is
/// shown the same names a member homed here is shown
/// (`../behavior/conversation-rooms.md` § The home nest).
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim (a room id *is* its channel id), so the requesting actor must be
/// a recorded foreign member of this channel whose recorded home nest is the
/// verified `origin_nest_id`.
///
/// **Why that gate alone, and deliberately not `is_room_member` on top.** The
/// same-nest door gates on the floor roster because that is the only record it
/// has of a local member. Here this nest holds a stronger one: the
/// `channel_foreign_members` row is its *own* record of the Welcome it relayed
/// and the binding it pinned — admission itself, not a mirror of it. The floor
/// roster of an end-to-end room is a member-*reported* mirror
/// (`conversation-rooms.md` § The floor roster) that routinely lags a
/// membership commit, so stacking it on would deny the read to the
/// newest-seated member — precisely the member whose co-members are still
/// nameless, and the case this whole path exists for. Nothing is disclosed by
/// the choice: an admitted channel member already reads every member's actor
/// id off the MLS ratchet tree, and what it gains here are the names its own
/// device will render.
///
/// **`admitted` is a live fact, not a historical one** — which is what makes
/// the binding-only gate safe rather than merely convenient. It is kept live
/// by a different ceremony per class, and both are needed for the sentence to
/// hold: a **community** room's `room.remove` purges the binding along with
/// the seat (`conversations_handlers::room_remove_handler`, S8: the fetch
/// authorization dies with the membership), while an **end-to-end** room's
/// removal reaches this nest as a roster report, and there
/// [`refuse_removed_room_member`] below the gate is what closes the door — on
/// the floor's own `removed_at` verdict, the binding left standing so a later
/// honest report can return the seat and the reach together. So this door
/// closes with the membership on both classes, as do the two beside it that
/// read the same row alone — the write-token mint and `channel.actors`. Before
/// the purge landed, a removed foreign member kept being served the room's
/// live floor here; before the floor read joined it, a removed member of an
/// **end-to-end** room still was.
///
/// **Read-only on every hop**, like the generation read beside it: serving a
/// roster registers nothing and mints nothing.
fn room_roster_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomRosterRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            refuse_removed_room_member(&state, &room_id, &requester).await?;
            let roster = crate::conversations_handlers::room_roster_reply(
                &state,
                &room_id,
                req.at_policy_version,
            )
            .await?;
            encode_reply(&FedRoomRosterReply { roster })
        })
    })
}

/// `fauna.federation.conversation.room_labels.fetch` — serve a room's foreign
/// member what the room's named labelers derived for **room-restricted
/// posts**, so that a member homed elsewhere sees the same badge a member
/// homed here sees (`../ui/feed.md` § Encryption at rest → *Room-restricted —
/// the ruling*, *Built* detail (v)).
///
/// **Why the post read needs a twin where the message read did not.** A room
/// message's verdicts ride `channel.fetch`'s page, which already relays. A
/// room post is its author's ordinary post: it reaches every follower through
/// `fauna.posts.get`, the feed pages and the deep-link door, none of them
/// floor-gated because the bytes are sealed — so its verdicts, derived from
/// the plaintext, take a post-scoped door of their own that no envelope read
/// carries. That door resolved each post's room from the serving nest's own
/// reception-pass map, which only the room's home nest writes
/// (`room_post_view::index_room_post` runs on the nest that *stores* the
/// post), so a foreign member's own nest resolved nothing and answered empty —
/// and empty is deliberately indistinguishable from "nobody labelled this
/// post", so the card lost its badge silently.
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim (a room id *is* its channel id), so the requesting actor must be a
/// recorded foreign member of this channel whose recorded home nest is the
/// verified `origin_nest_id`.
///
/// **And the floor on top of it, unlike [`room_roster_fetch_handler`]** — not
/// a new decision but the verdict plane's standing one. The gate binds *which
/// nest may ask for whom*; what a member may *read* is the floor's question,
/// and `conversations_handlers::page_verdicts` is already "the one home for
/// both rules, shared by the two doors a page is served through", so the
/// **message** verdict read across this very relay stacks
/// `is_live_floor_member` too. The roster read's ratified refusal to stack a
/// seat check cannot apply here: it feared denying the newest-seated member
/// off a member-*reported* mirror, and `is_live_floor_member` refuses any room
/// that is not floor-authoritative outright (`db::rooms::RoomRecord::
/// is_floor_authoritative` — "the nest's own ceremonies write its roster"), so
/// the only rooms it passes are ones whose floor this nest writes itself. A
/// member seated a moment ago is seated in *this* nest's own record.
///
/// **Read-only on every hop**, like the two reads beside it: serving a verdict
/// registers nothing and mints nothing.
fn room_labels_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomLabelsRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            let labels = crate::posts_handlers::room_labels_for_relay(
                &state,
                &requester,
                &req.post_ids,
                room_id,
            )
            .await?;
            encode_reply(&FedRoomLabelsReply { labels })
        })
    })
}

/// `fauna.federation.conversation.roster.report` — apply a foreign member's
/// **floor-roster report** to a room homed here, so that a membership commit
/// authored on the member's side of the federation lands on the floor the home
/// decides routing, custody and succession from
/// (`../behavior/conversation-rooms.md` § The floor roster). The write twin of
/// [`room_roster_fetch_handler`].
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim, standing IN PLACE OF the same-nest door's routing-roster gate 1
/// and strictly stronger than it: the `channel_foreign_members` row is this
/// nest's own record of the Welcome it relayed and the binding it pinned,
/// where a routing row is `channel.send`-self-registered and proves only
/// knowledge of the channel id. Everything past gate 1 — the roster
/// validation, gate 0's provenance refusal of a floor-authoritative room, the
/// gate 2 ratchet against the STORED roster, the commit-order guard, the
/// wholesale replace — is
/// [`crate::conversations_handlers::room_roster_report_apply`], the body the
/// same-nest door runs, so the two doors cannot drift. In particular the
/// bootstrap bound (`conversation-rooms.md` § Implementation status today)
/// does not widen here: a relayed FIRST report is admissible only from a
/// Welcome-bound member naming itself, never from any routing-roster actor.
///
/// Replay-safe: a wholesale replace with the same roster is a no-op, and
/// nothing is fanned out.
fn room_roster_report_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomRosterReportRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            let ack = crate::conversations_handlers::room_roster_report_apply(
                &state,
                &room_id,
                &requester,
                &req.members,
                req.policy_version,
                req.commit_seq,
            )
            .await?;
            encode_reply(&FedRoomRosterReportReply { ack })
        })
    })
}

/// `fauna.federation.conversation.room.leave` — retire a foreign member's own
/// seat on a room homed here, so that a departure decided on the member's side
/// of the federation lands on the floor the home decides routing, custody,
/// keying and succession from (`../behavior/conversation-rooms.md` § Roles and
/// authorization; § The home nest). The self-scoped twin of
/// [`room_roster_report_handler`].
///
/// **Why it is not [`channel_leave_handler`].** That door is generic and
/// deliberately so, and its single write is the `channel_foreign_members`
/// delete — which on a room is *half* a departure: the seat stays, the floor
/// never converges, and every later generation mint is obliged to wrap to the
/// departed seat's reception key rather than merely permitted to. It is
/// hardened to run this body too when the channel it names is a room, so the
/// two doors converge to one state; this one exists because a leave needs a
/// client leg of its own, and an additive field on the same-nest `room.leave`
/// would degrade silently on an older own-nest
/// (`fauna_protocol::conversations::RoomLeaveRemoteRequest`).
///
/// **Authorization**: [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim, and self-scoped past it: the body retires the *requesting* actor's
/// seat and nothing else, so a peer nest can end only its own members'
/// membership. Everything past the gate is
/// [`crate::conversations_handlers::room_leave_apply`], the body the same-nest
/// door runs — the owner's refusal, then on a floor-authoritative room the
/// binding purge, then the unseat, in that order — so the two doors cannot
/// drift. An end-to-end room's departure moves the seat alone and keeps the
/// binding, which is also what makes this door's retry converge on that class
/// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
/// mechanism*).
///
/// **Idempotent**, as the same-nest door is for a caller its floor stamped
/// departed. A caller already off the floor is answered with the live count
/// rather than a refusal: the postcondition holds, and this is the door a §4.D re-send arrives at, so a
/// refusal here would report a departure that landed as a failure that never
/// happened. The gate still fails loud for a member this nest never bound.
fn room_leave_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomLeaveRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            let outcome =
                crate::conversations_handlers::room_leave_apply(&state, &room_id, &requester)
                    .await?;
            encode_reply(&FedRoomLeaveReply {
                ack: fauna_protocol::conversations::RoomLeaveReply {
                    members: outcome.members,
                    extra: std::collections::BTreeMap::new(),
                },
            })
        })
    })
}

/// The home URL a relayed delivery binds to its **verified** origin — the
/// routing datum the recipient's client addresses its next hop back to the
/// delivering nest by (a Welcome's `nest_url`, a room invitation's
/// `room_node`), resolved from the connection's handshake-verified
/// `origin_nest_id` and NEVER taken from the per-request, peer-declared
/// address alone.
///
/// Prefer a dial-**proven** address of the verified origin: an attacker
/// cannot reach proven status without the honest nest's key. The declared URL
/// is honoured only when it is itself one of the origin's proven addresses (a
/// genuine migration between two proven addresses — the origin's current
/// self-claim wins among its own), and when the origin has NO proven address
/// yet (first cross-nest contact, where distrusting the declaration would
/// refuse every honest first invite). Either way the declaration is also
/// recorded as an UNPROVEN sighting bound to the verified identity, so a later
/// handshake can promote it and the directory's addresses-only-join invariant
/// is preserved (`db/channels.rs` § record_nest_address). With no declaration
/// at all a proven address still serves; `None` only when the origin declared
/// nothing and has never been dialled — for a Welcome that is the same-nest
/// relay, for a room invitation it is a delivery the recipient could never
/// act on, which the invite door refuses.
///
/// One home for the rule, shared by the Welcome relay and the room-invite
/// relay, so the two deliveries cannot drift on whose word a home URL is.
pub(crate) async fn resolve_origin_home_url(
    state: &AppState,
    origin_nest_id: &[u8; 32],
    declared: Option<&str>,
) -> Result<Option<String>, RpcError> {
    let declared = declared
        .map(|u| u.trim_end_matches('/'))
        .filter(|u| !u.is_empty());
    let proven = state
        .db
        .proven_foreign_nest_urls(origin_nest_id)
        .await
        .map_err(internal)?;
    if let Some(declared) = declared {
        // Record the declaration as an unproven sighting of the verified
        // origin (best-effort — a bookkeeping failure must not fail an
        // otherwise-good delivery, and the resolved URL below is what routes
        // this delivery regardless).
        if let Err(e) = state
            .db
            .record_nest_address(origin_nest_id, declared, false)
            .await
        {
            tracing::warn!("record relay origin sighting: {e}");
        }
        if proven.iter().any(|u| u == declared) {
            return Ok(Some(declared.to_string()));
        }
        if let Some(first_proven) = proven.into_iter().next() {
            return Ok(Some(first_proven));
        }
        return Ok(Some(declared.to_string()));
    }
    Ok(proven.into_iter().next())
}

/// `fauna.federation.conversation.room.invite` — a room's home nest delivers
/// a community-room invitation to an invitee homed here
/// (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*). The invitee's nest's half of that ruling, and
/// **it decides nothing about the room**: whether the inviter may invite and
/// whether the invitee may be seated are the home's questions, judged there
/// when the invitation was issued and again when it is accepted. What this
/// nest judges is its OWN member's side — that the record is genuine and
/// that this member is reachable by that inviter — and what it stores is one
/// inbox envelope, no room record and no invitation row.
///
/// In order:
///
/// 1. **A real local recipient** (F10, the Welcome relay's gate): a genuine
///    invitee has an account here.
/// 2. **The signed act verifies, and names this recipient.** The record is
///    what crossed the boundary, and the signature is why the crossing need
///    not be trusted (`RoomInviteInbox`); an invitation naming somebody else
///    is malformed, not misrouted.
/// 3. **The invitee's own reach policy, in full.** Unlike a Welcome, whose
///    wire carries no signed sender and so takes the unauthenticated floor,
///    this wire carries the inviter's signature — so the same gate the
///    same-nest invite door runs, with the inviter as the caller, runs here
///    at `Federation` origin: the floor's `federation_contact` pillar, the
///    invitee's inbox mode against their contact edge with the inviter. "The
///    group-Welcome gate applies unchanged" is thereby met by the stronger
///    form the signed sender makes possible.
/// 4. **Bind the room's home** to the connection's verified identity
///    ([`resolve_origin_home_url`]) — the `room_node` the invitee's
///    acceptance is relayed to. No resolvable home means an invitation the
///    invitee could never act on, so it is refused rather than delivered.
/// 5. **Deliver the knock** under the invitee's inbox quota, and nudge the
///    device, exactly as the same-nest invite door does.
fn room_invite_deliver_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomInviteRequest = decode(&payload)?;
            let recipient = parse_actor(&req.recipient_actor_id)?;
            if !state
                .db
                .is_actor_registered(&recipient)
                .await
                .map_err(internal)?
            {
                return Err(not_found("recipient not found"));
            }

            let signed: fauna_mls::room_policy::SignedRoomInvite =
                decode_strict(&req.signed_invite)
                    .map_err(|e| invalid_request(format!("malformed room invite: {e}")))?;
            signed
                .verify_signature()
                .map_err(|e| invalid_request(format!("room invite does not verify: {e}")))?;
            if signed.invite.invitee.0 != recipient {
                return Err(invalid_request(
                    "the invitation names a different invitee than the recipient",
                ));
            }
            let inviter = signed.inviter.0;
            if inviter == recipient {
                return Err(invalid_request("an inviter does not invite itself"));
            }

            crate::conversations_handlers::conversation_initiation_reach_gate(
                &state,
                &recipient,
                &inviter,
                true,
                ArrivalOrigin::Federation,
            )
            .await?;

            let Some(room_node) =
                resolve_origin_home_url(&state, &origin_nest_id, req.origin_nest_url.as_deref())
                    .await?
            else {
                return Err(invalid_request(
                    "the delivering nest declared no address an acceptance could be relayed to",
                ));
            };

            let inbox_bytes = fauna_protocol::inbox::InboxEnvelope::room_invite(
                &fauna_protocol::inbox::RoomInviteInbox {
                    signed_invite: req.signed_invite.clone(),
                    room_node: Some(room_node),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .and_then(|env| env.to_canonical_bytes())
            .map_err(internal)?;

            // The invitee's inbox tier quota, leg-parity with the Welcome
            // relay and the same-nest invite door: a knock flood cannot grow
            // the inbox past the tier limit.
            let inbox_id = if *state.enforce_tier_quotas.read().await {
                if let Err(e) = state.db.check_quota(&recipient, inbox_bytes.len()).await {
                    return Err(forbidden(format!("inbox quota: {e}")));
                }
                state
                    .db
                    .push_inbox_with_quota(&recipient, &inbox_bytes, None)
                    .await
                    .map_err(internal)?
            } else {
                state
                    .db
                    .push_inbox(&recipient, &inbox_bytes, None)
                    .await
                    .map_err(internal)?
            };

            crate::push::dispatch_offline_push(
                &state,
                &recipient,
                "Room invite",
                "You have been invited to a room",
                "/app/conversations",
            );

            encode_reply(&FedRoomInviteReply { inbox_id })
        })
    })
}

/// `fauna.federation.conversation.room.accept` — seat a foreign invitee on a
/// room homed here, relayed by the invitee's own nest
/// (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*). The seating twin of [`room_leave_handler`].
///
/// **Authorization is the invitation, not [`require_foreign_member`]** — the
/// one relayed room door whose gate is not the binding, because the binding
/// is what this door WRITES: the invitation the home recorded must name the
/// verified `origin_nest_id` as the invitee's home, so only the nest an
/// invitation was delivered to can seat that invitee, and self-scoped past it
/// (the requester is the only principal seated). Everything behind the gate —
/// the standing-offer judgement, the compare-and-swap seating, the
/// `InsertOnly` binding, the idempotent re-send arm — is
/// [`crate::conversations_handlers::room_accept_relayed`], which runs the
/// same-nest door's body for the seating, so the two doors cannot drift.
fn room_accept_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomAcceptRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let room_id = parse_id32(&req.room_id, "room_id")?;
            let ack = crate::conversations_handlers::room_accept_relayed(
                &state,
                &room_id,
                &requester,
                &origin_nest_id,
                &req.reception_pubkey,
            )
            .await?;
            encode_reply(&FedRoomAcceptReply { ack })
        })
    })
}

/// `fauna.federation.conversation.room.invite_issue` — a room's foreign
/// member issues an invitation into a room homed here, relayed by its own
/// nest (`../behavior/conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*, the foreign-inviter leg). The issuing twin of
/// [`room_accept_handler`], and the relayed door that gives a member homed
/// elsewhere the *invite* that `member-invite` grants every member.
///
/// **Authorization**: the signed act's signer must be the requesting actor
/// (`conversations_handlers::verify_room_invite_act` — the same-nest door's
/// own first act, binding the record to the member the relaying nest
/// authenticated), then [`require_foreign_member`] — the `channel.fetch` gate
/// verbatim: that actor holds a live binding on this room from the verified
/// origin. A removed member's binding is purged with its seat on this class,
/// and the body's own seat read refuses whoever the purge missed, so an
/// ex-member issues nothing. Everything past the gate is
/// [`crate::conversations_handlers::room_invite_apply`], the body the
/// same-nest door runs — the join-rule judgement, the already-a-member
/// refusal, and the delivery on one of its three arms — so the two doors
/// cannot drift, and a foreign member under `member-invite` invites exactly
/// what a member homed here may.
///
/// **The three deliveries.** An invitee homed on the room's home is served
/// by the same-nest arm, under the invitee's reach policy against a
/// federation-origin inviter; an invitee on a third nest by the same push
/// `room.invite` uses; and an invitee on the INVITER's nest — the common case,
/// and the one `invitee_node` leaves empty — by that push too, aimed at the
/// relaying nest's own address, resolved from its verified identity
/// ([`resolve_origin_home_url`]) rather than from anything it declared. The
/// delivery's dial then records the invitee's home as that identity, which is
/// what the invitee's relayed accept is later gated on.
///
/// **Not idempotent** (`forbid_replay=true`): a delivery per call.
fn room_invite_issue_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedRoomInviteIssueRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let (signed, room_id) = crate::conversations_handlers::verify_room_invite_act(
                &req.signed_invite,
                &requester,
            )?;
            require_foreign_member(&state, &room_id, &requester, &origin_nest_id).await?;
            let invitee_node = match req.invitee_node.trim() {
                "" => resolve_origin_home_url(
                    &state,
                    &origin_nest_id,
                    req.origin_nest_url.as_deref(),
                )
                .await?
                .ok_or_else(|| {
                    invalid_request(
                        "the relaying nest declared no address the invitation could be delivered to",
                    )
                })?,
                node => node.to_string(),
            };
            let ack = crate::conversations_handlers::room_invite_apply(
                &state,
                &room_id,
                &requester,
                &signed,
                &req.signed_invite,
                &invitee_node,
                ArrivalOrigin::Federation,
            )
            .await?;
            encode_reply(&FedRoomInviteIssueReply { ack })
        })
    })
}

/// Map a transport-independent [`InboxRejection`] (from the shared inbox core)
/// onto the federation error space — a pure namespace/code-family adapter over
/// [`InboxRejection::disposition`], the single shared statement of which
/// bucket each variant falls into (mirrored by
/// `inbox_handlers::rejection_to_error`). `Forbidden` takes the
/// **federation**-namespaced `forbidden` code; `MalformedClass` takes
/// `invalid_request` (wire code `fauna.federation.invalid_params` — there is
/// no federation `conflict`/`payload_too_large`, so those fold in here per the
/// `rsvp_deliver` precedent); `Internal` is the shared, un-namespaced
/// `fauna.protocol.internal`, identical to the client leg.
fn map_inbox_rejection(r: InboxRejection) -> RpcError {
    use InboxRejectionDisposition::{Capacity, Forbidden, Internal, MalformedClass};
    match r.disposition() {
        Forbidden(m) => forbidden(m),
        MalformedClass(m) => invalid_request(m),
        Capacity => crate::rpc_errors::rate_limited(),
        Internal(m) => internal(m),
    }
}

/// `fauna.federation.inbox.deliver` — a peer nest delivers a Fauna-native signed
/// `(ContactRequest, Post)` inbox payload to a local recipient. Runs the **same**
/// `routes::deliver_inbox_payload_core` (decode + verify + `InboxMode` routing +
/// `deliver_to_inbox`/`store_knock`) the HTTP twin `POST /api/v1/inbox/{actor}`
/// runs — the channel is now the carrier for cross-nest social inbox delivery.
fn inbox_deliver_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedInboxDeliverRequest = decode(&payload)?;
            let recipient = parse_actor(&req.recipient_actor_id)?;
            let body = Bytes::from(req.payload_bytes);
            match deliver_inbox_payload_core(&state, &recipient, &body, ArrivalOrigin::Federation)
                .await
            {
                InboxDeliveryOutcome::Delivered(row_id) => encode_reply(&FedInboxDeliverReply {
                    inbox_id: Some(row_id),
                }),
                InboxDeliveryOutcome::KnockStored => {
                    encode_reply(&FedInboxDeliverReply { inbox_id: None })
                }
                InboxDeliveryOutcome::Rejected(r) => Err(map_inbox_rejection(r)),
            }
        })
    })
}

// ── cross-nest shared folders + channel append (Phase 2 — federation.md
// § Cross-nest shared folders + channel append, ratified 2026-07-18) ────────
//
// One structural gate for the whole family ([`require_foreign_member`]): serve
// iff `foreign_member_home_nest(channel_id, requester) == origin_nest_id` — the
// row THIS nest wrote at Welcome-relay time. The binding is inviter-asserted
// (TOFU, a named accepted premise): never derive quota, billing, or identity
// decisions from `home_nest_id` alone. Bulk bytes never ride this channel
// (`transport.md` § carve-out) — reads GET by ciphertext hash on the open bulk
// plane; these kinds carry metadata + sealed envelopes only.

/// The structural authz gate shared by every member-scoped cross-nest kind
/// (`channel.append`, `channel.leave`, `folder.changes.fetch`,
/// `folder.content_key.fetch` — the `channel.fetch` gate reused verbatim):
/// the requesting actor must be a recorded foreign member of the channel AND
/// the connection's verified `origin_nest_id` must be that member's recorded
/// home nest. Absent row and mismatched binding fold to one `forbidden` (no
/// membership oracle for a nest that isn't the member's home).
///
/// **First authenticated use PINS the binding**: the first success here stamps `confirmed_at`, and from then on the
/// standing-based rebind arm of `register_foreign_channel_member` refuses to
/// move the grant (`federation.md` § Cross-nest shared folders + channel
/// append, the TOFU bullet). This is the one seam every exercising call from
/// the bound nest flows through, which is what makes it the pin's honest
/// witness — an outbound Welcome-relay ack is contact with the
/// *inviter-asserted* nest and deliberately confirms nothing. Best-effort: a
/// failed stamp only keeps the healing window open, never refuses the serve.
async fn require_foreign_member(
    state: &AppState,
    channel_id: &[u8; 32],
    requester: &[u8; 32],
    origin_nest_id: &[u8; 32],
) -> Result<(), RpcError> {
    match state
        .db
        .foreign_member_binding(channel_id, requester)
        .await
        .map_err(internal)?
    {
        Some((home, confirmed)) if home == *origin_nest_id => {
            if !confirmed
                && let Err(e) = state
                    .db
                    .confirm_foreign_member(channel_id, requester, &home)
                    .await
            {
                tracing::warn!("confirm_foreign_member: {e}");
            }
            Ok(())
        }
        _ => Err(forbidden(
            "requesting actor is not a member of this channel from this nest",
        )),
    }
}

/// The **live-membership** half of a relayed ROOM door's gate, stacked on
/// [`require_foreign_member`]'s structural half: refuse a requester whose
/// floor row positively says the room removed it.
///
/// **Why anything is stacked on a gate ratified as binding-only.** A removal
/// has to end the *admission*, not merely the seat — S8, "the fetch
/// authorization must die with the membership" — and on a **community** room
/// it does: `room.remove` purges the `channel_foreign_members` row before it
/// unseats ([`crate::conversations_handlers::room_remove_handler`]). An
/// **end-to-end** room has no such door. Its membership authority is the MLS
/// group, so that handler refuses the class outright (`floor_authoritative_room`)
/// and a removal arrives instead as a membership commit plus the committing
/// device's roster report, whose absorb stamps `removed_at` on every live row
/// the new roster did not name ([`crate::db::rooms::CacheDb::replace_floor_roster`])
/// and purges no binding at all. So on that class the binding outlived the
/// seat: a removed member's home nest kept passing the gate above and kept
/// being served the room's **live** floor — every later join's
/// `handle@domain`, roles, the signed policy, the labeler set and each
/// member's reception key — which is exactly the read the same-nest twin
/// refuses as "membership is not public".
///
/// **Why a floor READ rather than the community class's purge.** Purging on
/// absorb would give both classes one rule, and it was weighed and rejected.
/// The report ratchet deliberately admits a departing member's final report
/// ([`crate::conversations_handlers::room_roster_report_apply`], gate 2:
/// refusing it would leave the departure invisible until some other member
/// happened to commit, with the custody door serving the leaver's grants
/// meanwhile) — so one live member's false report naming only itself would
/// sever **every** co-member's binding, and the Welcome relay's insert arm
/// holds `InsertOnly` power, so no later honest report can write back what it
/// destroyed. A member still inside the MLS group has no re-Welcome ceremony
/// to be re-admitted by, which makes that state client-causable and
/// unrecoverable by a client (`principles.md` § No client-causable
/// unrecoverable nest state). A refusal *derived* from the floor carries the
/// same denial and none of the destruction: the next honest report's upsert
/// clears `removed_at` and the member's relayed reach returns with its seat.
///
/// **Positively removed, never merely absent** — the asymmetry
/// [`crate::db::rooms::CacheDb::room_member_removed`] exists for. A requester
/// with no floor row is admitted, because that is the newest-seated member
/// whose co-members are still nameless, and because a folder channel holds no
/// `room_members` row at all — which is also why this sits at the three room
/// doors rather than inside `require_foreign_member`, shared with
/// `channel.append`, `channel.leave` and the folder plane.
///
/// **And only where the removal ceremony is a REPORT** — the check is skipped
/// on a floor-authoritative room, which makes it exactly complementary to the
/// purge rather than overlapping it: `floor_authoritative_room` serves only
/// the rooms whose floor this nest's own ceremonies write, this refuses only
/// the rooms whose floor a member reports, so every room has exactly one
/// severance mechanism and none has two. The skip is **required**, not an
/// optimization. On that class the binding IS admission and the seat is a
/// separate act, so a re-invite's Welcome relay re-inserts the purged binding
/// and the member is served while its old `removed_at` row — history the purge
/// already superseded — still stands; stacking a floor read there would
/// blacklist an actor the room chose to take back, which
/// `conformance_cross_nest_conversations_client.rs::a_removed_foreign_members_relayed_roster_read_and_mint_die_with_the_binding`'s
/// re-admission arm pins (it reddened on exactly this before the skip landed).
///
/// **Declared residue — the re-add window.** On the reporting class a member
/// re-added by a fresh membership commit is refused until that commit's roster
/// report lands, since the report is the only signal this nest has and the
/// binding's own stamps cannot tell a re-add from the removal that preceded it
/// (the binding was never purged on this class). The window is the one the
/// committing device owes a report for immediately, it is self-healing at the
/// next report from any device, and it fails CLOSED — a briefly elided roster
/// on the re-added member's own device, never a read for someone off the
/// floor — where admitting instead is the hole this helper exists to close.
/// It is the re-admission form of the lag the binding-only gate is ratified
/// against, narrowed from "every newest-seated member" to "a re-added one,
/// until the report it is already owed".
///
/// **The refusal is the gate above's, verbatim**, so the two halves are one
/// door to a caller: a nest that has lost its reach learns that it has, never
/// which half took it.
///
/// **Per-class asymmetry, declared.** This closes the three **plaintext**
/// doors that read the binding alone. It deliberately leaves `channel.fetch`'s
/// ratified ciphertext drain open on the end-to-end class, where the community
/// class's is now shut by the purge: an end-to-end removal is an MLS commit
/// that rotates the epoch, so what a removed member may still fetch it cannot
/// open, and that residue is the one its own retained keys already are
/// (`federation.md` § Federation residue surface, the *room roster read* row).
async fn refuse_removed_room_member(
    state: &AppState,
    room_id: &[u8; 32],
    requester: &[u8; 32],
) -> Result<(), RpcError> {
    // No room record at all: a folder channel, or a room this nest does not
    // home. Nothing to read a verdict from, and nothing this door owes.
    let Some(room) = state.db.get_room(room_id).await.map_err(internal)? else {
        return Ok(());
    };
    // The purge's class. Its floor carries removal history the purge already
    // superseded, so the binding is the only live fact here — see the doc.
    if room.is_floor_authoritative() {
        return Ok(());
    }
    if state
        .db
        .room_member_removed(room_id, requester)
        .await
        .map_err(internal)?
    {
        return Err(forbidden(
            "requesting actor is not a member of this channel from this nest",
        ));
    }
    Ok(())
}

/// RAII: retires the pool's in-flight-verification count
/// ([`FederationChannelPool::note_verification_finished`]) when a domain
/// binding ([`bind_domain_to_origin`]) ends, on every exit path — the count
/// [`start_domain_verification`] checks against
/// `MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS`.
pub(crate) struct VerificationInFlight(Arc<AppState>);

impl Drop for VerificationInFlight {
    fn drop(&mut self) {
        self.0.federation_pool.note_verification_finished();
    }
}

/// The owner label a set's HOME nest stamps on the two carriers it
/// originates — the cross-nest folder Welcome relay and the federated
/// content-key read reply (`federation.md` § Cross-nest shared folders +
/// channel append → *The cross-nest owner label*, *Carrier*): `owner`'s
/// handle off this nest's own `users` row and this nest's handle domain,
/// **both or neither** (a handle without a domain would cross a nest boundary
/// bare, which the receiving nest never forwards anyway). Never
/// client-asserted. Best-effort: a lookup error stamps nothing — a share or a
/// read never fails for a name.
pub(crate) async fn owner_label_stamp(
    state: &AppState,
    owner: &[u8; 32],
) -> (Option<String>, Option<String>) {
    let Some(domain) = state.handle_domain_if_set().filter(|d| !d.is_empty()) else {
        return (None, None);
    };
    match state.db.get_handle(owner).await {
        Ok(Some(handle)) if !handle.is_empty() => (Some(handle), Some(domain)),
        Ok(_) => (None, None),
        Err(e) => {
            tracing::warn!("owner label stamp: resolve the set owner's handle: {e}");
            (None, None)
        }
    }
}

/// The trimmed `(handle, domain)` a peer asserted for one of its own users,
/// or `None` when either half is absent, empty or malformed — the syntax half
/// of every domain-bound name a peer volunteers (the id→handle announce and
/// the cross-nest owner label alike): [`fauna_protocol::handle::validate_handle`]
/// and [`fauna_core::web::is_domain_authority_syntax`]. A malformed pair is
/// ignored, never an error the honest peer sees.
pub(crate) fn well_formed_asserted_name<'a>(
    handle: Option<&'a str>,
    domain: Option<&'a str>,
    origin_nest_id: &[u8; 32],
    context: &'static str,
) -> Option<(&'a str, &'a str)> {
    let (Some(handle), Some(domain)) = (
        handle.map(str::trim).filter(|h| !h.is_empty()),
        domain.map(str::trim).filter(|d| !d.is_empty()),
    ) else {
        return None;
    };
    if let Err(why) = fauna_protocol::handle::validate_handle(handle) {
        tracing::debug!(
            origin = %hex::encode(origin_nest_id),
            "{context}: asserted handle ignored ({why})"
        );
        return None;
    }
    if !fauna_core::web::is_domain_authority_syntax(domain) {
        tracing::debug!(
            origin = %hex::encode(origin_nest_id),
            "{context}: asserted domain ignored (invalid syntax)"
        );
        return None;
    }
    Some((handle, domain))
}

/// Admit one domain binding under the fleet-wide concurrency cap
/// (`MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS`, shared by every path that binds a
/// peer-asserted domain), or `None` past it. The returned guard holds the
/// in-flight count until dropped; take it before spawning, so a burst of
/// spawned bindings is counted before any of them runs.
pub(crate) fn start_domain_verification(
    state: &Arc<AppState>,
    origin_nest_id: &[u8; 32],
    context: &'static str,
) -> Option<VerificationInFlight> {
    if state.federation_pool.in_flight_verification_count()
        >= crate::federation_pool::MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS
    {
        tracing::debug!(
            origin = %hex::encode(origin_nest_id),
            "{context}: domain verification dropped (concurrency cap)"
        );
        return None;
    }
    state.federation_pool.note_verification_started();
    Some(VerificationInFlight(Arc::clone(state)))
}

/// What binding a peer-asserted domain to the asserting peer's verified key
/// found.
pub(crate) enum DomainBinding {
    /// The discovery chain from the domain landed on the asserting peer.
    Bound,
    /// It landed on ANOTHER nest — the assertion is refused (logged as the
    /// signal it is).
    Refused,
    /// The domain did not resolve (or a concurrent binding is resolving it —
    /// [`crate::federation_pool::PoolError::ResolveInFlight`]); nothing is
    /// known either way.
    Unresolved(crate::federation_pool::PoolError),
}

/// Bind `domain` to `origin_nest_id` **from the domain end** — the one
/// verification behind every domain-bound name a peer volunteers
/// (`federation.md` § Cross-nest shared folders + channel append, the
/// id→handle bullet and *The cross-nest owner label*'s trust rule): the
/// anonymous discovery chain [`FederationChannelPool::resolve_domain_nest_id`]
/// (cached per domain, singleflighted, negative-cached, under the resolver's
/// own dial timeout) must land on the authenticated origin. Any nest can
/// advertise any string; which domain is its own is not for it to say. The
/// caller holds a [`start_domain_verification`] guard for the duration.
pub(crate) async fn bind_domain_to_origin(
    state: &AppState,
    domain: &str,
    origin_nest_id: &[u8; 32],
    context: &'static str,
) -> DomainBinding {
    match state.federation_pool.resolve_domain_nest_id(domain).await {
        Ok(id) if id == *origin_nest_id => DomainBinding::Bound,
        Ok(id) => {
            tracing::warn!(
                origin = %hex::encode(origin_nest_id),
                resolved = %hex::encode(id),
                domain,
                "{context}: a nest named its user at a domain that resolves to another \
                 nest — refused"
            );
            DomainBinding::Refused
        }
        Err(e) => {
            if !matches!(e, crate::federation_pool::PoolError::ResolveInFlight(_)) {
                tracing::debug!(
                    origin = %hex::encode(origin_nest_id),
                    domain,
                    "{context}: asserted domain did not resolve ({e}) — not believed"
                );
            }
            DomainBinding::Unresolved(e)
        }
    }
}

/// The cross-nest owner label a relayed folder Welcome may carry to its
/// recipient — the receiving nest's half of *The cross-nest owner label*'s
/// trust rule (`federation.md` § Cross-nest shared folders + channel append):
/// the pair the origin stamped ([`FedWelcomeDeliverRequest::owner_handle`] /
/// `owner_domain`), forwarded only when its domain binds to the
/// handshake-verified `origin_nest_id`. The Welcome is a one-shot delivery and
/// the knock is where the name matters most, so the binding is AWAITED inline
/// — but under the shared cap, and any failure (past the cap, a slow or
/// unreachable domain, a mismatch) delivers the Welcome unstamped: a share is
/// never refused for a name.
pub(crate) async fn verified_owner_label(
    state: &Arc<AppState>,
    origin_nest_id: &[u8; 32],
    handle: Option<&str>,
    domain: Option<&str>,
) -> (Option<String>, Option<String>) {
    const CONTEXT: &str = "federation welcome relay: owner label";
    let Some((handle, domain)) = well_formed_asserted_name(handle, domain, origin_nest_id, CONTEXT)
    else {
        return (None, None);
    };
    let Some(_in_flight) = start_domain_verification(state, origin_nest_id, CONTEXT) else {
        return (None, None);
    };
    match bind_domain_to_origin(state, domain, origin_nest_id, CONTEXT).await {
        DomainBinding::Bound => (Some(handle.to_string()), Some(domain.to_string())),
        DomainBinding::Refused | DomainBinding::Unresolved(_) => (None, None),
    }
}

/// Record the `handle@domain` a foreign member's home nest announced on the
/// member's own relayed drain — the id→handle ruling (`federation.md`
/// § Cross-nest shared folders + channel append, the id→handle bullet, ratified
/// 2026-09-10).
///
/// **What is verified, and why it is enough.** The announcing nest is the
/// authenticated `origin_nest_id` and is the authority for handles at *its own*
/// domain — but which domain is its own is not for it to say: any nest can
/// advertise any string. So the asserted domain is bound to the asserting key
/// **from the domain end**, by the same anonymous discovery chain a client runs
/// to resolve `bob@domain` ([`FederationChannelPool::resolve_domain_nest_id`]:
/// `https://{domain}` → SRV/port → `fauna.nest.info` over authenticated TLS).
/// The pair is stored only when that chain lands on `origin_nest_id`. A nest
/// naming its member at a domain it does not serve is refused and logged — it
/// is a signal, not an error the honest drain should see.
///
/// **Off the fetch path.** The dedup decision
/// ([`FederationChannelPool::begin_verification`]) is the only part on the
/// request's own call stack; the resolve + DB write run in a spawned task
/// ([`AppState::spawn_scoped`]) the reply never awaits, so a slow or hostile
/// domain never delays the page read that triggered it.
///
/// **Bounded.** Identical announces are a no-op before any lookup
/// ([`FederationChannelPool::begin_verification`]), and the domain→key answer
/// is cached, so the steady state of a member's poll loop costs nothing. A
/// hostile peer buys at most one discovery **per distinct domain** per
/// `ANNOUNCE_VERIFY_TTL` window ([`FederationChannelPool::resolve_domain_nest_id`]'s
/// negative cache, itself capped at `MAX_DOMAIN_FAILURE_ENTRIES` regardless of
/// the window), never one per fetch — alternating between two non-resolving
/// domains does not defeat this, since the cache keys on the domain, not the
/// binding. Past the window the same assertion is re-verified too, so a
/// domain that was only transiently unreachable regains its member's name
/// rather than staying stuck on its first failure. Best-effort throughout:
/// the drain never waits on, or fails for, a name.
///
/// **Validated and rate-limited before anything is recorded.** The domain
/// must be a syntactically valid `host[:port]` authority
/// ([`fauna_core::web::is_domain_authority_syntax`]) or the announce is
/// ignored exactly like a malformed handle; total concurrently in-flight
/// verifications across every peer are capped at
/// `MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS`, checked **before**
/// `begin_verification` so a drop at the cap is never recorded as a
/// completed check — the identical assertion is free to try again on the
/// member's next drain rather than being suppressed for the rest of
/// `ANNOUNCE_VERIFY_TTL`. A domain already being resolved by a concurrent
/// binding short-circuits the same way (`PoolError::ResolveInFlight`), and
/// that binding's own now-uncheckable assertion is discarded
/// ([`FederationChannelPool::discard_verification`]) for the same reason
/// ().
///
/// **Never an identity input.** What lands is what `room.list_roster` shows
/// for the member; every membership decision still keys on the actor id, and
/// the row it lands on is the binding the gate just admitted — so an announce
/// for an actor that is not a recorded foreign member writes nothing.
async fn record_announced_handle(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    requester: &[u8; 32],
    origin_nest_id: &[u8; 32],
    handle: Option<&str>,
    domain: Option<&str>,
) {
    const CONTEXT: &str = "federation channel fetch: announce";
    let Some((handle, domain)) = well_formed_asserted_name(handle, domain, origin_nest_id, CONTEXT)
    else {
        return;
    };
    // The cap is checked BEFORE `begin_verification` records the assertion: a
    // drop here must never be mistaken for a completed check, or the identical
    // assertion would be suppressed for the rest of `ANNOUNCE_VERIFY_TTL`
    // instead of trying again on the member's next drain
    // (). A `None` from
    // `begin_verification` drops the guard, retiring the count it took.
    let Some(in_flight) = start_domain_verification(state, origin_nest_id, CONTEXT) else {
        return;
    };
    let Some(generation) = state
        .federation_pool
        .begin_verification(*channel_id, *requester, handle, domain)
        .await
    else {
        return;
    };

    let channel_id = *channel_id;
    let requester = *requester;
    let origin_nest_id = *origin_nest_id;
    let handle = handle.to_string();
    let domain = domain.to_string();
    let state = Arc::clone(state);
    let task_state = Arc::clone(&state);
    task_state.spawn_scoped(async move {
        let _in_flight = in_flight;
        match bind_domain_to_origin(&state, &domain, &origin_nest_id, CONTEXT).await {
            DomainBinding::Bound => {
                if !state
                    .federation_pool
                    .is_current_verification(channel_id, requester, generation)
                    .await
                {
                    // A newer assertion superseded this one while it was
                    // resolving — that one's own verification (in flight or
                    // already landed) speaks for the binding now.
                    return;
                }
                match state
                    .db
                    .record_foreign_member_handle(&channel_id, &requester, &handle, &domain)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => tracing::debug!(
                        "federation channel fetch: announce for an actor with no binding \
                         row — not recorded"
                    ),
                    Err(e) => tracing::warn!("record_foreign_member_handle: {e}"),
                }
            }
            DomainBinding::Refused => {}
            DomainBinding::Unresolved(crate::federation_pool::PoolError::ResolveInFlight(_)) => {
                // A concurrent binding is already resolving this exact
                // domain — this attempt made no network call and wrote
                // nothing to the negative cache. Discard our own recorded
                // assertion (if still current) so this binding's next
                // identical assertion gets a fresh attempt instead of being
                // suppressed for the rest of `ANNOUNCE_VERIFY_TTL` by a check
                // that never ran ().
                state
                    .federation_pool
                    .discard_verification(channel_id, requester, generation)
                    .await;
            }
            DomainBinding::Unresolved(_) => {}
        }
    });
}

#[cfg(test)]
mod announce_domain_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::record_announced_handle;
    use crate::db::CacheDb;
    use crate::federation_pool::MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS;
    use crate::routes::AppState;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test(Arc::new(
            CacheDb::open_in_memory().unwrap(),
        )))
    }

    /// A 300-byte domain — every byte a legal hostname character — must be
    /// ignored before it ever reaches `begin_verification`
    /// (); only the length ceiling
    /// catches it, so a validator built from the metacharacter check alone
    /// would let it through.
    #[tokio::test]
    async fn an_oversized_domain_is_ignored_before_any_lookup() {
        let state = test_state();
        let channel = [1u8; 32];
        let actor = [2u8; 32];
        let origin = [3u8; 32];
        let oversized = format!("{}.example", "a".repeat(300));

        record_announced_handle(
            &state,
            &channel,
            &actor,
            &origin,
            Some("bob"),
            Some(&oversized),
        )
        .await;

        assert!(
            !state
                .federation_pool
                .is_current_verification(channel, actor, 1)
                .await,
            "an oversized domain must never even start a verification"
        );
        assert_eq!(state.federation_pool.in_flight_verification_count(), 0);
    }

    /// The concurrency cap-drop happens BEFORE `begin_verification` records
    /// the assertion, so a capped announce is retried on the member's next
    /// drain rather than being suppressed for the rest of
    /// `ANNOUNCE_VERIFY_TTL` ().
    #[tokio::test]
    async fn a_concurrency_cap_drop_never_records_the_assertion() {
        let state = test_state();
        let channel = [4u8; 32];
        let actor = [5u8; 32];
        let origin = [6u8; 32];

        // Saturate the cap synthetically — no verification is actually
        // running, but the counter is all `record_announced_handle` reads.
        for _ in 0..MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS {
            state.federation_pool.note_verification_started();
        }
        record_announced_handle(
            &state,
            &channel,
            &actor,
            &origin,
            Some("bob"),
            Some("127.0.0.1:1"),
        )
        .await;
        assert!(
            !state
                .federation_pool
                .is_current_verification(channel, actor, 1)
                .await,
            "a cap-dropped announce must not be recorded as a completed check"
        );

        // Release the synthetic saturation: the identical assertion must be
        // free to try again immediately, proving it was never marked
        // "seen" while capped.
        for _ in 0..MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS {
            state.federation_pool.note_verification_finished();
        }
        record_announced_handle(
            &state,
            &channel,
            &actor,
            &origin,
            Some("bob"),
            Some("127.0.0.1:1"),
        )
        .await;
        assert!(
            state
                .federation_pool
                .is_current_verification(channel, actor, 1)
                .await,
            "once the cap clears, the same assertion must start its own \
             verification rather than staying suppressed"
        );
    }

    /// **The `ResolveInFlight` collision drives the handler's own
    /// `discard_verification` call, not just the pool method it wraps** —
    /// remove that call and this reds; the pool-level
    /// `discard_verification_only_removes_the_still_current_generation`
    /// exercises the method directly and can't catch a dropped call site
    /// (). Two distinct bindings
    /// assert the same never-resolving loopback domain concurrently — the
    /// same dead-loopback `tokio::join!` shape
    /// `federation_pool.rs::concurrent_resolves_of_the_same_domain_singleflight_to_one_attempt`
    /// uses — so exactly one collides into `PoolError::ResolveInFlight` and
    /// must discard its own recorded assertion.
    #[tokio::test]
    async fn a_singleflight_collision_discards_the_losers_recorded_assertion() {
        let state = test_state();
        let channel = [11u8; 32];
        let actor_a = [12u8; 32];
        let actor_b = [13u8; 32];
        let origin = [14u8; 32];
        let domain = "127.0.0.1:1";

        tokio::join!(
            record_announced_handle(
                &state,
                &channel,
                &actor_a,
                &origin,
                Some("alice"),
                Some(domain)
            ),
            record_announced_handle(
                &state,
                &channel,
                &actor_b,
                &origin,
                Some("bob"),
                Some(domain)
            ),
        );

        // Wait for every verification this spawned to finish — quiescence,
        // not a fixed sleep (convention 14).
        let quiesce_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while state.federation_pool.in_flight_verification_count() > 0 {
            assert!(
                tokio::time::Instant::now() < quiesce_deadline,
                "spawned verifications never quiesced"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let a_current = state
            .federation_pool
            .is_current_verification(channel, actor_a, 1)
            .await;
        let b_current = state
            .federation_pool
            .is_current_verification(channel, actor_b, 1)
            .await;
        assert_ne!(
            a_current, b_current,
            "exactly one of the two colliding bindings must have had its \
             generation-1 assertion discarded by the ResolveInFlight \
             collision: a={a_current} b={b_current}"
        );
        let (loser, handle) = if a_current {
            (actor_b, "bob")
        } else {
            (actor_a, "alice")
        };

        // The loser's discarded assertion must let its very next identical
        // announce start a fresh verification right away — `discard_verification`
        // REMOVES the map entry entirely, so the next `begin_verification`
        // restarts at generation 1 (not 2), rather than staying suppressed
        // for the rest of `ANNOUNCE_VERIFY_TTL` by a check that never ran.
        record_announced_handle(
            &state,
            &channel,
            &loser,
            &origin,
            Some(handle),
            Some(domain),
        )
        .await;
        assert!(
            state
                .federation_pool
                .is_current_verification(channel, loser, 1)
                .await,
            "the collision loser must be free to start a fresh verification \
             immediately, not stay suppressed"
        );
    }
}

/// Resolve a channel to the **claimed** folder bound to it: the claim
/// (`folder_channel_claimed_by`) names the owner, whose folder rows are
/// scanned for the one whose derived `ChannelId::from_group_id(mls_group_id)`
/// matches. Per S7 (the `welcome.deliver` precedent): the folder kinds read
/// THIS nest's claim state, never peer-asserted typing — an unclaimed
/// conversation channel is `not_found`, whatever the peer says it is.
pub(crate) async fn claimed_folder_for_channel(
    state: &AppState,
    channel_id: &[u8; 32],
) -> Result<crate::db::FolderRow, RpcError> {
    let claimant = state
        .db
        .folder_channel_claimed_by(channel_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("channel has no folder claim"))?;
    state
        .db
        .get_folders_for_actor_full(&claimant)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|fs| {
            fs.mls_group_id
                .as_deref()
                .is_some_and(|g| fauna_mls::types::ChannelId::from_group_id(g).0 == *channel_id)
        })
        .ok_or_else(|| not_found("no folder bound to this channel"))
}

/// `fauna.federation.channel.append` request — the mutating twin of
/// `channel.fetch`: a foreign member's send, relayed by their home nest into
/// this (the channel-home) nest's log. `envelope` is the opaque MLS
/// `ChannelEnvelope` bytes; `expect_no_commit_since` rides through to the
/// seq-locked append (the device-owned-epoch commit gate, same as the same-nest
/// `channel.send`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelAppendRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_no_commit_since: Option<i64>,
    /// The foreign member's plaintext attachment references, relayed verbatim
    /// from their `channel.send_remote` (`ChannelSendRequest::attachment_refs`
    /// — the conversation kind's blob-reachability floor). The home nest
    /// records them beside the appended record exactly as for a same-nest
    /// send. Additive: absent when the send has no attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<String>,
}

/// `fauna.federation.channel.append` reply — the appended record's channel seq.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelAppendReply {
    pub seq: i64,
}

/// `fauna.federation.channel.leave` request — the self-scoped delete of the
/// requester's own foreign-member row (their home nest relays their leave).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelLeaveRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.channel.leave` reply — whether a row was deleted
/// (`false` = an idempotent re-leave; both are success).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedChannelLeaveReply {
    pub removed: bool,
}

/// `fauna.federation.conversation.write_token.mint` request — a foreign
/// **conversation member** asks the room's home nest for a short-lived
/// byte-plane token so its client can POST a sealed attachment DIRECT to the
/// home nest's `POST /api/v1/blob` (bulk bytes never ride the federation
/// channel — `transport.md` § carve-out). The blob twin of `channel.append`:
/// the room's attachment bytes rest on the room's home nest, beside the record
/// and the plaintext `attachment_refs` that pin them
/// (`conversation-rooms.md` § The home nest → *Attachment bytes*, ratified
/// 2026-09-09).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedConversationWriteTokenMintRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.conversation.write_token.mint` reply — the opaque
/// bulk-write token + its absolute expiry (Unix seconds). The client POSTs
/// bytes to the home nest URL it already holds (the channel's recorded
/// Welcome `nest_url`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedConversationWriteTokenMintReply {
    pub token: String,
    pub expires_at: u64,
}

/// `fauna.federation.folder.changes.fetch` request — the cross-nest read of a
/// shared set's change log (manifest refs, sizes, versions, nest-stamped
/// authorship), relayed by the member's home nest. Same `after`/`limit` cursor
/// semantics as `channel.fetch` (`limit <= 0` = full page, clamped).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderChangesFetchRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
    pub after: i64,
    pub limit: i64,
}

/// `fauna.federation.folder.changes.fetch` reply — the same wire rows a
/// same-nest `fauna.sync.changes.list` serves (one `SyncChange` shape on both
/// planes; `seq > after`, oldest first, frame-budgeted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderChangesFetchReply {
    pub changes: Vec<fauna_protocol::sync::SyncChange>,
    /// The requester's **current** access grant on this set, stamped by this
    /// (the home) nest from its own `folder_member_access` row — the refresh
    /// half of *Recipient-side access discovery* (`federation.md` § Cross-nest).
    /// Free to compute: the handler already resolved membership for the gate.
    ///
    /// See [`caller_access_stamp`] for why **every** federated folder read
    /// reply carries this, and for the `None` semantics (no role row or a storage blip ⇒
    /// asserts nothing ⇒ the client keeps what it holds; never a revocation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// The folder's content residency — see [`residency_stamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// The page's `signer_certs` side table — the same one a same-nest
    /// `changes.list` carries (`SyncChangesListReply::signer_certs`), which the
    /// relaying nest passes through verbatim. Wire-additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_certs: Vec<fauna_core::encoding::EmbedAsBytes>,
}

/// `fauna.federation.folder.content_key.fetch` request — the cross-nest read
/// of the set's sealed content-key envelope (opaque to both nests; only group
/// membership opens it — `mls-group-key-material.md` § M2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderContentKeyFetchRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.folder.content_key.fetch` reply — epoch + hex-encoded
/// sealed envelope, exactly what the same-nest `content_key.get` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderContentKeyFetchReply {
    pub epoch: i64,
    pub sealed: String,
    /// The requester's current access grant — see [`caller_access_stamp`].
    /// This is the read a foreign member's client actually runs in production
    /// today (the custody-ingest commit poll), so it is the carrier that makes
    /// the refresh live rather than latent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// The home nest's own deployment `nest_actor_id` (hex 32-byte pubkey),
    /// stamped on every reply so the member's `home_nest_actor_id` refreshes
    /// alongside `caller_access` — the byte-plane SPKI-pin trust root
    /// (`security.md` § Transport trust, the federation-granted Axis-2 row).
    /// Belt-and-braces beside the Welcome seed (the identity is stable); `None`
    /// only if the home nest's signing key is unexpectedly absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    /// The set's owner-stamped content-key floor off this (the home) nest's
    /// envelope row — the same value `fauna.folders.list` projects as the row's
    /// `content_key_floor` — so the member's engine can hold its writes behind
    /// it locally instead of learning of a rotation only from this nest's
    /// `stale_content_key` refusal (`on-demand-files.md` § Shared sets on a
    /// capability host → *One mechanism*, question 2). Wire-additive: a
    /// home nest with no floor sends none and a member that does not read it ignores it.
    /// `None` ⇒ no floor established (or a read blip) ⇒ the member keeps what
    /// it holds — never a claim the floor was cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_floor: Option<u64>,
    /// The folder's content residency — see [`residency_stamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// The set's owner's handle + this (the home) nest's handle domain — see
    /// [`owner_label_stamp`]. The member's own nest forwards the pair only on
    /// a warm domain binding (`federation.md` § … *The cross-nest owner
    /// label*); both `None` ⇒ the member keeps what it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_domain: Option<String>,
}

/// `fauna.federation.folder.actors.fetch` request — the cross-nest writer
/// roster read (`federation.md` § Cross-nest…, *The cross-nest writer roster
/// read*): which actors hold `writer` on the set, for the writer-signed
/// change-record reader (`mls-group-key-material.md` § M2, ruling (3)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderActorsFetchRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.folder.actors.fetch` reply — the same projection the
/// same-nest `fauna.folders.members.list_actors` serves (one function behind
/// both doors), ids-only: every `handle` rides EMPTY across nests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderActorsFetchReply {
    pub members: Vec<fauna_protocol::folders::FolderActorMember>,
    /// The requester's current access grant — see [`caller_access_stamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// The folder's content residency — see [`residency_stamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
}

/// `fauna.federation.channel.append` — gate ([`require_foreign_member`]), then
/// run the **same** `channel_send_core` the same-nest `channel.send` runs (size
/// cap, folder-channel Commit admission + non-claimant rate cap, gated roster
/// auto-register, strict ingest, L2 anti-spam, seq-locked append, push
/// fan-out) with the foreign member as the acting actor. Consequences fall
/// out of that reuse by construction:
///
/// - **Folder-channel Commit admission (2026-08-24 re-ratification,
///   `federation.md` § Cross-nest shared folders + channel append —
///   supersedes the old S2 claimant-only refusal)**: a `Commit` is admitted
///   from any actor on the channel roster, a foreign member included — the
///   nest can see THAT an envelope is a Commit, never WHAT it changes, so
///   owner-only roster-change enforcement lives at each MEMBER's
///   `MlsEngine::process_commit`, not here. A foreign non-claimant's Commit
///   consumes the same per-(actor, channel) rate cap a same-nest member's
///   does (§ residual (a)) — `channel_send_core` is the shared
///   chokepoint, so the cap inherits with no extra wiring here. A
///   conversation channel's member Commit lands unthrottled (any member
///   commits a DM/group; the cap is folder-channel-only).
/// - An *Application* envelope on a claimed folder channel also fails (the
///   gated auto-register skips claimed channels, so the strict ingest's roster
///   check refuses the sender) — folder channels carry rotations, not chat,
///   and no cross-nest flow legitimately appends Applications to them.
///
/// The L2 behavioral anti-spam path keys on the acting actor and runs for the
/// foreign sender exactly as it would same-nest (`run_antispam = true`).
/// Redelivery: the per-connection L3 idempotency cache dedups a same-key retry;
/// past that cache the MLS consumers quiet-skip duplicate ciphertext — this
/// kind never promises nest-side exactly-once (S3).
fn channel_append_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedChannelAppendRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            let attachment_refs =
                crate::conversations_handlers::parse_attachment_refs(&req.attachment_refs)?;

            let reply = crate::conversations_handlers::channel_send_core(
                &state,
                &requester,
                &channel_id,
                Bytes::from(req.envelope),
                true,
                req.expect_no_commit_since,
                &attachment_refs,
            )
            .await?;
            encode_reply(&FedChannelAppendReply { seq: reply.seq })
        })
    })
}

/// `fauna.federation.channel.leave` — the self-scoped, idempotent delete of the
/// requester's own `channel_foreign_members` row. Deliberately generic (a
/// cross-nest conversation participant's leave is the same operation as a
/// folder member's). Revokes future federated discovery only — never key
/// material already held (parity with same-nest voluntary leave). An absent row
/// is an idempotent success; a row bound to a DIFFERENT home nest is refused
/// and left intact (only the member's own home nest leaves for them — under the
/// TOFU premise a misbound member cannot self-serve here, which the genuine
/// member's client sees as a loud error, never a false success).
///
/// **On a room, the binding delete is only half a departure, so this door runs
/// the room's own leave body instead.** A room's membership is its floor, not
/// its relay binding: dropping the binding alone leaves the seat, and a seat
/// nobody occupies is not inert — the roster-coverage gate *obliges* every
/// later generation mint to wrap to its reception key, and `room.invite`
/// refuses to re-admit a principal that is already a member, so the ghost both
/// keeps being handed key material and blocks the re-admission that would heal
/// it. Nothing of ours calls this kind with a room channel id (the conversations
/// plane has its own [`room_leave_handler`], and no app carries a conversation
/// self-leave gesture at all), but a peer nest can, and a door that converges
/// only when the caller picks the right kind is not converged. Delegating —
/// rather than bolting an unseat on here — is what keeps the two doors from
/// drifting on the owner refusal, the provenance refusal and the write order.
fn channel_leave_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedChannelLeaveRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;

            match state
                .db
                .foreign_member_home_nest(&channel_id, &requester)
                .await
                .map_err(internal)?
            {
                // Absent row → idempotent success, nothing to delete. No
                // unseat either, deliberately: with no binding this nest can
                // prove nothing about the caller's home, and unseating on an
                // unproven claim would hand any peer a removal door for any
                // principal on any room's floor.
                None => encode_reply(&FedChannelLeaveReply { removed: false }),
                Some(home) if home == origin_nest_id => {
                    // A room homed here: the departure is the room's, and
                    // `room_leave_apply` performs both halves in the ratified
                    // order (purge, then unseat). `removed` keeps naming the
                    // binding, which is this kind's own contract.
                    if state
                        .db
                        .get_room(&channel_id)
                        .await
                        .map_err(internal)?
                        .is_some_and(|room| room.is_floor_authoritative())
                    {
                        let outcome = crate::conversations_handlers::room_leave_apply(
                            &state,
                            &channel_id,
                            &requester,
                        )
                        .await?;
                        return encode_reply(&FedChannelLeaveReply {
                            removed: outcome.binding_purged,
                        });
                    }
                    let removed = state
                        .db
                        .remove_foreign_channel_member(&channel_id, &requester)
                        .await
                        .map_err(internal)?;
                    encode_reply(&FedChannelLeaveReply { removed })
                }
                Some(_) => Err(forbidden(
                    "requesting actor is not a member of this channel from this nest",
                )),
            }
        })
    })
}

/// `fauna.federation.folder.changes.fetch` — gate, resolve the claimed set
/// ([`claimed_folder_for_channel`], S7), then serve the same wire rows the
/// same-nest `changes.list` serves, cursor-paged (`seq > after`, clamped limit)
/// and byte-budgeted to the WS frame like `channel.fetch`.
fn folder_changes_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderChangesFetchRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            let fs = claimed_folder_for_channel(&state, &channel_id).await?;

            let limit = crate::segments::effective_fetch_limit(req.limit);
            let mut rows = state
                .db
                .get_sync_changes_for_folder(fs.id, req.after, None)
                .await
                .map_err(internal)?;
            rows.truncate(limit as usize);
            // Frame-budget the page exactly like `channel.fetch` — close early,
            // never skip (the member's drain walks a contiguous seq cursor).
            let (rows, rest) = crate::segments::take_page_within_budget(rows, |c| {
                c.wire_len() + crate::segments::RECORD_WIRE_OVERHEAD
            });
            if rows.is_empty()
                && let Some(head) = rest.first()
            {
                tracing::error!(
                    channel = %hex::encode(channel_id),
                    seq = head.seq,
                    "federation folder changes fetch: a single change row exceeds \
                     the WS frame budget — the foreign member's drain cannot advance"
                );
            }
            let signer_certs = crate::sync_handlers::signer_certs_for(&state.db, &rows).await?;
            encode_reply(&FedFolderChangesFetchReply {
                changes: rows
                    .iter()
                    .map(crate::sync_handlers::change_to_wire)
                    .collect(),
                caller_access: caller_access_stamp(&state, &channel_id, &requester).await,
                residency: residency_stamp(&fs),
                signer_certs,
            })
        })
    })
}

/// `fauna.federation.folder.public.fetch` request — the cross-nest read of a
/// **`public`-audience** folder's change log, relayed by the follower's own
/// nest (`folders.md` § Publicly-synced follow; `federation.md` § The public
/// folder read plane).
///
/// ⚠ **No `requesting_actor_id`, deliberately** — it is the one field every
/// other kind in this family carries, and its absence here is the design, not
/// an omission: there is no membership to check, so the follower's identity
/// never crosses the wire. The home nest sees the requesting *nest* and source
/// IP (its throttle keys) and nothing about which user follows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderPublicFetchRequest {
    /// Hex owner actor id, paired with [`Self::folder_name`] — the
    /// first-contact address. Ignored when [`Self::folder_id`] is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_actor_id: Option<String>,
    /// The folder's plaintext name (world-readable by the ratified public
    /// exception).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    /// This nest's stable `folders.id`, pinned by the follower's first read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<i64>,
    pub since: i64,
    pub limit: i64,
}

/// `fauna.federation.folder.public.fetch` reply — the folder's identity plus
/// one floor-filtered, stripped page (projection contract: `folders.md`
/// § Publicly-synced follow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderPublicFetchReply {
    pub folder_id: i64,
    pub name: String,
    /// This (the home) nest's deployment identity — the byte-plane SPKI-pin
    /// trust root the follower dials the open by-hash bulk plane under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    pub changes: Vec<fauna_protocol::sync::SyncChange>,
}

/// `fauna.federation.folder.public.fetch` — the public read plane's federation
/// leg. **No gate call here**: the whole authorization is
/// [`crate::folder_public::resolve_public_folder`]'s inverse-shaped one (*serve
/// iff the addressed row's current `audience == 'public'`*), which is what lets
/// this handler skip the `require_foreign_member` binding every other folder
/// kind runs — there is no membership to bind, and adding a caller identity
/// just to check nothing would put a follower's identity on the wire for no
/// gain.
///
/// **Writes nothing, charges nothing.** No follower row (not enumerable, not
/// floodable into disk) and no metering (reads are unmetered everywhere). The
/// abuse bound is the per-source-IP `/64` + per-nest throttle every
/// `fauna.federation.*` kind already rides (`serve_request` step 2.5).
fn folder_public_fetch_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderPublicFetchRequest = decode(&payload)?;
            let address = public_address_from_wire(
                req.folder_id,
                req.owner_actor_id.as_deref(),
                req.folder_name.as_deref(),
            )?;
            let (folder, grant) =
                crate::folder_public::resolve_public_folder(&state, &address).await?;
            let changes = crate::folder_public::public_changes_page(
                &state, &folder, grant, req.since, req.limit,
            )
            .await?;
            encode_reply(&FedFolderPublicFetchReply {
                folder_id: folder.id,
                name: folder.name,
                // This nest IS the folder's home (the row is local), so its own
                // deployment identity is the follower's byte-plane pin root.
                home_nest_actor_id: Some(hex::encode(state.nest_identity.public_key_bytes())),
                changes,
            })
        })
    })
}

/// Map a public-fetch request's addressing fields onto a
/// [`crate::folder_public::PublicFolderAddress`] — shared by the federation
/// handler above and the client twin, so the two planes can never disagree
/// about which address wins.
///
/// `folder_id` wins when present (the pinned form survives a rename); otherwise
/// the `(owner, name)` pair. A request naming neither is malformed — that is a
/// caller bug, not a folder that happens to be absent, so it does NOT fold into
/// the plane's uniform `not_found`.
pub(crate) fn public_address_from_wire(
    folder_id: Option<i64>,
    owner_actor_id: Option<&str>,
    folder_name: Option<&str>,
) -> Result<crate::folder_public::PublicFolderAddress, RpcError> {
    if let Some(id) = folder_id {
        return Ok(crate::folder_public::PublicFolderAddress::Id(id));
    }
    match (owner_actor_id, folder_name) {
        (Some(owner), Some(name)) => Ok(crate::folder_public::PublicFolderAddress::OwnerAndName(
            parse_actor(owner)?,
            name.to_string(),
        )),
        _ => Err(crate::rpc_errors::coded_ns(
            "folders",
            "invalid_request",
            "address a public folder by folder_id, or by owner_actor_id + folder_name",
        )),
    }
}

/// `fauna.federation.folder.content_key.fetch` — gate, resolve the claimed
/// set (S7), then return the sealed content-key envelope exactly as the
/// same-nest `content_key.get` does. The envelope is opaque ciphertext to both
/// nests; a member who was evicted before the current epoch holds keys that no
/// longer open it (rotate-on-removal), so serving the latest envelope to a
/// *rostered* foreign member leaks nothing beyond membership itself.
fn folder_content_key_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderContentKeyFetchRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            // The S7 claim check: an unclaimed channel refuses here.
            let fs = claimed_folder_for_channel(&state, &channel_id).await?;

            let (epoch, sealed) = state
                .db
                .get_folder_content_key(&channel_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("no content-key envelope published yet"))?;
            // The set's owner-stamped floor, the same value `fauna.folders.list`
            // projects — best-effort like the access stamp: a read blip stamps
            // nothing (the member keeps what it holds), never fails the fetch.
            let content_key_floor = crate::folder_handlers::content_key_floor_to_wire(
                state
                    .db
                    .get_folder_content_key_floor(&channel_id)
                    .await
                    .ok()
                    .flatten(),
            );
            let (owner_handle, owner_domain) = match <[u8; 32]>::try_from(fs.actor_id.as_slice()) {
                Ok(owner) => owner_label_stamp(&state, &owner).await,
                Err(_) => (None, None),
            };
            encode_reply(&FedFolderContentKeyFetchReply {
                epoch,
                sealed: hex::encode(&sealed),
                caller_access: caller_access_stamp(&state, &channel_id, &requester).await,
                // This nest IS the set's home (S7 claim just passed), so its own
                // deployment identity is the byte-plane SPKI-pin trust root the
                // member's agent graduates against (`security.md` § Transport trust).
                home_nest_actor_id: Some(hex::encode(state.nest_identity.public_key_bytes())),
                content_key_floor,
                residency: residency_stamp(&fs),
                owner_handle,
                owner_domain,
            })
        })
    })
}

/// `fauna.federation.folder.actors.fetch` — the cross-nest writer roster read
/// (`federation.md` § Cross-nest…, *The cross-nest writer roster read*): the
/// family's structural gate, the S7 claim-resolve, then the **same** projection
/// the same-nest `members.list_actors` serves
/// ([`crate::folder_handlers::actor_roster_for_channel`]) with every `handle`
/// blanked — ids, roles and grants only across nests (id→handle is the
/// announce's direction, never a nest's answer to another) — plus the
/// `caller_access` stamp. Read-only on every hop: registers nothing, mints
/// nothing.
fn folder_actors_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderActorsFetchRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            let fs = claimed_folder_for_channel(&state, &channel_id).await?;
            let owner = <[u8; 32]>::try_from(fs.actor_id.as_slice())
                .map_err(|_| internal("folder owner id is not 32 bytes"))?;
            let mut members =
                crate::folder_handlers::actor_roster_for_channel(&state.db, &channel_id, &owner)
                    .await
                    .map_err(internal)?;
            for m in &mut members {
                m.handle.clear();
            }
            encode_reply(&FedFolderActorsFetchReply {
                members,
                caller_access: caller_access_stamp(&state, &channel_id, &requester).await,
                residency: residency_stamp(&fs),
            })
        })
    })
}

/// The **write-kind** gate (`federation.md` § Cross-nest…, gate bullet): the structural [`require_foreign_member`] binding **then** the
/// owner-granted `access == 'writer'` role for the claimed set. The peer's
/// signature is attribution; the authorization is state THIS nest wrote (the
/// owner's claimant-gated `members.set_access`). Returns the resolved
/// [`FolderRow`] both write kinds need. A rostered reader (or an absent role
/// row = reader) is refused `forbidden`, exactly as `resolve_writable_folder`
/// refuses a same-nest reader.
async fn require_foreign_writer(
    state: &AppState,
    channel_id: &[u8; 32],
    requester: &[u8; 32],
    origin_nest_id: &[u8; 32],
) -> Result<crate::db::FolderRow, RpcError> {
    require_foreign_member(state, channel_id, requester, origin_nest_id).await?;
    let fs = claimed_folder_for_channel(state, channel_id).await?;
    let is_writer = state
        .db
        .get_folder_member_role(channel_id, requester)
        .await
        .map_err(internal)?
        .is_some_and(|role| role.access == "writer");
    if is_writer {
        Ok(fs)
    } else {
        Err(forbidden("requesting actor is not a writer of this folder"))
    }
}

/// Stamp the requester's **current** access grant on a federated folder read
/// reply — the refresh half of *Recipient-side access discovery*
/// (`federation.md` § Cross-nest; ratified 2026-07-20).
///
/// **Every federated folder READ reply carries this, uniformly.** The ratified
/// design named `changes.fetch` as the carrier; `content_key.fetch` is stamped
/// too because it is the read a foreign member's client actually runs in
/// production (the custody-ingest commit poll — `changes.fetch` has no
/// production client caller until the agent gains foreign read routing), and
/// because "one rule for the whole read plane" is the shape a cold reader can
/// hold. Both handlers have already resolved membership for their gate, so the
/// stamp is one extra row read on a path that is polling anyway.
///
/// `None` means **"this nest asserts nothing"** — a set with no role row (the
/// implicit reader default) or a storage blip. It is *never* a revocation
/// signal: the client leaves its held value alone, and demotion is enforced
/// fail-closed at the next mint/record, never inferred from an absent field.
/// Best-effort by construction — a stamp failure must never fail the read the
/// member came for.
async fn caller_access_stamp(
    state: &AppState,
    channel_id: &[u8; 32],
    requester: &[u8; 32],
) -> Option<String> {
    state
        .db
        .get_folder_member_role(channel_id, requester)
        .await
        .ok()
        .flatten()
        .map(|role| role.access)
}

/// Stamp the folder's content residency on a federated folder read reply —
/// beside [`caller_access_stamp`], on the same three replies (`federation.md`
/// § Cross-nest shared folders + channel append → *Relay serving across
/// nests*, the `residency` stamp). The member's custody record keeps it and
/// its engine arms from it what a same-nest seat arms from its row: the upload
/// skip, the holder-keeps gate, the upload door's refusal
/// (`file-sync.md` § Relay serving → *A member on another nest*, step (1)).
///
/// Always **stated** — `"metadata_only"` or `"full"`, read off the claimed row
/// the handler already resolved for its gate — because absent on the wire
/// means *not stated*, never *full*.
pub(crate) fn residency_stamp(fs: &crate::db::FolderRow) -> Option<String> {
    Some(
        if crate::folder_handlers::residency_of(fs) == "metadata_only" {
            "metadata_only"
        } else {
            "full"
        }
        .to_string(),
    )
}

/// `fauna.federation.folder.changes.record` request — the mutating twin of
/// `changes.fetch`: a foreign **writer's** change, relayed by their home nest
/// into this (the set-home) nest's log. Carries the same semantic fields the
/// same-nest `fauna.sync.changes.record` does (the set is channel-keyed here,
/// not name-keyed — a foreign set's `name` only resolves on its home nest).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FedFolderChangesRecordRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
    pub device_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    pub change_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_hash: Option<String>,
    /// `path`, sealed client-side under the set's chunk-seal root — see
    /// [`fauna_protocol::sync::SyncChange::path_sealed`]. Relayed verbatim from
    /// the member's own nest so a cross-nest write seals identically to a
    /// same-nest one (both planes land in `record_change_core`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<serde_bytes::ByteBuf>,
    /// Causal watermark, relayed verbatim — see
    /// [`fauna_protocol::sync::SyncChange::derived_through`]. Wire-additive: an
    /// old member nest omits it and the row rests NULL (unknown causality).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_through: Option<i64>,
    /// Resolution marker, relayed verbatim ([`SyncChange::is_resolution`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolution: Option<bool>,
    /// The writer's signature over the record's `SignedChange` statement,
    /// relayed verbatim (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<serde_bytes::ByteBuf>,
    /// The key [`Self::signature`] verifies under. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<serde_bytes::ByteBuf>,
    /// The writer's `DeviceAuthorization`, **inline** — the cert crosses the
    /// trust boundary with the record because the foreign writer has no
    /// device row on this nest. Resolved by the writer's own home nest over
    /// its live grants. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_cert: Option<fauna_core::encoding::EmbedAsBytes>,
}

/// `fauna.federation.folder.changes.record` reply — the assigned `seq` (the
/// existing row's on a content-identical replay; the exactly-once guarantee is
/// [`crate::sync_handlers::record_change_core`] → `record_sync_change_metered`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderChangesRecordReply {
    pub seq: i64,
}

/// `fauna.federation.folder.write_token.mint` request — a foreign writer asks
/// the set-home nest for a short-lived byte-plane token so its client can POST
/// sealed chunks/manifests DIRECT over the open by-hash HTTPS bulk plane (bytes
/// never ride the federation channel — `transport.md` § carve-out).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderWriteTokenMintRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.folder.write_token.mint` reply — the opaque bulk-write
/// token + its absolute expiry (Unix seconds). The client POSTs bytes to the
/// home nest URL it already holds (the `ForeignFolder` record's `home_nest_url`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderWriteTokenMintReply {
    pub token: String,
    pub expires_at: u64,
}

/// `fauna.federation.folder.changes.record` — the write twin of
/// `changes.fetch`. Gate: [`require_foreign_writer`] (structural foreign-member
/// binding + owner-granted `access == 'writer'`), then the **same**
/// [`crate::sync_handlers::record_change_core`] the same-nest `changes.record`
/// runs: owner-pays metering + per-member cap + version floor, nest-stamping
/// `author_actor_id = requester`, and content-idempotent by construction (a
/// re-relayed record — reconnect / >L3-TTL redelivery / crashed-ack retry —
/// returns the original seq and charges nothing). No device
/// write-capability gate: a foreign writer has no device registered on this
/// nest, so the writer role IS the authorization (the same-nest device gate is a
/// local-plane concept).
fn folder_changes_record_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderChangesRecordRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            let device_id = parse_id32(&req.device_id, "device_id")?;
            let fs =
                require_foreign_writer(&state, &channel_id, &requester, &origin_nest_id).await?;

            let seq = crate::sync_handlers::record_change_core(
                &state,
                &requester,
                &fs,
                &req.path,
                req.manifest_hash.as_deref(),
                req.size_bytes,
                &req.change_type,
                req.content_key_version,
                req.thumbnail_hash.as_deref(),
                &device_id,
                req.path_sealed.as_ref().map(|b| &b[..]),
                req.derived_through,
                req.is_resolution,
                crate::change_signature::CarriedSignature::new(
                    req.signature.as_ref(),
                    req.signer_key.as_ref(),
                ),
                // Across the trust boundary: the cert rides inline, and the
                // signed actor must be `requesting_actor_id` — the recorder
                // the core rebuilds the statement for.
                crate::change_signature::CertCarriage::Inline(req.signer_cert.as_ref()),
            )
            .await?;
            encode_reply(&FedFolderChangesRecordReply { seq })
        })
    })
}

/// `fauna.federation.folder.write_token.mint` — gate ([`require_foreign_writer`]),
/// then mint a **short-lived, write-only** bulk-byte token bound to the foreign
/// writer's actor id (`federation.md` § Cross-nest…, contract point (ii)). TTL is the Rust constant [`FOREIGN_WRITE_TOKEN_TTL_SECS`] — no config
/// knob (no-operator invariant). The token is TTL-bound, **not** revocation-checked
/// per POST: eviction/demotion refuses the next *mint*, and an outstanding token's
/// residual window is one TTL constant (accepted, parity with "generations already
/// held are not revoked" — no per-POST grant re-check without a measured abuse
/// signal). Metering is unaffected: the byte POST is unmetered, and the owner's
/// quota is charged at `changes.record` time (owner-pays, keyed on the granted
/// actor per S1).
fn folder_write_token_mint_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderWriteTokenMintRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            let fs =
                require_foreign_writer(&state, &channel_id, &requester, &origin_nest_id).await?;

            let (token, expires_at) = state
                .auth
                .bulk_byte_tokens
                .mint(
                    fauna_core::identity::ActorId(requester),
                    // Audit/attribution key only (the byte routes never branch on
                    // it — the chunk store is global + content-addressed). The
                    // set's row id names what nest minted the token for whom —
                    // never its name, which rests sealed.
                    crate::bulk_byte_token::folder_attribution(fs.id),
                    BulkByteAccess::Write,
                    BulkByteMintPurpose::ForeignFolderWrite,
                    FOREIGN_WRITE_TOKEN_TTL_SECS,
                )
                .await;
            encode_reply(&FedFolderWriteTokenMintReply { token, expires_at })
        })
    })
}

/// `fauna.federation.folder.read_token.mint` request — a foreign member asks
/// the set-home nest for a short-lived, read-scoped byte-plane token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderReadTokenMintRequest {
    pub requesting_actor_id: String,
    pub channel_id: String,
}

/// `fauna.federation.folder.read_token.mint` reply — the opaque read-only bulk
/// token + its absolute expiry (Unix seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFolderReadTokenMintReply {
    pub token: String,
    pub expires_at: u64,
}

/// `fauna.federation.folder.read_token.mint` — the read-scoped twin of
/// [`folder_write_token_mint_handler`] (`federation.md` § Cross-nest shared
/// folders + channel append → *Relay serving across nests*). Gate:
/// [`require_foreign_member`] alone — a reader holds no write grant to mint
/// under — on a channel that is a claimed folder's. The token is `Read`, so
/// every bulk write route refuses it (`auth::BulkWriteAuth`), on the write
/// token's TTL constant.
///
/// What it opens is one door, the store-miss arm of the chunk route
/// (`auth::relay_reader`), and that door re-reads the membership row at every
/// request — so unlike the write token, whose residual window after a removal
/// is one TTL, a removed member's read token reads nothing from the moment the
/// row is gone.
fn folder_read_token_mint_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderReadTokenMintRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            let fs = claimed_folder_for_channel(&state, &channel_id).await?;

            let (token, expires_at) = state
                .auth
                .bulk_byte_tokens
                .mint(
                    fauna_core::identity::ActorId(requester),
                    crate::bulk_byte_token::folder_attribution(fs.id),
                    BulkByteAccess::Read,
                    BulkByteMintPurpose::ForeignFolderRead,
                    FOREIGN_WRITE_TOKEN_TTL_SECS,
                )
                .await;
            encode_reply(&FedFolderReadTokenMintReply { token, expires_at })
        })
    })
}

/// `fauna.federation.conversation.write_token.mint` — gate
/// ([`require_foreign_member`], the `channel.fetch` gate verbatim — **no**
/// `access == 'writer'` arm, because a conversation has no claimant and every
/// member posts — plus [`refuse_removed_room_member`], the third of the three
/// doors that read the binding alone and so must close with an end-to-end
/// room's removal too), then mint a **short-lived, write-only** bulk-byte token bound
/// to the foreign member's actor id, purpose
/// [`BulkByteMintPurpose::ForeignConversationWrite`]. The blob twin of
/// `channel.append`: the room's attachment bytes rest on its home nest beside
/// the record + `attachment_refs` that pin them (`conversation-rooms.md` § The
/// home nest → *Attachment bytes*, ratified 2026-09-09), and this is how a
/// foreign member's bytes get there — a direct HTTPS multipart POST to the home
/// nest's `POST /api/v1/blob` under the token's `BulkWriteAuth` arm, never over
/// the federation channel.
///
/// Same contract as the folder mint: TTL is the Rust constant
/// [`FOREIGN_WRITE_TOKEN_TTL_SECS`] (no config knob); the token is TTL-bound,
/// **not** revocation-checked per POST (a leave/evict refuses the next *mint*;
/// an outstanding token's residual window is one TTL). Unlike the folder plane
/// there is no metering to route: conversations carry no owner-pays byte cap
/// (`federation.md` § Cross-nest → *Substrate vs. policy*), so the POST is
/// bounded by `BLOB_BODY_LIMIT` alone, exactly as a same-nest member's is. The
/// bytes are reclaimed by the ordinary blob GC once the record naming them is
/// gone (`backup-restore.md` § 9 step 2g).
fn conversation_write_token_mint_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedConversationWriteTokenMintRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            require_foreign_member(&state, &channel_id, &requester, &origin_nest_id).await?;
            refuse_removed_room_member(&state, &channel_id, &requester).await?;

            let (token, expires_at) = state
                .auth
                .bulk_byte_tokens
                .mint(
                    fauna_core::identity::ActorId(requester),
                    // Audit/attribution key only (the byte routes never branch on
                    // it — the blob store is global + content-addressed). A
                    // conversation has no set name, so the channel names what
                    // this nest minted the token for.
                    format!("conv:{}", hex::encode(channel_id)),
                    BulkByteAccess::Write,
                    BulkByteMintPurpose::ForeignConversationWrite,
                    FOREIGN_WRITE_TOKEN_TTL_SECS,
                )
                .await;
            encode_reply(&FedConversationWriteTokenMintReply { token, expires_at })
        })
    })
}

/// Folder federation handlers (Phase 2 read plane + Phase 3 write plane + the
/// channel-substrate append/leave pair registered with the conversations family
/// above). All ride the channel's inherited per-IP + per-(nest, kind) throttles
/// — nothing per-kind to configure (no-operator invariant; raise a per-kind Rust
/// constant only if real polling fan-out is observed to exceed the shared
/// bucket, S4).
pub fn register_folder_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.folder.changes.fetch",
        meta(folder_changes_fetch_handler()),
    );
    b.add(
        "fauna.federation.folder.content_key.fetch",
        meta(folder_content_key_fetch_handler()),
    );
    b.add(
        "fauna.federation.folder.actors.fetch",
        meta(folder_actors_fetch_handler()),
    );
    b.add(
        "fauna.federation.folder.changes.record",
        meta(folder_changes_record_handler()),
    );
    b.add(
        "fauna.federation.folder.write_token.mint",
        meta(folder_write_token_mint_handler()),
    );
    b.add(
        "fauna.federation.folder.read_token.mint",
        meta(folder_read_token_mint_handler()),
    );
    // The public read plane's ONE read-only kind (phase 4 slice 4f-i). It sits
    // in this family by subject, NOT by gate: every kind above is
    // `channel_id`-keyed and membership-gated, while a public folder is
    // typically unbound — no MLS group, no channel, no roster row to gate on.
    // Its gate is the inverse shape (`folder_public::resolve_public_folder`),
    // which is exactly why the design refused an audience arm on
    // `folder.changes.fetch` (`federation.md` § The public folder read plane).
    b.add(
        "fauna.federation.folder.public.fetch",
        meta(folder_public_fetch_handler()),
    );
    // Relay serving across nests (`federation.md` § Cross-nest shared folders
    // + channel append → *Relay serving across nests*): the member's nest
    // leases a seat on the home nest, and the home nest asks the member's nest
    // to push an ask. Both replay-safe — a re-sent announce renews the same
    // lease, a re-sent ask pushes one more ask whose second answer nothing
    // takes.
    b.add(
        KIND_FED_FOLDER_SERVE_ANNOUNCE,
        meta(folder_serve_announce_handler()),
    );
    b.add(
        KIND_FED_FOLDER_CHUNK_WANTED,
        meta(folder_chunk_wanted_handler()),
    );
}

// ── Relay serving across nests (ruled 2026-10-01) ─────────────────────────────

/// Member's nest → home nest: a cross-nest writer's device serves (or no
/// longer serves) a folder homed on the callee.
pub const KIND_FED_FOLDER_SERVE_ANNOUNCE: &str = "fauna.federation.folder.serve.announce";

/// Home nest → member's nest: push a `fauna.sync.chunk.wanted` to the device
/// that announced the folder with the caller as its home.
pub const KIND_FED_FOLDER_CHUNK_WANTED: &str = "fauna.federation.folder.chunk.wanted";

/// `fauna.federation.folder.serve.announce` request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FedFolderServeAnnounceRequest {
    /// The member — the actor whose seat it is.
    pub requesting_actor_id: String,
    /// The folder's channel, hex.
    pub channel_id: String,
    /// The member's device that serves it, hex.
    pub device_id: String,
    /// `true` leases or renews the seat; `false` is *no longer serving*.
    pub serving: bool,
}

/// `fauna.federation.folder.serve.announce` reply.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FedFolderServeAnnounceReply {
    /// How long the lease runs, in seconds, from this reply — the member's
    /// nest renews at half of it. `0` on *no longer serving*.
    pub lease_secs: u64,
}

/// `fauna.federation.folder.chunk.wanted` request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FedFolderChunkWantedRequest {
    /// The member whose seat is asked.
    pub requesting_actor_id: String,
    /// The folder's channel, hex.
    pub channel_id: String,
    /// The seat's device, hex.
    pub device_id: String,
    /// The home nest's request id — the one the seat answers on the home
    /// nest's answer route.
    pub request_id: u64,
    /// The chunk's store key, 64 lowercase hex.
    pub store_key: String,
}

/// `fauna.federation.folder.chunk.wanted` reply.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FedFolderChunkWantedReply {
    /// Whether a connection was there to push the ask to. `false` — the
    /// connection is gone, or announced the folder with another home — lets
    /// the home nest's window refill at once.
    pub pushed: bool,
}

/// `fauna.federation.folder.serve.announce` — the home nest's half of step (2)
/// (`file-sync.md` § Relay serving → *A member on another nest*). *Serving*:
/// the write gate ([`require_foreign_writer`] — the member bound to the
/// calling nest, holding `writer`), then a lease on
/// [`crate::chunk_relay::ForeignSeats`], asked back through the member's nest
/// at the URL its roster row records (or, lacking one, a proven address of
/// that nest). *No longer serving*: drop the seat the calling nest leased, and
/// nothing else. The lease is memory only — a restart drops every seat, and
/// the member's nest's next renewal brings it back.
fn folder_serve_announce_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderServeAnnounceRequest = decode(&payload)?;
            let requester = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            let device = parse_id32(&req.device_id, "device_id")?;
            let seats = state.sync.chunk_resolver.foreign_seats();
            if !req.serving {
                if let Ok(fs) = claimed_folder_for_channel(&state, &channel_id).await {
                    seats.withdraw(fs.id, &requester, &device, &origin_nest_id);
                }
                return encode_reply(&FedFolderServeAnnounceReply { lease_secs: 0 });
            }
            let fs =
                require_foreign_writer(&state, &channel_id, &requester, &origin_nest_id).await?;
            let nest_url = match state
                .db
                .foreign_member_nest_url(&channel_id, &requester)
                .await
                .map_err(internal)?
            {
                Some(url) => url,
                None => state
                    .db
                    .proven_foreign_nest_urls(&origin_nest_id)
                    .await
                    .map_err(internal)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        invalid_request("this nest knows no address to ask the member's nest at")
                    })?,
            };
            seats
                .lease(crate::chunk_relay::ForeignSeat {
                    folder_id: fs.id,
                    channel_id,
                    member: requester,
                    device,
                    origin_nest_id,
                    nest_url,
                })
                .map_err(|full| {
                    crate::rpc_errors::unavailable_ns(
                        "federation",
                        format!("no room for another foreign seat ({full:?})"),
                    )
                })?;
            encode_reply(&FedFolderServeAnnounceReply {
                lease_secs: crate::chunk_relay::FOREIGN_SEAT_LEASE.as_secs(),
            })
        })
    })
}

/// Whether a leased foreign seat still passes the write gate it was admitted
/// on — read at every walk, so a removal or a demotion drops the seat at the
/// next ask (`federation.md` § … → *Relay serving across nests*). The same
/// two facts [`require_foreign_writer`] checks — the member bound to the
/// leasing nest, holding `writer` — read without confirming the binding.
pub(crate) async fn foreign_seat_still_admitted(
    state: &AppState,
    seat: &crate::chunk_relay::ForeignSeat,
) -> anyhow::Result<bool> {
    let bound = state
        .db
        .foreign_member_binding(&seat.channel_id, &seat.member)
        .await?
        .is_some_and(|(home, _)| home == seat.origin_nest_id);
    if !bound {
        return Ok(false);
    }
    Ok(state
        .db
        .get_folder_member_role(&seat.channel_id, &seat.member)
        .await?
        .is_some_and(|role| role.access == "writer"))
}

/// `fauna.federation.folder.chunk.wanted` — the member's nest's half of step
/// (3): push `fauna.sync.chunk.wanted`, naming the folder by its foreign ref,
/// on the one connection that announced this device and folder **with the
/// calling nest as its home** ([`crate::ws::WsState::foreign_serving_connection`]).
/// An ask from any other nest, or for a connection that is gone, pushes
/// nothing and says so. This nest learns a store key; the answer goes from the
/// seat straight to the home nest's byte plane.
fn folder_chunk_wanted_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFolderChunkWantedRequest = decode(&payload)?;
            let member = parse_actor(&req.requesting_actor_id)?;
            let channel_id = parse_id32(&req.channel_id, "channel_id")?;
            let device = parse_id32(&req.device_id, "device_id")?;
            let is_store_key = req.store_key.len() == 64
                && req
                    .store_key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !is_store_key {
                return Err(invalid_request("store_key is not 64 lowercase hex"));
            }
            let pushed = match state.ws.foreign_serving_connection(
                &member,
                &channel_id,
                &device,
                &origin_nest_id,
            ) {
                Some(conn) => crate::ws::WsState::push_to_connection(
                    &conn,
                    &fauna_protocol::PushEvent::SyncChunkWanted(
                        fauna_protocol::push_events::SyncChunkWantedPayload {
                            request_id: req.request_id,
                            folder: fauna_core::folder_keys::FolderRef::Foreign(channel_id)
                                .to_wire(),
                            store_key: req.store_key,
                            extra: Default::default(),
                        },
                    ),
                ),
                None => false,
            };
            encode_reply(&FedFolderChunkWantedReply { pushed })
        })
    })
}

/// TTL for a cross-nest writer's byte-plane token (`federation.md` § Cross-nest…,
/// contract point (ii)). A Rust constant, never a config knob — the
/// no-operator invariant. 10 minutes matches the same-nest WebDAV bulk token
/// (`bridge_blob_handlers::BULK_BYTE_TOKEN_TTL_SECS`): long enough for a bounded
/// upload burst, short enough that the residual post-demotion window is one such
/// burst.
const FOREIGN_WRITE_TOKEN_TTL_SECS: u64 = crate::bridge_blob_handlers::BULK_BYTE_TOKEN_TTL_SECS;

// ── Nest-writer backup plane (`federation.md` § Nest-writer backup plane) ─────
//
// The two kinds a **source nest's** in-process backup coordinator speaks to a
// **destination** nest, over the ordinary `fauna.federation.hello` handshake —
// no pairing, no new envelope (`message-segment-store.md` § Cross-location
// backup protocol → Nest→destination auth transport, ratified 2026-07-23).
//
// The gate is [`require_backup_writer`]: the nest-writer grant row **this** nest
// wrote when the owner's client registered it. The handshake proves *which* nest
// is calling; the grant is what says that nest may write. Deliberately the same
// "signature is attribution, authorization is state this nest wrote" shape as
// the `channel.fetch` foreign-member gate above.

/// TTL for a source nest's backup byte-plane token. A Rust constant, never a
/// config knob (no-operator invariant); the same 10 minutes as
/// [`FOREIGN_WRITE_TOKEN_TTL_SECS`], for the same reason — long enough for a
/// bounded upload burst, short enough that the residual window after a
/// revocation is one such burst.
const NEST_BACKUP_WRITE_TOKEN_TTL_SECS: u64 = 600;

/// `fauna.federation.backup.changes.record` request — a source nest relays one
/// custody record for an owner's segment backup (or an ordinary-folder mirror,
/// when `folder_id` is present) into this destination's `backup_custody`
/// projection. Mirrors the semantic fields of the same-nest
/// `fauna.sync.changes.record`; the set name is always derived HERE — from the
/// segment kind, or from the verified origin nest + `folder_id` — never chosen
/// by the writer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedBackupChangesRecordRequest {
    /// Hex actor id of the backup's **owner** — whose custody this is, whose
    /// quota pays for it, and the subject of the writer grant.
    pub owner_actor_id: String,
    /// Segment kind (`mail` / `conv` / `post` / `calendar` / `card`).
    pub kind: String,
    /// Hex 32-byte scope: the actor for mail/post/calendar/card, the channel id
    /// for conv (the segment store's own scoping — `message-segment-store.md`
    /// § Layout).
    pub scope_id: String,
    /// Hex 32-byte id the source nest records as the writing "device". A source
    /// nest has no device registered here (it is not one of the owner's
    /// clients), so this is attribution only — exactly as a foreign folder
    /// writer's is.
    pub device_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    pub change_type: String,
    /// Present on an **ordinary-folder mirror** record
    /// (`backup-destinations.md` § Ordinary-folder coverage): the folder's row
    /// id on the SOURCE nest. The destination then resolves the custody set as
    /// `__folder/<origin-nest-hex>/<folder-id>` from its own **verified**
    /// `origin_nest_id` — never from a writer-declared name, so a writer cannot
    /// aim custody at another source nest's mirror namespace. Absent = the
    /// segment plane above (additive; an older destination that drops this
    /// field falls into the kind-derived branch and refuses loudly, so folder
    /// coverage never lands mis-filed there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<i64>,
    /// Folder-mirror records only: the source row's sealed path blob, carried
    /// as-is (already ciphertext, blind to this destination) so a restore can
    /// recover file names under the owner's keys. The plaintext `path` on such
    /// a record is the source `path_hash` hex — a synthetic machine path,
    /// uniform across sealed- and plaintext-path source rows so supersede
    /// keying can never fork when a folder's audience flips.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<Vec<u8>>,
}

/// `fauna.federation.backup.changes.record` reply — the assigned `seq` (the
/// existing row's on a content-identical replay; exactly-once by content lives
/// in `record_sync_change_metered`, exactly as on the folder write plane).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedBackupChangesRecordReply {
    pub seq: i64,
}

/// `fauna.federation.backup.write_token.mint` request — a source nest asks for a
/// short-lived byte-plane token so it can POST sealed chunks/manifests DIRECT
/// over the by-hash HTTPS bulk plane. Bulk bytes never ride this channel
/// (`transport.md` § carve-out).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedBackupWriteTokenMintRequest {
    /// Hex actor id of the backup's owner — the grant subject, and the actor the
    /// minted token is bound to.
    pub owner_actor_id: String,
}

/// `fauna.federation.backup.write_token.mint` reply — the opaque write-only bulk
/// token + its absolute expiry (Unix seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedBackupWriteTokenMintReply {
    pub token: String,
    pub expires_at: u64,
}

/// The nest-writer gate both backup kinds run: the calling nest (the handshake's
/// **verified** `origin_nest_id`) must hold `owner`'s writer seat on this nest
/// with its grant in force (`segment-backup-protocol.md` § Cross-location
/// backup protocol → *The writer seat*).
///
/// Refuses the typed `fauna.backup.writer_not_seated` otherwise — its own
/// code, so the calling coordinator can tell a refusal from an outage — and
/// **fails closed** on a storage
/// error (propagated as `internal`, never as "allow"). Revocation therefore
/// takes effect at the next call of either kind — which is precisely the
/// freeze-the-backup affordance, operable from the owner's client with the
/// source nest fully hostile.
async fn require_backup_writer(
    state: &AppState,
    owner: &[u8; 32],
    origin_nest_id: &[u8; 32],
) -> Result<(), RpcError> {
    let granted = state
        .db
        .has_backup_writer_grant(owner, origin_nest_id)
        .await
        .map_err(internal)?;
    if granted {
        Ok(())
    } else {
        Err(crate::rpc_errors::coded_ns(
            "backup",
            "writer_not_seated",
            "calling nest does not hold this owner's backup writer seat with a grant in force",
        ))
    }
}

/// Resolve — creating on first use — the owner's reserved custody-copy set
/// custody set for `(kind, scope)` on this destination.
///
/// **Get-or-create inside the gated handler is deliberate.** The client-driven
/// path provisioned the set from the owner's own connection, but a source nest
/// cannot know the owner's channel set in advance and the owner's client would
/// otherwise have to pre-create one set per kind *and per conversation* before
/// any backup could flow. Creating it lazily behind the grant gate is idempotent
/// and crash-recoverable (`nest/common.md` § Client-state recoverability), and
/// mirrors the coordinator's existing lazy destination provisioning.
///
/// The set is owned by **the owner**, never the scope: that is what makes
/// `record_change_core`'s owner-pays metering charge the enrolled member for
/// their own conv backup, per the ratified conv v1 shape
/// (`message-segment-store.md` § Cross-location backup protocol).
async fn resolve_backup_custody_set(
    state: &AppState,
    owner: &[u8; 32],
    kind: &str,
    scope_id: &[u8; 32],
    // The ordinary-folder mirror axis: `Some` names the SOURCE folder id, and
    // the set name derives from the handshake's verified `origin_nest_id` —
    // the one naming decision a writer must never make itself.
    folder_id: Option<i64>,
    origin_nest_id: &[u8; 32],
) -> Result<crate::db::FolderRow, RpcError> {
    let name = match folder_id {
        Some(folder_id) => {
            crate::db::sync_storage::folder_backup_set_name(origin_nest_id, folder_id)
        }
        None => crate::db::sync_storage::reserved_backup_set_name(kind, scope_id)
            .ok_or_else(|| invalid_request(format!("kind has no backup surface: {kind}")))?,
    };

    if let Some(fs) = state
        .db
        .get_folder_for_actor(&name, owner)
        .await
        .map_err(internal)?
    {
        // An existing set that is NOT a custody copy is the owner's own live
        // rail on their own nest — refuse rather than write custody into it
        // (`reserved-folders.md` § Destination capability: the role holding
        // live state wins, the newcomer is refused). Reachable only if a
        // destination is also the owner's source nest, which the enroll flow
        // does not do, but silently co-mingling the two would be worse than a
        // typed refusal.
        if !crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
            return Err(forbidden(
                "reserved set for this kind exists on this nest as a live rail, not a \
                 backup custody copy",
            ));
        }
        return Ok(fs);
    }

    // A concurrent pass may win the `UNIQUE(name, actor_id)` race; the re-read
    // below is the arbiter either way, so the create's own error is not fatal.
    let _ = state
        .db
        .create_folder_with_options(
            &name,
            owner,
            crate::db::FolderOptions {
                // This provisioner is one of the two writers of the flag
                // (`reserved-folders.md` § Destination capability).
                custody_copy: true,
                ..Default::default()
            },
        )
        .await;

    let fs = state
        .db
        .get_folder_for_actor(&name, owner)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("backup custody set missing immediately after create"))?;
    // A concurrent rail mint may have won the race: the same refusal as above.
    if !crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
        return Err(forbidden(
            "reserved set for this kind exists on this nest as a live rail, not a \
             backup custody copy",
        ));
    }
    Ok(fs)
}

/// `fauna.federation.backup.changes.record` — gate ([`require_backup_writer`]),
/// resolve-or-create the owner's reserved backup set, then run the **same**
/// [`crate::sync_handlers::record_change_core`] every other record path runs:
/// owner-pays metering, backup-custody routing, content-idempotent insert. One
/// core for the same-nest, cross-nest folder and nest-backup planes, so a
/// record of the same content behaves identically on all three (priority #2).
///
/// The recorder is the **owner**, not the calling nest: the source nest records
/// the owner's own custody under the owner's grant, so the set owner and the
/// recorder coincide and no MLS group / member-cap machinery is involved.
fn backup_changes_record_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedBackupChangesRecordRequest = decode(&payload)?;
            let owner = parse_actor(&req.owner_actor_id)?;
            let scope_id = parse_id32(&req.scope_id, "scope_id")?;
            let device_id = parse_id32(&req.device_id, "device_id")?;
            require_backup_writer(&state, &owner, &origin_nest_id).await?;

            let fs = resolve_backup_custody_set(
                &state,
                &owner,
                &req.kind,
                &scope_id,
                req.folder_id,
                &origin_nest_id,
            )
            .await?;

            let seq = crate::sync_handlers::record_change_core(
                &state,
                &owner,
                &fs,
                &req.path,
                req.manifest_hash.as_deref(),
                req.size_bytes,
                &req.change_type,
                None,
                None,
                &device_id,
                // Segment records carry no `path_sealed`, and none is ever
                // coming for them: a coordinator-synthetic destination path
                // (`<hex>/seg-NNNNNNNN.dat`) is not a user-chosen name, so it
                // is a **declared non-seal** — `file-sync.md` § Sealed names &
                // paths → *Deliberate non-seals*. A folder-MIRROR record may
                // carry the source row's sealed blob (see the field's doc);
                // its plaintext `path` is still a synthetic machine path.
                req.path_sealed.as_deref(),
                // Machine bookkeeping rows (single-writer destination sets) —
                // causality honestly unknown, permanently.
                None,
                None,
                // Unsigned by set class: a reserved (`__`) custody set is out
                // of scope for writer-signed change records (never bound,
                // never shared, never walked by the pass) and is routed to
                // `backup_custody` before the core's signature check.
                crate::change_signature::CarriedSignature::default(),
                crate::change_signature::CertCarriage::ByReference,
            )
            .await?;
            encode_reply(&FedBackupChangesRecordReply { seq })
        })
    })
}

/// `fauna.federation.backup.write_token.mint` — gate
/// ([`require_backup_writer`]), then mint a short-lived, **write-only** bulk-byte
/// token bound to the owner's actor id. TTL is the Rust constant
/// [`NEST_BACKUP_WRITE_TOKEN_TTL_SECS`].
///
/// The token is TTL-bound, **not** re-checked per byte POST: a revocation
/// refuses the next *mint*, and an outstanding token's residual window is one
/// TTL. That is the accepted contract restated verbatim from the folder write
/// plane, and it is why the grace window T (30 d) — not the token — is what
/// actually bounds a rogue source nest's destructive reach.
fn backup_write_token_mint_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedBackupWriteTokenMintRequest = decode(&payload)?;
            let owner = parse_actor(&req.owner_actor_id)?;
            require_backup_writer(&state, &owner, &origin_nest_id).await?;

            let (token, expires_at) = state
                .auth
                .bulk_byte_tokens
                .mint(
                    fauna_core::identity::ActorId(owner),
                    // Audit/attribution key only — the byte routes never branch
                    // on it (the chunk store is global + content-addressed).
                    String::new(),
                    BulkByteAccess::Write,
                    BulkByteMintPurpose::NestBackupWrite,
                    NEST_BACKUP_WRITE_TOKEN_TTL_SECS,
                )
                .await;
            encode_reply(&FedBackupWriteTokenMintReply { token, expires_at })
        })
    })
}

// ── identity succession: the propagation push (slice 4) ──────────────────────

/// `fauna.federation.succession.push` — a nest tells a peer that one of the
/// identities the peer holds residue about has been superseded
/// (`identity-succession.md:81` § Propagation → *Federation peers*).
///
/// **A push is a HINT, never evidence.** The plane's adversary holds the
/// victim's identity seed (`identity-succession.md:28`), and a seed holder can
/// mint a registration chain every signature of which verifies — so bytes
/// delivered by the party asserting the succession can never be the trust
/// anchor. The receiver takes exactly one thing from this request: *which
/// identity to go verify*. Verification then runs against the receiver's own
/// recorded anchor for that identity — the home binding it learned first plus
/// the chain head it has persisted (`succession_pull` module docs, the anchor
/// rule) — and an identity the receiver holds no addressable anchor for is
/// refused, not TOFU'd from the payload. That anchoring, not any authorization
/// gate, is why this kind can stay gate-free beyond the handshake's
/// attribution and throttle.
///
/// Both fields are **verbatim canonical DAG-CBOR** — the same embed-as-bytes
/// rule the home nest's store and lookup already keep, so nothing on this path
/// re-encodes a record the relaying nest did not author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSuccessionPushRequest {
    /// Canonical bytes of one `SignedIdentitySuccession`. The receiver reads
    /// only its `old_actor_id`; the signatures are re-established from the
    /// anchor, not from these bytes.
    #[serde(with = "serde_bytes")]
    pub statement: Vec<u8>,
    // No `chain`: the old identity's registration chain rode here for
    // receivers older than the anchor-rule hardening, which verified from the
    // payload. It left the wire with the compat-remnant sweep
    // (`version-compatibility.md` § Dimension 2); an older sender's stray
    // `chain` key is ignored on decode.
}

/// Whether the receiver *learned* something. `false` is an ordinary,
/// non-error outcome — a re-push, a push to the identity's own home nest, a
/// hint the receiver holds no anchor for, a throttled hint, and a hint the
/// anchored home nest did not confirm all return it. Deliberately one
/// undifferentiated value: distinguishing them would be an oracle over the
/// receiver's residue, and the pusher can act on none of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSuccessionPushReply {
    pub recorded: bool,
}

fn succession_push_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedSuccessionPushRequest = decode(&payload)?;

            // The ONLY value taken from the payload: which identity to verify.
            // The request carries no chain — a seed thief can mint a chain
            // every signature of which is genuine, so the pushed statement is a
            // hint and the anchor rule (`succession_pull`) is the trust
            // decision.
            let signed: fauna_core::recovery::SignedIdentitySuccession =
                fauna_core::encoding::canonical_decode(&req.statement)
                    .map_err(|_| RpcError::new("fauna.rpc.malformed", "error.rpc.malformed"))?;
            let old = signed.statement.old_actor_id.0;

            // Fast refusals, each an ordinary "nothing learned" rather than an
            // error: a finer-grained verdict would be an oracle over this
            // nest's residue, and the pusher can act on none of them anyway.
            let already = state.db.succession_for(&old[..]).await.map_err(|e| {
                tracing::error!("read succession: {e}");
                RpcError::new("fauna.rpc.internal", "error.rpc.internal")
            })?;
            let is_local = state.db.get_user(&old).await.map_err(|e| {
                tracing::error!("read local account: {e}");
                RpcError::new("fauna.rpc.internal", "error.rpc.internal")
            })?;
            let recorded = if already.is_some() {
                // First-succession-wins already settled this identity — the
                // common re-push outcome.
                false
            } else if is_local.is_some() {
                // Only `succession.submit` may supersede a local account,
                // because only it re-points the account too.
                tracing::debug!(
                    target: "recovery",
                    "succession push names a locally-homed identity; ignoring"
                );
                false
            } else {
                // Throttled, bounded verify against this nest's own anchor.
                crate::succession_pull::verify_from_hint(&state, &old).await
            };

            encode_reply(&FedSuccessionPushReply { recorded })
        })
    })
}

/// Identity-succession propagation (`federation.md` § Identity-succession
/// propagation). One kind; the pull direction deliberately has none — see that
/// section.
pub fn register_succession_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.succession.push",
        meta(succession_push_handler()),
    );
}

/// Nest-writer backup-plane handlers (`federation.md` § Nest-writer backup
/// plane). Ride the channel's inherited per-IP + per-(nest, kind) throttles, like
/// every other federation kind — nothing per-kind to configure.
pub fn register_backup_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.backup.changes.record",
        meta(backup_changes_record_handler()),
    );
    b.add(
        "fauna.federation.backup.write_token.mint",
        meta(backup_write_token_mint_handler()),
    );
}

/// `RpcKindMeta` for a replay-safe federation kind at the 30 s default deadline —
/// every §4.E row except the five declared `forbid_replay` explicitly above
/// (`keypackage.fetch`, `welcome.deliver`, `inbox.deliver`, `room.invite`,
/// `room.invite_issue`, each with its rationale at the declaration site).
///
/// `forbid_replay: false` here is a **load-bearing assertion of semantic
/// idempotence** — it is what licenses the originating side's §4.D re-dial +
/// re-send (`FederationRouter::retry_safe`), and the peer's idempotency cache
/// is per-connection so it never survives that redial. Every row below was
/// re-verified at handler + DB level 2026-08-01. The reads are pure — except
/// `post.get`, whose worker-miss branch *caches* the fetched post locally
/// (content-addressed `store_post`, so a replay converges on the same rows).
/// The writes are idempotent upserts or content-keyed dedups —
/// `namespace_put_with_source` `INSERT OR REPLACE` on
/// `(namespace, entry_id, source)` (`sync.push`; a replay re-stamps a fresh
/// seq, so pullers re-fetch that entry once — harmless); content-addressed
/// `ensure_in_segment` + `INSERT OR REPLACE` projection (`post.forward`, with
/// the bridge fan-out gated on `newly_stored`); `AND tombstoned = 0` keyed
/// acks (`sync.{mls_ack,mail_ack}`); event-`id`-PRIMARY-KEY dedup with
/// broadcast + seal gated on `Stored` (`sync.nostr_push`); latest-epoch-wins
/// per-peer upserts (`{reports,trends}.exchange`);
/// absent-row-is-success deletes (`channel.leave`; `post.delete` via
/// `AlreadyGone`, which also keeps every propagation leg from re-firing);
/// exactly-once-by-content record relays (`{folder,backup}.changes.record` —
/// `federation.md` contract point (i), pinned by tier_3
/// `folder_write_plane_gates_on_writer_and_is_content_idempotent`);
/// in-memory-token-only mints (`{folder,backup,conversation}.write_token.mint` — a
/// duplicate is one more short-TTL entry until gc, no row/quota/counter);
/// `record_peer_succession` keyed on `old_actor_id PRIMARY KEY`
/// (`succession.push`). `channel.append` is the one deliberate exception to
/// "no second row": a replay past the cache appends a second copy of the same
/// MLS ciphertext under a fresh seq, which consumers quiet-skip (ratified S3;
/// known residue: the `dm_sent` anti-spam behavioral event double-counts, an
/// imprecision `conversations_handlers.rs` itself documents). Within one
/// connection the idempotency cache additionally replays a repeated
/// `idempotency_key` (§4.B), but that is a bonus, never the ground.
///
/// Byte payloads are `Vec<u8>` fields carrying `#[serde(with = "serde_bytes")]`,
/// so each rides as one CBOR byte string (`serialization.md` § Canonical IPLD
/// dag-cbor, "Variable-length byte fields") and a record's wire cost is its
/// raw length plus framing.
fn meta(handler: RpcHandler) -> RpcKindMeta {
    RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(30),
        handler,
    }
}

// ── nest-sync: §4.E rows 3–6 (paired surface) ────────────────────────────────
//
// The HTTP twins (`nest_sync_routes`) carry the calling nest's id + signature in
// the body and gate on `is_paired(actor, body_nest_id)`. On the channel the peer
// is verified once at handshake, so these drop the per-request nest_id/envelope
// and gate `is_paired` on the connection's **verified subject** nest_id (the
// §4.C hardening over the body-claimed id).

/// `fauna.federation.sync.pull` — paired nest pulls namespace entries since a seq.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncPullRequest {
    pub actor_id: String,
    pub namespace: String,
    pub since: i64,
}

/// One namespace entry on the wire (raw bytes — no hex/base64 as the JSON twin
/// needs, since CBOR carries bytes natively).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncEntry {
    #[serde(with = "serde_bytes")]
    pub entry_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub actor_sig: Vec<u8>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncPullReply {
    pub entries: Vec<FedSyncEntry>,
    pub up_to: i64,
}

/// `fauna.federation.sync.push` — paired nest pushes namespace entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncPushRequest {
    pub actor_id: String,
    pub namespace: String,
    pub entries: Vec<FedSyncPushEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncPushEntry {
    #[serde(with = "serde_bytes")]
    pub entry_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub actor_sig: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSyncPushReply {
    pub up_to: i64,
}

/// `fauna.federation.sync.mls_pull` — paired nest pulls buffered MLS ciphertext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMlsPullRequest {
    pub actor_id: String,
    pub since_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMlsBufferedMessage {
    pub seq: i64,
    #[serde(with = "serde_bytes")]
    pub channel_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    /// Present iff this buffered message has been taken down under a legal
    /// obligation on the serving nest — `envelope` withheld (empty), the
    /// tombstone `reference` carried for the paired nest to render. Additive
    /// (see [`FedChannelFetchMessage::legal_takedown`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown: Option<LegalTakedownMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMlsPullReply {
    pub messages: Vec<FedMlsBufferedMessage>,
    pub up_to: i64,
}

/// `fauna.federation.sync.mls_ack` — paired nest confirms MLS receipt up to a seq.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMlsAckRequest {
    pub actor_id: String,
    pub up_to_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMlsAckReply {
    pub purged: u64,
}

/// `fauna.federation.sync.mail_pull` — paired private nest pulls sealed
/// `__mail` records (inbound + Sent) from the public relay nest, after a seq.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMailPullRequest {
    pub actor_id: String,
    pub since_seq: i64,
}

/// One sealed mail record on the wire. `envelope` is the **verbatim** sealed
/// `MailRecordEnvelope` bytes (the relay never opens them — the public nest
/// holds no key); `floor` is the canonical-encoded `MailFloorMetadata` so the
/// destination reconstructs a faithful mirror row + segment footer. `record_id`
/// is the source's 32-byte id, preserved as the relay dedup key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMailRecord {
    pub seq: i64,
    #[serde(with = "serde_bytes")]
    pub record_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub floor: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMailPullReply {
    pub records: Vec<FedMailRecord>,
    pub up_to: i64,
}

/// `fauna.federation.sync.mail_ack` — paired private nest confirms receipt up
/// to a seq; the public nest tombstones + purges those records (the no-mail-on-
/// public property).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMailAckRequest {
    pub actor_id: String,
    pub up_to_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedMailAckReply {
    pub purged: u64,
}

// ── Nostr proxy-delegation relay (spec R6 (account-data-plane.md § The ratified decisions)) ────────────────────────────────────
//
// The keyless public *serving* box and the paired head holding the deposited
// `nsec` bridge the actor's Nostr relay over two head-originated channel kinds
// `fauna.federation.sync.nostr_{push,pull}`, both gated by the `nostr_push`
// pairing capability (`docs/goal/ui/nostr.md` § The bridging gate → Phase 2).
// The wire carries only signed public wire JSON + opaque kind-1059 wraps
// (constraint (i)/(iii): the row IS the event — self-authenticating; no key
// material, no DM rows, ever spellable here). These are the P2.1 wire
// shapes; the handlers (`nostr_push_handler`/`nostr_pull_handler`) and the
// head-side worker arm are later slices.

/// One event on the Nostr federation wire: the verbatim signed NIP-01 wire JSON
/// (`id`/`pubkey`/`sig` all live inside it — self-authenticating).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_push_handler`/`nostr_pull_handler`).
pub struct FedNostrEvent {
    pub raw_json: String,
}

/// `fauna.federation.sync.nostr_push` — the head pushes its `origin='ingest'`
/// rows (class-1 materialized/authored events + the actor's kind-1059 wraps)
/// public-ward; the RPC reply is the ack (the head advances its push cursor on
/// success). Carries `pubkey`/`relay_list` so the public box's account row is
/// auto-provisioned `signing_mode='proxied'` on first push (spec R9).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_push_handler`) + P2.4 (worker).
pub struct FedNostrPushRequest {
    pub actor_id: String,
    pub pubkey: String,
    pub relay_list: Option<String>,
    pub events: Vec<FedNostrEvent>,
}

/// One rejected event on a push reply — the row stays on the head (unlike mail,
/// nothing is destroyed), but the cursor advances past it (spec R6, reject
/// handling).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_push_handler`) + P2.4 (worker).
pub struct FedNostrReject {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_push_handler`) + P2.4 (worker).
pub struct FedNostrPushReply {
    pub accepted: u64,
    pub rejected: Vec<FedNostrReject>,
}

/// `fauna.federation.sync.nostr_pull` — the head fetches externally-deposited
/// `origin='ingest'` rows head-ward, cursor-parameterized by the compound
/// `(stored_at, id)` the head persists. Non-destructive: the public box keeps
/// serving everything it relays (relay semantics — no purge, hence no ack kind).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_pull_handler`) + P2.4 (worker).
pub struct FedNostrPullRequest {
    pub actor_id: String,
    pub since_stored_at: i64,
    pub since_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by P2.3 (`nostr_pull_handler`) + P2.4 (worker).
pub struct FedNostrPullReply {
    pub events: Vec<FedNostrEvent>,
    pub up_to_stored_at: i64,
    pub up_to_id: String,
}

pub fn register_sync_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add("fauna.federation.sync.pull", meta(sync_pull_handler()));
    b.add("fauna.federation.sync.push", meta(sync_push_handler()));
    b.add("fauna.federation.sync.mls_pull", meta(mls_pull_handler()));
    b.add("fauna.federation.sync.mls_ack", meta(mls_ack_handler()));
    b.add("fauna.federation.sync.mail_pull", meta(mail_pull_handler()));
    b.add("fauna.federation.sync.mail_ack", meta(mail_ack_handler()));
    // Nostr proxy-delegation legs (spec P2.3) — only under the `nostr` feature,
    // whose module owns the account/event stores the handlers touch. A
    // default-feature build registers neither kind, so it stays green.
    #[cfg(feature = "nostr")]
    {
        b.add(
            "fauna.federation.sync.nostr_push",
            meta(nostr_push_handler()),
        );
        b.add(
            "fauna.federation.sync.nostr_pull",
            meta(nostr_pull_handler()),
        );
    }
}

/// The namespace a `sync.pull` / `sync.push` may touch: the paired actor's own
/// self-namespace (= its public key), and no other. The pairing check says the
/// calling nest may sync *for this actor*; without this bind it could name any
/// account's namespace beside it, to read or to `INSERT OR REPLACE`. Owner: `private-mode.md` § Namespace Sync.
fn paired_actors_namespace(actor: &[u8; 32], namespace: &str) -> Result<[u8; 32], RpcError> {
    let ns = parse_id32(namespace, "namespace")?;
    if ns != *actor {
        return Err(forbidden("namespace is not the paired actor's"));
    }
    Ok(ns)
}

fn sync_pull_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedSyncPullRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .is_paired(&actor, &origin_nest_id)
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired"));
            }
            let ns = paired_actors_namespace(&actor, &req.namespace)?;
            let entries = state
                .db
                .namespace_entries_since(&ns, req.since, 1000)
                .await
                .map_err(internal)?;
            let up_to = entries.last().map(|e| e.seq).unwrap_or(req.since);
            let entries = entries
                .into_iter()
                .map(|e| FedSyncEntry {
                    entry_id: e.entry_id,
                    ciphertext: e.ciphertext,
                    actor_sig: e.actor_sig,
                    updated_at: e.updated_at,
                })
                .collect();
            encode_reply(&FedSyncPullReply { entries, up_to })
        })
    })
}

fn sync_push_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedSyncPushRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .is_paired(&actor, &origin_nest_id)
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired"));
            }
            let ns = paired_actors_namespace(&actor, &req.namespace)?;
            // The entry's `source` is the verified peer nest (hex), not a
            // body-claimed id — mirrors the HTTP twin's `source = req.nest_id`.
            let source = hex::encode(origin_nest_id);
            let mut max_seq: i64 = 0;
            for entry in &req.entries {
                match state
                    .db
                    .namespace_put_with_source(
                        &ns,
                        &entry.entry_id,
                        &entry.ciphertext,
                        &entry.actor_sig,
                        &source,
                    )
                    .await
                {
                    Ok(seq) => max_seq = max_seq.max(seq),
                    Err(e) => tracing::warn!("federation sync push entry failed: {e}"),
                }
            }
            encode_reply(&FedSyncPushReply { up_to: max_seq })
        })
    })
}

fn mls_pull_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedMlsPullRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .is_paired(&actor, &origin_nest_id)
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired"));
            }
            let channels = state
                .db
                .list_actor_channels(&actor)
                .await
                .map_err(internal)?;
            let rows = crate::segments::conv::read_for_scopes_after_seq(
                &state.conv_segments,
                &state.db,
                &channels,
                req.since_seq,
                crate::segments::SERVE_PAGE_MAX_RECORDS,
            )
            .await
            .map_err(internal)?;
            // Page byte-budget: same rule as `mail_pull` below (the puller's
            // contiguous ack PURGES the source, so close early before the
            // record that would overflow the frame and NEVER skip — a skipped
            // record the ack passes is irrecoverable loss). A head record
            // alone over the budget freezes the page (empty, `up_to` unmoved)
            // with a loud error.
            let (rows, rest) = crate::segments::take_page_within_budget(
                rows,
                |(_, _, body, _): &(i64, [u8; 32], Vec<u8>, Option<String>)| body.len(),
            );
            if rows.is_empty()
                && let Some((seq, channel, body, _)) = rest.first()
            {
                tracing::error!(
                    actor = %hex::encode(actor),
                    channel = %hex::encode(channel),
                    seq,
                    record_bytes = body.len(),
                    "mls relay: a single stored record exceeds the WS frame \
                     budget — the actor's relay cannot advance past it \
                     (transport.md § Max frame; remedy is a targeted heal, \
                     never a skip)"
                );
            }
            let messages: Vec<FedMlsBufferedMessage> = rows
                .into_iter()
                .map(|(seq, channel, body, legal_ref)| FedMlsBufferedMessage {
                    seq,
                    channel_id: channel.to_vec(),
                    envelope: body,
                    legal_takedown: LegalTakedownMarker::from_ref(legal_ref),
                })
                .collect();
            let up_to = messages.last().map(|m| m.seq).unwrap_or(req.since_seq);
            encode_reply(&FedMlsPullReply { messages, up_to })
        })
    })
}

fn mls_ack_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedMlsAckRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .is_paired(&actor, &origin_nest_id)
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired"));
            }
            let channels = state
                .db
                .list_actor_channels(&actor)
                .await
                .map_err(internal)?;
            let purged =
                crate::segments::conv::tombstone_up_to_seq(&state.db, &channels, req.up_to_seq)
                    .await
                    .map_err(internal)?;
            encode_reply(&FedMlsAckReply {
                purged: purged as u64,
            })
        })
    })
}

fn mail_pull_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedMailPullRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            // Capability-gated (not bare `is_paired`): a pairing must carry
            // `mail_pull` to drain the actor's mail. The channel already
            // verified `origin_nest_id` at handshake (§4.C).
            if !state
                .db
                .pairing_has_capability(
                    &actor,
                    &origin_nest_id,
                    fauna_protocol::pair::capability::MAIL_PULL,
                )
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired or missing mail_pull capability"));
            }
            let rows = crate::segments::mail::read_after_seq(
                &state.mail_segments,
                &state.db,
                &actor,
                req.since_seq,
                500,
            )
            .await
            .map_err(internal)?;
            // Page byte-budget (deployment-home-with-public-relay.md § Relay
            // frame budget): the reply must fit one 2 MiB WS frame, and the
            // perimeter admits messages whose stored envelope alone exceeds
            // it. Close the page early before the record that would overflow
            // — NEVER skip-and-continue: the puller acks contiguously and the
            // ack PURGES the source, so a skipped record the cursor passes is
            // irrecoverable loss. A head-of-line record that alone exceeds
            // the budget freezes the page (records empty, `up_to` unmoved) —
            // a loud, honest stall; the remedy is the continuation-record
            // re-append heal (message-segment-store.md § Continuation
            // records), not a bigger frame and not a skip.
            // (`segments::take_page_within_budget` is the shared cut — its
            // budget equals `INLINE_MAIL_REQUEST_BUDGET_BYTES`, both derived
            // as the 2 MiB frame minus a 64 KiB headroom.)
            let mut records = Vec::with_capacity(rows.len());
            for (seq, record_id, envelope, floor) in rows {
                // Re-encode the floor for the wire; the destination decodes it
                // and re-stamps its own local seq in `append_sealed_record`.
                let floor_bytes = floor.encode().map_err(internal)?;
                records.push(FedMailRecord {
                    seq,
                    record_id: record_id.to_vec(),
                    envelope,
                    floor: floor_bytes,
                });
            }
            let (records, rest) = crate::segments::take_page_within_budget(records, |r| {
                r.envelope.len() + r.floor.len()
            });
            if records.is_empty()
                && let Some(r) = rest.first()
            {
                tracing::error!(
                    actor = %hex::encode(actor),
                    seq = r.seq,
                    record_bytes = r.envelope.len() + r.floor.len(),
                    "mail relay: a single stored record exceeds the WS frame \
                     budget — the actor's relay cannot advance past it \
                     (deployment-home-with-public-relay.md § Relay frame budget; \
                     remedy: the continuation-record re-append heal)"
                );
            }
            let up_to = records.last().map(|r| r.seq).unwrap_or(req.since_seq);
            encode_reply(&FedMailPullReply { records, up_to })
        })
    })
}

fn mail_ack_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedMailAckRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .pairing_has_capability(
                    &actor,
                    &origin_nest_id,
                    fauna_protocol::pair::capability::MAIL_PULL,
                )
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired or missing mail_pull capability"));
            }
            let purged =
                crate::segments::mail::tombstone_up_to_seq(&state.db, &actor, req.up_to_seq)
                    .await
                    .map_err(internal)?;
            // Also expunge the IMAP placements the public relay box created at
            // MTA ingest (`bridge_imap_messages` rows): tombstoning the `__mail`
            // segment removes the *content*, but the placement row — and the
            // metadata it carries (sender domain, timestamp, UID) — would persist
            // and keep the message visible in the MDA's INBOX (EXISTS count),
            // violating the no-readable/persistent-copy property
            // (`deployment-home-with-public-relay.md` § Done definition). The
            // relay is destination-canonical, so the public box keeps nothing
            // after the home box pulled + acked.
            let now = now_epoch_secs();
            match state
                .db
                .purge_mail_placements_up_to_seq(&actor, req.up_to_seq, now)
                .await
            {
                Ok(expunged) => {
                    for (mailbox, uids, modseq) in expunged {
                        let rec = fauna_mail::segments::placement::MailPlacementRecord::Expunge {
                            mailbox,
                            uid_set: uids,
                            modseq: modseq as u64,
                            deleted_at: now,
                        };
                        if let Err(e) = state.mail_placement.append_event(&actor, &rec).await {
                            tracing::warn!(
                                "mail relay purge: placement Expunge append failed: {e}"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("mail relay purge: expunge IMAP placements failed: {e}");
                }
            }
            encode_reply(&FedMailAckReply {
                purged: purged as u64,
            })
        })
    })
}

// ── Nostr proxy-delegation handlers (spec P2.3) ───────────────────────────────
//
// Both run on the keyless public *serving* box, both gated `nostr_push` (as
// `mail_pull` gates `mail_ack` too — one capability, both legs). The head is the
// only side that originates; `nostr_push` carries head-authored rows public-ward
// (its reply is the ack) and `nostr_pull` fetches externally-deposited rows
// head-ward. The wire carries only signed public wire JSON + opaque kind-1059
// wraps (constraint (i)/(iii)); no key material or DM row is spellable.
// Registered under `#[cfg(feature = "nostr")]` in
// [`register_sync_federation_handlers`], so a default-feature build never sees
// them (the `nostr` module — and thus `ingest_federated_event` / the account +
// event stores — only exists under that feature).

/// `fauna.federation.sync.nostr_push` — the head pushes its `origin='ingest'`
/// rows public-ward. Gate: the pairing carries `nostr_push` (keyed by the
/// channel-verified `origin_nest_id`). Then (spec R9) auto-provision the actor's
/// proxied account row so the box can serve/gate it — absent → provision
/// `signing_mode='proxied'` (no key); a row with a **deposited key** or a
/// **different pubkey** is refused loudly rather than clobbered; a matching
/// proxied row needs no write. `req.pubkey` is peer-supplied and unproven, so a
/// pubkey **another actor** on this box holds is refused too — the writer's
/// one-pubkey-one-actor rule (`nostr::db::LinkAccountError::PubkeyHeld`). Each event then rides
/// [`nostr::federation::ingest_federated_event`](crate::nostr::federation::ingest_federated_event)
/// (verify → scope-gate → store `origin='federation'` → broadcast → the
/// keyless-self-gating seal seam), counting `accepted`/`rejected` into the reply
/// (the head advances its push cursor past both on a successful reply — a
/// permanently-rejectable row must not stall the leg). A **transient** store
/// error fails the whole batch (an internal `RpcError`) so the head does not
/// advance and retries.
#[cfg(feature = "nostr")]
fn nostr_push_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedNostrPushRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .pairing_has_capability(
                    &actor,
                    &origin_nest_id,
                    fauna_protocol::pair::capability::NOSTR_PUSH,
                )
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired or missing nostr_push capability"));
            }
            let actor_hex = hex::encode(actor);

            // R9: auto-provision the proxied account row (absent → provision;
            // deposited key or a different pubkey → refuse; matching → no write).
            {
                let conn = state.db.conn().await;
                match crate::nostr::db::get_account(&conn, &actor_hex).map_err(internal)? {
                    None => {
                        crate::nostr::db::link_account(
                            &conn,
                            &actor_hex,
                            &req.pubkey,
                            "proxied",
                            None,
                            None,
                            req.relay_list.as_deref(),
                        )
                        .map_err(|e| match e {
                            // `req.pubkey` is peer-supplied and unproven: a
                            // pubkey another actor on this box holds is
                            // refused, never provisioned over.
                            crate::nostr::db::LinkAccountError::PubkeyHeld => {
                                tracing::error!(
                                    actor = %actor_hex,
                                    "nostr push refused: pubkey held by another account"
                                );
                                forbidden("pubkey is linked to another account on this box")
                            }
                            e => internal(e),
                        })?;
                    }
                    Some(acct) if acct.encrypted_privkey.is_some() => {
                        tracing::error!(
                            actor = %actor_hex,
                            "nostr push refused: actor holds a deposited nsec on this box \
                             — it is not a proxy target (constraint (i))"
                        );
                        return Err(forbidden(
                            "actor holds a deposited nsec on this box; cannot proxy",
                        ));
                    }
                    Some(acct) if acct.nostr_pubkey != req.pubkey => {
                        tracing::error!(
                            actor = %actor_hex,
                            "nostr push refused: actor already linked to a different pubkey"
                        );
                        return Err(forbidden("actor already linked to a different pubkey"));
                    }
                    // A matching proxied row already exists — nothing to write.
                    Some(_) => {}
                }
            }

            let mut accepted: u64 = 0;
            let mut rejected: Vec<FedNostrReject> = Vec::new();
            for ev in &req.events {
                match crate::nostr::federation::ingest_federated_event(
                    &state,
                    &req.pubkey,
                    &ev.raw_json,
                )
                .await
                // A transient store error fails the whole batch so the head
                // does not advance its cursor and retries next cycle.
                .map_err(internal)?
                {
                    crate::nostr::federation::IngestOutcome::Stored
                    | crate::nostr::federation::IngestOutcome::Duplicate => accepted += 1,
                    crate::nostr::federation::IngestOutcome::Rejected(reason) => {
                        // Best-effort event id for the reject record (the row may
                        // be malformed JSON, in which case it has no readable id).
                        let id = serde_json::from_str::<serde_json::Value>(&ev.raw_json)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_str().map(String::from)))
                            .unwrap_or_default();
                        rejected.push(FedNostrReject { id, reason });
                    }
                }
            }
            encode_reply(&FedNostrPushReply { accepted, rejected })
        })
    })
}

/// `fauna.federation.sync.nostr_pull` — the head fetches externally-deposited
/// `origin='ingest'` rows head-ward, cursor-parameterized by the compound
/// `(stored_at, id)` the head persists. Same `nostr_push` gate. Resolves the
/// actor's proxied account pubkey (absent → an empty page, cursor echoed), then
/// [`nostr::federation::list_events_for_pull`](crate::nostr::federation::list_events_for_pull)
/// selects `origin='ingest'` rows for `pubkey ∪ #p` after the cursor, paged
/// within one WS frame via [`crate::segments::take_page_within_budget`] (a single
/// event over budget freezes the page — cursor unmoved — with a loud error, the
/// same close-early-never-skip rule the mail relay uses). Non-destructive: the
/// public box keeps serving everything it relays (relay semantics — no ack).
#[cfg(feature = "nostr")]
fn nostr_pull_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedNostrPullRequest = decode(&payload)?;
            let actor = parse_id32(&req.actor_id, "actor_id")?;
            if !state
                .db
                .pairing_has_capability(
                    &actor,
                    &origin_nest_id,
                    fauna_protocol::pair::capability::NOSTR_PUSH,
                )
                .await
                .map_err(internal)?
            {
                return Err(forbidden("not paired or missing nostr_push capability"));
            }
            let actor_hex = hex::encode(actor);

            let rows = {
                let conn = state.db.conn().await;
                let pubkey =
                    match crate::nostr::db::get_account(&conn, &actor_hex).map_err(internal)? {
                        Some(acct) => acct.nostr_pubkey,
                        // No account on this box yet (a pull before any push) —
                        // an empty page with the cursor echoed unchanged.
                        None => {
                            return encode_reply(&FedNostrPullReply {
                                events: Vec::new(),
                                up_to_stored_at: req.since_stored_at,
                                up_to_id: req.since_id.clone(),
                            });
                        }
                    };
                crate::nostr::federation::list_events_for_pull(
                    &conn,
                    &pubkey,
                    (req.since_stored_at, &req.since_id),
                    crate::segments::SERVE_PAGE_MAX_RECORDS as usize,
                )
                .map_err(internal)?
            };

            let (page, rest) = crate::segments::take_page_within_budget(rows, |r| r.raw_json.len());
            if page.is_empty()
                && let Some(r) = rest.first()
            {
                tracing::error!(
                    actor = %actor_hex,
                    stored_at = r.stored_at,
                    id = %r.id,
                    record_bytes = r.raw_json.len(),
                    "nostr relay pull: a single stored event exceeds the WS frame \
                     budget — the actor's relay cannot advance past it \
                     (transport.md § Max frame; remedy is a targeted heal, never a skip)"
                );
            }
            let (up_to_stored_at, up_to_id) = page
                .last()
                .map(|r| (r.stored_at, r.id.clone()))
                .unwrap_or((req.since_stored_at, req.since_id.clone()));
            let events = page
                .into_iter()
                .map(|r| FedNostrEvent {
                    raw_json: r.raw_json,
                })
                .collect();
            encode_reply(&FedNostrPullReply {
                events,
                up_to_stored_at,
                up_to_id,
            })
        })
    })
}

// ── posts: §4.E rows 7 (forward) + 9 (get) ───────────────────────────────────

/// `fauna.federation.post.forward` — a paired private nest relays a signed post.
/// The post's own author envelope is verified (kept); the HTTP twin's extra
/// nest-signature over the forward body is dropped (channel-authed), and the
/// paired-only policy gate keys on the verified subject nest_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostForwardRequest {
    #[serde(with = "serde_bytes")]
    pub post_bytes: Vec<u8>,
    /// 100-byte signed envelope (36-byte CID || 64-byte sig) of the post.
    #[serde(with = "serde_bytes")]
    pub post_envelope: Vec<u8>,
    /// Optional delegated-authoring cert (`atproto-pds-full.md` D10): the
    /// canonical embed-as-bytes of the identity-signed `DeviceAuthorization`
    /// authorizing a post signed by the account's server-held authoring
    /// sub-key rather than its identity key. Additive — an old peer omits it
    /// (serde default) and its receive verify fail-closed-refuses a delegated
    /// forward, leaving all existing author-signed traffic untouched. When
    /// present it is re-inserted into the reconstructed wire so the peer
    /// derives the same content-addressed `post_id` this nest did.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub signer_auth: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostForwardReply {}

/// `fauna.federation.post.delete` — the delete twin of `post.forward`: a paired
/// private nest relays a post deletion so a forwarded copy on the public nest
/// does not outlive the original (`feed.md` § State & data shape → *Post
/// deletion* → Propagation). The author-signed `Tombstone` embed-as-bytes rides
/// verbatim; the receiving nest re-verifies the tombstone's own author envelope
/// (`decode_tombstone`) — the same end-to-end author binding `post.forward`
/// keeps — and the paired-only policy gate keys on the same `post_forward`
/// capability that authorized the original forward (a delete is the removal half
/// of the same relay grant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostDeleteRequest {
    /// The signed embed-as-bytes `Tombstone` wire, verbatim — the `req.body`
    /// the author's client sent to `fauna.posts.delete`.
    #[serde(with = "serde_bytes")]
    pub tombstone_body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostDeleteReply {}

/// `fauna.federation.post.get` — a peer fetches a single post's raw bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostGetRequest {
    pub post_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPostGetReply {
    #[serde(with = "serde_bytes")]
    pub post: Vec<u8>,
    /// Present iff the post is withheld under a legal obligation on the
    /// serving nest — `post` is then withheld (empty, never read/resolved)
    /// and this carries the tombstone `reference`. A legal takedown is the
    /// *transparent, non-silent* carve-out (`moderation.md` § Categories &
    /// enforcement item 1), so the peer fetch **discloses** the withholding —
    /// consistent with the HTTP twin's 451 and the conversation-federation
    /// marker ([`FedChannelFetchMessage::legal_takedown`]) — and deliberately
    /// does NOT mirror policy-quarantine's existence-hiding `not_found`
    /// (posture ratified 2026-07-06). Additive: a peer that predates the
    /// field still sees an empty `post` (content withheld) — fail-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown: Option<LegalTakedownMarker>,
}

pub fn register_post_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.post.forward",
        meta(post_forward_handler()),
    );
    b.add("fauna.federation.post.delete", meta(post_delete_handler()));
    b.add("fauna.federation.post.get", meta(post_get_handler()));
}

fn post_forward_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedPostForwardRequest = decode(&payload)?;
            if req.post_envelope.len() != 100 {
                return Err(invalid_request("invalid post_envelope (need 100 bytes)"));
            }
            // The delegated-authoring cert (D10), when present, is re-inserted
            // into the reconstructed wire so both the receive verify AND the
            // re-served/re-stored bytes retain it (identical content-addressed
            // `post_id` to the origin nest).
            let signer_auth = match &req.signer_auth {
                Some(cert_bytes) => Some(Box::new(
                    fauna_core::encoding::canonical_decode::<fauna_core::encoding::EmbedAsBytes>(
                        cert_bytes,
                    )
                    .map_err(|_| invalid_request("invalid signer_auth cert"))?,
                )),
                None => None,
            };
            // Decode + verify the post's own author signature (kept — the
            // end-to-end author binding; only the relay nest-sig is dropped),
            // accepting a delegated authoring sub-key's signature when the cert
            // rides (D10).
            let wire = fauna_core::encoding::EmbedAsBytes {
                envelope: req.post_envelope.clone(),
                bytes: req.post_bytes.clone(),
                signer_auth,
            };
            let (post_bytes, post_env) = wire
                .clone()
                .into_signed()
                .map_err(|_| invalid_request("invalid envelope"))?;
            let post: fauna_core::data::Post =
                fauna_core::encoding::decode_signed_bytes(&post_bytes)
                    .map_err(|_| invalid_request("invalid post"))?;
            if fauna_core::encoding::verify_authoring_envelope(
                &post,
                &post_bytes,
                &post_env,
                wire.signer_auth.as_deref(),
                &fauna_core::data::Capability::Post,
                post.created_at,
            )
            .is_err()
            {
                return Err(invalid_request("invalid post signature"));
            }

            // Same future-created_at bound the local ingest door applies
            // (`crate::storage::reject_future_created_at`) — the federation
            // leg never calls `Storage::ingest_post`, so it needs its own
            // call to the shared check.
            if let Err(reason) = crate::storage::reject_future_created_at(post.created_at) {
                return Err(invalid_request(format!(
                    "ingest rejected: {}",
                    reason.as_snake_case()
                )));
            }

            // Paired-only submission policy: gate on the verified peer nest_id
            // AND the pairing's `post_forward` capability — the relay grant
            // `private-mode.md` § Post Forwarding names, and the sibling of the
            // mail relay's `mail_pull` gate. A capability-agnostic `is_paired`
            // would let a nest paired for (say) mail relay alone inject posts.
            let policy = state
                .config
                .submission
                .as_ref()
                .map(|s| &s.policy)
                .unwrap_or(&SubmissionPolicy::Open);
            if *policy == SubmissionPolicy::PairedOnly {
                let authorized = state
                    .db
                    .pairing_has_capability(
                        &post.author.0,
                        &origin_nest_id,
                        fauna_protocol::pair::capability::POST_FORWARD,
                    )
                    .await
                    .unwrap_or(false);
                if !authorized {
                    return Err(forbidden("nest not paired for this actor"));
                }
            }

            let stored_bytes = fauna_core::encoding::canonical_encode(&wire).map_err(internal)?;
            let post_id: [u8; 32] = *blake3::hash(&stored_bytes).as_bytes();

            // A post this nest has seen deleted is never re-ingested by replay
            // (`feed.md` § State & data shape → *Post deletion*): its signed
            // bytes stay public, so under `Open` any peer could otherwise undo
            // the author's delete and re-publish it through the fan-out below.
            // `Ok` with no store, no fan-out, no index — the peer stops retrying.
            if state
                .db
                .post_was_deleted(&post_id)
                .await
                .map_err(internal)?
            {
                return encode_reply(&FedPostForwardReply {});
            }

            // No relay-side classification: the nest is not a scoring position
            // (`content-scoring.md` § The placement matrix — a scorer runs only
            // where a capability for its inputs is held). This is uniform with a
            // local `fauna.posts.create`, which likewise no longer classifies at
            // ingest; post scoring is the capability-holder story (the re-score
            // drain under a user-minted grant).
            //
            // The newly-stored gate for the bridge fan-out below: `store_post`
            // is idempotent (a re-delivered forward upserts and reports `Ok`),
            // so first-arrival must be read off the projection, not the store
            // result. Forwards for one post come from one origin's outbox
            // (serial retries), so exists-then-store is race-free in practice.
            let newly_stored = !state.db.post_exists(&post_id).await.map_err(internal)?;

            // Store the embed-as-bytes wire shape (content-addressed post_id):
            // body → `__post` segment store, projection with empty payload.
            match crate::segments::post::store_post(
                &state.post_segments,
                &state.db,
                &post_id,
                &stored_bytes,
                None,
            )
            .await
            {
                Ok(()) => {
                    if newly_stored {
                        // The same create-side bridge fan-out as a local
                        // `fauna.posts.create` (`activitypub.md` § The produce
                        // direction, paired-deployments bullet): the author
                        // enabled their bridges on THIS nest, so a post arriving
                        // by relay reaches their followers exactly like a local
                        // one. The gate keeps outbox retries from
                        // double-publishing.
                        crate::routes::spawn_post_bridge_fanout(
                            &state,
                            post.author.0,
                            post_id,
                            &stored_bytes,
                        );
                        // ...and the same reception pass: a room-restricted
                        // post this nest now stores is one it may index, if it
                        // homes the room (`crate::room_post_view`).
                        crate::room_post_view::index_room_post(&state, &post_id, &stored_bytes)
                            .await;
                    }
                }
                Err(e) if e.to_string().contains("UNIQUE") => {}
                Err(e) => return Err(internal(e)),
            }

            encode_reply(&FedPostForwardReply {})
        })
    })
}

/// `fauna.federation.post.delete` receive side — apply a relayed post deletion
/// (the delete twin of `post_forward_handler`). Verify-then-decode the signed
/// tombstone (the same signed-only surface `posts_delete_handler` enforces), gate
/// on the `post_forward` capability under `PairedOnly`, then remove through the
/// shared `delete_post_core` (author-check-2 = the stored post's author is the
/// tombstone author; crash-safe removal order; the public nest's own derivation
/// legs chase idempotently). Idempotent: a re-delivered delete hits
/// `AlreadyGone` and still returns Ok.
fn post_delete_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedPostDeleteRequest = decode(&payload)?;

            // Decode + verify the tombstone's own author signature (kept — the
            // end-to-end author binding; the channel already authed this peer).
            let tombstone = fauna_core::encoding::decode_tombstone(&req.tombstone_body)
                .map_err(|_| invalid_request("invalid signed tombstone"))?;
            let digest = crate::db::posts::cid_to_digest(&tombstone.post_id);

            // Paired-only submission policy: gate on the verified peer nest_id
            // AND the pairing's `post_forward` capability — the same grant that
            // authorized the original forward. A delete rides the forward
            // capability; it is the removal half of the same relay.
            let policy = state
                .config
                .submission
                .as_ref()
                .map(|s| &s.policy)
                .unwrap_or(&SubmissionPolicy::Open);
            if *policy == SubmissionPolicy::PairedOnly {
                let authorized = state
                    .db
                    .pairing_has_capability(
                        &tombstone.author.0,
                        &origin_nest_id,
                        fauna_protocol::pair::capability::POST_FORWARD,
                    )
                    .await
                    .unwrap_or(false);
                if !authorized {
                    return Err(forbidden("nest not paired for this actor"));
                }
            }

            // Apply the deletion through the shared core. `actor =
            // tombstone.author.0`: the channel is authed as the peer nest, not a
            // user session, so the connection actor IS the verified tombstone
            // author (author-check-1 is then the trivial identity; the real
            // gates are author-check-2 inside the core + the capability gate
            // above). AlreadyGone (a replayed delete) is Ok.
            match crate::routes::delete_post_core(
                &state,
                tombstone.author.0,
                &tombstone,
                digest,
                crate::routes::RenderSite::Now,
            )
            .await
            {
                Ok(_) => encode_reply(&FedPostDeleteReply {}),
                Err(crate::routes::PostDeleteError::NotAuthor) => {
                    Err(forbidden("tombstone author is not the stored post author"))
                }
                Err(crate::routes::PostDeleteError::Internal(msg)) => Err(internal(msg)),
            }
        })
    })
}

fn post_get_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedPostGetRequest = decode(&payload)?;
            let post_id = parse_id32(&req.post_id, "post_id")?;
            // A peer nest is never the author/admin, so the quarantine gate sees
            // `caller = None` (mirrors the HTTP twin's no-bearer cross-nest fetch).
            match get_post_core(&state, None, post_id).await {
                GetPostOutcome::Found(bytes) => encode_reply(&FedPostGetReply {
                    post: bytes,
                    legal_takedown: None,
                }),
                // Legally taken down: the body is withheld from the peer-nest
                // fetch (never propagates across federation), but the
                // withholding itself is DISCLOSED via the additive marker —
                // the transparent, non-silent carve-out, consistent with the
                // HTTP twin's 451 and the conv-federation marker, deliberately
                // unlike policy-quarantine's existence-hiding `not_found`
                // (`moderation.md` § Implementation status today, ratified
                // 2026-07-06). The peer already holds the CID it is asking
                // about, so disclosure adds no enumeration surface.
                GetPostOutcome::LegalTakedown { reference } => encode_reply(&FedPostGetReply {
                    post: Vec::new(),
                    legal_takedown: LegalTakedownMarker::from_ref(Some(reference)),
                }),
                GetPostOutcome::NotFound => Err(not_found("post not found")),
                GetPostOutcome::Error => Err(internal("storage error")),
            }
        })
    })
}

// ── feed: §4.E row 8 (query) ─────────────────────────────────────────────────
//
// Reuses the shared `RemoteQueryRequest` / `RemoteQueryResponse` + the
// `remote_query_feed_core` extracted from the HTTP twin — the channel and the
// HTTP route are the same query over public post data. Unauthenticated on HTTP
// today; on the channel it gains the connection's mutual auth + per-nest throttle
// for free (§4.E hardening dividend).

pub fn register_feed_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add("fauna.federation.feed.query", meta(feed_query_handler()));
}

fn feed_query_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: RemoteQueryRequest = decode(&payload)?;
            let resp: RemoteQueryResponse = remote_query_feed_core(&state, &req)
                .await
                .map_err(map_api_error)?;
            encode_reply(&resp)
        })
    })
}

// ── the shared peer-import error ────────────────────────────────────────────

/// A peer-supplied aggregate batch that failed import — shared by the serving
/// exchange handlers (a peer pushed to us) and the exchange originator's
/// pull-import (we pulled a peer's export), so BOTH inbound directions of the
/// reports and trends pairs run the identical validation + peer-bucket
/// treatment. There is no second import path.
#[derive(Debug)]
pub enum PeerImportError {
    /// The batch is malformed / out of bounds — the serving handler maps this
    /// to `invalid_request`; the originator drops the pulled reply.
    Invalid(String),
    /// A local DB failure — `internal` on the serving side.
    Db(anyhow::Error),
}

impl std::fmt::Display for PeerImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerImportError::Invalid(m) => write!(f, "invalid peer batch: {m}"),
            PeerImportError::Db(e) => write!(f, "db: {e:#}"),
        }
    }
}

/// Parse a 32-byte hex id (the `parse_id32` twin without the RpcError shape).
fn hex32(s: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(s).ok()
}

// ── distributed report sharing: reports.{exchange,export} ───────────────────
//
// The k-anonymized per-item report aggregates (`report-sharing.md`
// § Federation exchange): nest-signature
// auth (the channel's mutual handshake), open-federation, per-request entry
// cap. Only `(content_hash, factor, count)` crosses — never reporter identity,
// never content bytes. Import collapses every peer into ONE flat non-scaling
// corroboration bucket (the hostile-signer invariant: `nest_id`s are
// self-minted, so claimed counts and peer multiplicity buy nothing); export
// serves only LOCAL k-gate-passed counts (no laundering).

/// A single per-item report aggregate on the federation wire. `content_hash`
/// is the hex-encoded canonical 32-byte hash (uniform with the other
/// federation handlers' hex ids); `count` is the exporter's LOCAL distinct-
/// reporter count, which passed the exporter's k-gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedReportEntry {
    pub content_hash: String,
    pub factor: String,
    pub count: u32,
}

/// `fauna.federation.reports.exchange` — a peer pushes its ≥k aggregates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedReportsExchangeRequest {
    pub epoch: i64,
    pub entries: Vec<FedReportEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedReportsExchangeReply {
    pub imported: usize,
}

/// `fauna.federation.reports.export` reply — this nest's local k-gate-passed
/// aggregates (the request payload is ignored). This is byte-identical to the
/// transparency view a local user reads (`report-sharing.md` § Client wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedReportsExportReply {
    pub entries: Vec<FedReportEntry>,
}

/// Bound on a peer's factor name — a malformed-request check; which factors
/// may land at all is `fauna_core::scoring::reports::is_exchanged_factor`
/// (`report:spam` + the Layer-B `signal:{watch-complete,skip}` cue aggregates,
/// engagement-cues.md § Layer B — same exchange pair, no new kinds).
const MAX_REPORT_FACTOR_LEN: usize = 64;

pub fn register_reports_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.reports.exchange",
        meta(reports_exchange_handler()),
    );
    b.add(
        "fauna.federation.reports.export",
        meta(reports_export_handler()),
    );
}

/// Import a peer's ≥k report aggregates: k-gate honored on the import end too
/// (a below-k claim is skipped, not an error — a mixed batch still imports),
/// flat non-scaling peer bucket via `upsert_peer_report`, affected aggregates
/// recomputed. Returns the count that landed.
///
/// **Everything a peer supplies is hostile until checked, and the check is the
/// value, not only the count.** This path re-runs the exporter's k-gate on
/// `count` (`exposed_report_count`) and dates rows from the nest's own
/// `updated_at`, confining the peer's `epoch` to a monotonicity guard;
/// `import_trends_entries` re-gates `engager_count` (`exposed_trend_entry`),
/// passes its own `now_ms`, and treats `score_pm` as a fetch-order hint it never
/// sums. Neither feeds a peer-supplied scalar into an average. When adding a
/// third import path, the question is not "is the value capped downstream" but
/// "does an unbounded value change an aggregate before the cap applies" — a
/// bound on how many reporters a peer can pretend to be bounds nothing about
/// the magnitude, sign or age of the number it sends.
pub async fn import_report_entries(
    state: &AppState,
    origin_nest_id: &[u8; 32],
    epoch: i64,
    entries: &[FedReportEntry],
) -> Result<usize, PeerImportError> {
    if entries.len() > crate::db::reports::MAX_REPORT_EXCHANGE_ENTRIES {
        return Err(PeerImportError::Invalid(format!(
            "report entries {} exceeds cap {}",
            entries.len(),
            crate::db::reports::MAX_REPORT_EXCHANGE_ENTRIES
        )));
    }
    let mut imported = 0usize;
    for entry in entries {
        let hash = hex32(&entry.content_hash)
            .ok_or_else(|| PeerImportError::Invalid("content_hash not 32-byte hex".into()))?;
        if entry.factor.is_empty() || entry.factor.len() > MAX_REPORT_FACTOR_LEN {
            return Err(PeerImportError::Invalid(
                "factor length out of bounds".into(),
            ));
        }
        // Only the exchanged factor family is writable here:
        // any other factor would replace that factor's local bus row. Skipped,
        // not refused, so a newer peer's additive factor never fails a batch.
        if !fauna_core::scoring::reports::is_exchanged_factor(&entry.factor) {
            continue;
        }
        // A count the exporter's own k-gate would have withheld is not
        // accepted as corroboration (the gate is honored on both ends).
        if fauna_core::scoring::reports::exposed_report_count(entry.count).is_none() {
            continue;
        }
        let landed = state
            .db
            .upsert_peer_report(origin_nest_id, &hash, &entry.factor, entry.count, epoch)
            .await
            .map_err(PeerImportError::Db)?;
        if landed {
            imported += 1;
            // Recompute the affected aggregate → the flat peer bucket lands
            // on any local items carrying this hash.
            let key = crate::db::reports::ReportKey {
                content_hash: hash,
                factor: entry.factor.clone(),
                content_kind: "mail".to_string(),
            };
            state
                .db
                .recompute_report_score(&key)
                .await
                .map_err(PeerImportError::Db)?;
        }
    }
    Ok(imported)
}

fn reports_exchange_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedReportsExchangeRequest = decode(&payload)?;
            let imported = import_report_entries(&state, &origin_nest_id, req.epoch, &req.entries)
                .await
                .map_err(|e| match e {
                    PeerImportError::Invalid(m) => invalid_request(m),
                    PeerImportError::Db(e) => internal(e),
                })?;
            encode_reply(&FedReportsExchangeReply { imported })
        })
    })
}

fn reports_export_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, _payload| {
        Box::pin(async move {
            let entries = state
                .db
                .export_report_aggregates()
                .await
                .map_err(internal)?
                .into_iter()
                .map(|(hash, factor, count)| FedReportEntry {
                    content_hash: hex::encode(hash),
                    factor,
                    count,
                })
                .collect();
            encode_reply(&FedReportsExportReply { entries })
        })
    })
}

// ── trends: the distinct-peer ramp exchange (`trending.md` § Federation) ──────

/// A single local trend entry on the federation wire (`trending.md`
/// § Federation exchange). `content_id` is the hex-encoded canonical 32-byte
/// post id (uniform with the other federation handlers' hex ids). `score_pm` is
/// the exporter's UN-composed `local_pm` (never a peer-derived term — no
/// laundering), a fetch-prioritization hint the importer NEVER sums.
/// `engager_count` is the exporter's distinct local engager count, which passed
/// the exporter's k-gate and the importer re-validates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedTrendEntry {
    pub content_id: String,
    pub score_pm: u16,
    pub engager_count: u32,
}

/// `fauna.federation.trends.exchange` — a peer pushes its ≥k local trend entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedTrendsExchangeRequest {
    pub epoch: i64,
    pub entries: Vec<FedTrendEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedTrendsExchangeReply {
    pub imported: usize,
}

/// `fauna.federation.trends.export` reply — this nest's local k-gate-passed trend
/// entries (the request payload is ignored).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedTrendsExportReply {
    pub entries: Vec<FedTrendEntry>,
}

pub fn register_trends_federation_handlers(b: &mut FederationRouterBuilder) {
    b.add(
        "fauna.federation.trends.exchange",
        meta(trends_exchange_handler()),
    );
    b.add(
        "fauna.federation.trends.export",
        meta(trends_export_handler()),
    );
}

/// Import a peer's ≥k trend entries (`trending.md` § Federation exchange): the
/// k-gate is re-validated on the claimed `engager_count` (a below-k claim is
/// skipped, not an error — a mixed batch still imports), the presence bit lands
/// via `upsert_peer_trend` (latest-epoch-wins per peer), and each affected local
/// `trending` row is recomputed so the distinct-peer ramp takes effect — IFF the
/// post is seen+public here; an unseen id scores 0 (no blind row) and waits for
/// the Slice-4 `post.get` fetch. Returns the count that landed.
pub async fn import_trend_entries(
    state: &AppState,
    origin_nest_id: &[u8; 32],
    epoch: i64,
    entries: &[FedTrendEntry],
) -> Result<usize, PeerImportError> {
    if entries.len() > crate::db::trends::MAX_TREND_EXCHANGE_ENTRIES {
        return Err(PeerImportError::Invalid(format!(
            "trend entries {} exceeds cap {}",
            entries.len(),
            crate::db::trends::MAX_TREND_EXCHANGE_ENTRIES
        )));
    }
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    let now_ms = now_us / 1000;
    let mut imported = 0usize;
    for entry in entries {
        let content_id = hex32(&entry.content_id)
            .ok_or_else(|| PeerImportError::Invalid("content_id not 32-byte hex".into()))?;
        // A count the exporter's own k-gate would have withheld is not accepted as
        // corroboration (the gate is honored on both ends).
        if fauna_core::scoring::trends::exposed_trend_entry(entry.engager_count).is_none() {
            continue;
        }
        let landed = state
            .db
            .upsert_peer_trend(
                origin_nest_id,
                &content_id,
                entry.score_pm,
                entry.engager_count,
                epoch,
                now_ms,
            )
            .await
            .map_err(PeerImportError::Db)?;
        if landed {
            imported += 1;
            state
                .db
                .recompute_trend_score(&content_id, now_us)
                .await
                .map_err(PeerImportError::Db)?;
        }
    }
    Ok(imported)
}

fn trends_exchange_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedTrendsExchangeRequest = decode(&payload)?;
            let imported = import_trend_entries(&state, &origin_nest_id, req.epoch, &req.entries)
                .await
                .map_err(|e| match e {
                    PeerImportError::Invalid(m) => invalid_request(m),
                    PeerImportError::Db(e) => internal(e),
                })?;
            encode_reply(&FedTrendsExchangeReply { imported })
        })
    })
}

fn trends_export_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, _payload| {
        Box::pin(async move {
            let now_us = fauna_core::data::Timestamp::now().as_i64();
            let entries = state
                .db
                .export_trend_entries(now_us)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|(id, score_pm, engager_count)| FedTrendEntry {
                    content_id: hex::encode(id),
                    score_pm,
                    engager_count,
                })
                .collect();
            encode_reply(&FedTrendsExportReply { entries })
        })
    })
}

// ── Nostr proxy-delegation handler tests (spec P2.3) ──────────────────────────
//
// The two handlers run on the keyless public serving box. Each test builds one
// public-box `AppState`, authorizes a head `origin_nest_id` with `nostr_push`,
// and invokes the handler closure directly (no channel — the channel transport +
// the head-side worker arm are exercised end-to-end by the tier_3
// `nostr_federation_legs` integration test).
#[cfg(all(test, feature = "nostr"))]
mod nostr_fed_tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::nostr;
    use crate::nostr::db as nostr_db;
    use crate::routes::AppState;
    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
    use std::sync::Arc;

    const NOSTR_PUSH: &str = fauna_protocol::pair::capability::NOSTR_PUSH;

    async fn public_box() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        nostr::init_db(&db).await.expect("init nostr tables");
        Arc::new(AppState::for_test(db))
    }

    fn keypair(seed: u8) -> Keypair {
        Keypair::from_secret_bytes([seed.max(1); 32]).unwrap()
    }
    fn pubkey_hex(kp: &Keypair) -> String {
        hex::encode(kp.public_key_bytes())
    }
    fn signed(kp: &Keypair, kind: u64, created_at: u64, tags: Vec<Tag>, content: &str) -> Event {
        kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at,
            kind,
            tags,
            content: content.to_string(),
        })
    }
    fn fed_event(ev: &Event) -> FedNostrEvent {
        FedNostrEvent {
            raw_json: serde_json::to_string(ev).unwrap(),
        }
    }
    fn enc<T: Serialize>(v: &T) -> Bytes {
        Bytes::from(encode_canonical(v).unwrap().to_vec())
    }

    async fn authorize(state: &Arc<AppState>, actor: &[u8; 32], head_nest_id: &[u8; 32]) {
        state
            .db
            .store_pairing(
                actor,
                head_nest_id,
                &[NOSTR_PUSH.to_string()],
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }

    // (a) the push handler gates on the capability.
    #[tokio::test]
    async fn push_gates_on_nostr_push_capability() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let actor = [0x42u8; 32];
        let kp = keypair(2);
        let req = FedNostrPushRequest {
            actor_id: hex::encode(actor),
            pubkey: pubkey_hex(&kp),
            relay_list: None,
            events: vec![fed_event(&signed(&kp, 1, 1000, vec![], "hi"))],
        };

        // No pairing row → forbidden.
        let err = nostr_push_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.federation.forbidden");

        // Authorize → accepted.
        authorize(&state, &actor, &head).await;
        let reply_bytes = nostr_push_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap();
        let reply: FedNostrPushReply = decode_strict(&reply_bytes).unwrap();
        assert_eq!(reply.accepted, 1);
        assert!(reply.rejected.is_empty());
    }

    // (b) push auto-provisions the proxied account, and refuses to clobber a
    //     deposited row or a row linked to a different pubkey.
    #[tokio::test]
    async fn push_auto_provisions_proxied_and_refuses_to_clobber() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let actor = [0x42u8; 32];
        let actor_hex = hex::encode(actor);
        authorize(&state, &actor, &head).await;

        let kp = keypair(2);
        let req = FedNostrPushRequest {
            actor_id: actor_hex.clone(),
            pubkey: pubkey_hex(&kp),
            relay_list: Some("wss://relay.example".into()),
            events: vec![fed_event(&signed(&kp, 1, 1000, vec![], "hi"))],
        };
        nostr_push_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap();

        // The proxied account row now exists: keyless, mode 'proxied', pubkey set.
        {
            let conn = state.db.conn().await;
            let acct = nostr_db::get_account(&conn, &actor_hex).unwrap().unwrap();
            assert_eq!(acct.signing_mode, "proxied");
            assert_eq!(acct.nostr_pubkey, pubkey_hex(&kp));
            assert!(acct.encrypted_privkey.is_none(), "keyless proxied row");
        }

        // A second push with a DIFFERENT pubkey is refused, not clobbered.
        let other = keypair(3);
        let req_other = FedNostrPushRequest {
            actor_id: actor_hex.clone(),
            pubkey: pubkey_hex(&other),
            relay_list: None,
            events: vec![],
        };
        let err = nostr_push_handler()(state.clone(), head, enc(&req_other))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.federation.forbidden");
        // The original pubkey is intact.
        {
            let conn = state.db.conn().await;
            let acct = nostr_db::get_account(&conn, &actor_hex).unwrap().unwrap();
            assert_eq!(acct.nostr_pubkey, pubkey_hex(&kp));
        }

        // An actor holding a DEPOSITED key is never a proxy target.
        let dep_actor = [0x77u8; 32];
        let dep_hex = hex::encode(dep_actor);
        authorize(&state, &dep_actor, &head).await;
        let dep_kp = keypair(4);
        {
            let conn = state.db.conn().await;
            nostr_db::link_account(
                &conn,
                &dep_hex,
                &pubkey_hex(&dep_kp),
                "custodial",
                Some(&[0xAAu8; 48]),
                None,
                None,
            )
            .unwrap();
        }
        let req_dep = FedNostrPushRequest {
            actor_id: dep_hex,
            pubkey: pubkey_hex(&dep_kp),
            relay_list: None,
            events: vec![],
        };
        let err = nostr_push_handler()(state.clone(), head, enc(&req_dep))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.federation.forbidden");
    }

    // (b2) the auto-provision carries a peer-supplied, unproven pubkey: when
    //      ANOTHER actor on this box already holds it (custodial, deposited
    //      nsec), the provision is refused and that actor's row survives — a
    //      REPLACE over the UNIQUE pubkey would delete it, deposit and all.
    #[tokio::test]
    async fn push_refuses_a_pubkey_another_actor_holds() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let victim_hex = hex::encode([0x77u8; 32]);
        let victim_kp = keypair(4);
        {
            let conn = state.db.conn().await;
            nostr_db::link_account(
                &conn,
                &victim_hex,
                &pubkey_hex(&victim_kp),
                "generated",
                Some(&[0xAAu8; 48]),
                None,
                None,
            )
            .unwrap();
        }

        let attacker = [0x42u8; 32];
        authorize(&state, &attacker, &head).await;
        let req = FedNostrPushRequest {
            actor_id: hex::encode(attacker),
            pubkey: pubkey_hex(&victim_kp),
            relay_list: None,
            events: vec![],
        };
        let err = nostr_push_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.federation.forbidden");

        let conn = state.db.conn().await;
        let victim = nostr_db::get_account(&conn, &victim_hex)
            .unwrap()
            .expect("the victim's row survives");
        assert_eq!(victim.encrypted_privkey.as_deref(), Some(&[0xAAu8; 48][..]));
        assert!(
            nostr_db::get_account(&conn, &hex::encode(attacker))
                .unwrap()
                .is_none(),
            "no row is provisioned for the caller"
        );
    }

    // (c) pushed rows land `origin='federation'` and are excluded from re-export
    //     (a subsequent pull on the receiving box never selects them).
    #[tokio::test]
    async fn pushed_rows_are_federation_origin_and_never_re_exported() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let actor = [0x42u8; 32];
        let actor_hex = hex::encode(actor);
        authorize(&state, &actor, &head).await;

        let kp = keypair(2);
        let pubkey = pubkey_hex(&kp);
        let ev = signed(&kp, 1, 1000, vec![], "authored on the head");
        let req = FedNostrPushRequest {
            actor_id: actor_hex.clone(),
            pubkey: pubkey.clone(),
            relay_list: None,
            events: vec![fed_event(&ev)],
        };
        let reply_bytes = nostr_push_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap();
        let reply: FedNostrPushReply = decode_strict(&reply_bytes).unwrap();
        assert_eq!(reply.accepted, 1);

        let conn = state.db.conn().await;
        // The row rests as ORIGIN_FEDERATION …
        let origin: String = conn
            .prepare("SELECT origin FROM nostr_events WHERE id = ?1")
            .unwrap()
            .query_row([&ev.id], |r| r.get(0))
            .unwrap();
        assert_eq!(origin, nostr::store::ORIGIN_FEDERATION);
        // … so the pull selection (origin='ingest' only) excludes it — a pushed
        // row is never pulled back (echo/loop kill, spec R4).
        let pull_rows =
            nostr::federation::list_events_for_pull(&conn, &pubkey, (0, ""), 100).unwrap();
        assert!(
            pull_rows.is_empty(),
            "a federation-origin row must not be re-exported by the pull leg"
        );
    }

    // (d) the pull handler pages by the compound (stored_at, id) cursor.
    #[tokio::test]
    async fn pull_returns_ingest_rows_and_respects_the_cursor() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let actor = [0x42u8; 32];
        let actor_hex = hex::encode(actor);
        authorize(&state, &actor, &head).await;

        let kp = keypair(2);
        let pubkey = pubkey_hex(&kp);
        // Provision the proxied account so the pull handler resolves the pubkey.
        {
            let conn = state.db.conn().await;
            nostr_db::link_account(&conn, &actor_hex, &pubkey, "proxied", None, None, None)
                .unwrap();
            // Two locally-ingested (origin='ingest') events at controlled
            // stored_at, authored by the actor — the deposit surface a pull drains.
            for (id, sa) in [("id_a", 10i64), ("id_b", 20i64)] {
                conn.execute(
                    "INSERT INTO nostr_events
                         (id, pubkey, kind, created_at, raw_json, derived, stored_at, origin)
                     VALUES (?1, ?2, 1, 0, ?3, 0, ?4, 'ingest')",
                    rusqlite::params![id, pubkey, format!("{{\"id\":\"{id}\"}}"), sa],
                )
                .unwrap();
            }
        }

        // From the zero cursor: both rows, up_to = the last (20, id_b).
        let req = FedNostrPullRequest {
            actor_id: actor_hex.clone(),
            since_stored_at: 0,
            since_id: String::new(),
        };
        let reply: FedNostrPullReply = decode_strict(
            &nostr_pull_handler()(state.clone(), head, enc(&req))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply.events.len(), 2);
        assert_eq!(
            (reply.up_to_stored_at, reply.up_to_id.as_str()),
            (20, "id_b")
        );

        // Strict-after that cursor: nothing left.
        let req2 = FedNostrPullRequest {
            actor_id: actor_hex.clone(),
            since_stored_at: reply.up_to_stored_at,
            since_id: reply.up_to_id.clone(),
        };
        let reply2: FedNostrPullReply = decode_strict(
            &nostr_pull_handler()(state.clone(), head, enc(&req2))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(reply2.events.is_empty());
        // Empty page echoes the request cursor unchanged.
        assert_eq!(
            (reply2.up_to_stored_at, reply2.up_to_id.as_str()),
            (20, "id_b")
        );
    }

    // The pull handler gates on the capability too, and returns an empty page
    // when the actor has no account row yet (a pull before any push).
    #[tokio::test]
    async fn pull_gates_and_empty_page_without_account() {
        let state = public_box().await;
        let head = [0x11u8; 32];
        let actor = [0x42u8; 32];
        let req = FedNostrPullRequest {
            actor_id: hex::encode(actor),
            since_stored_at: 0,
            since_id: String::new(),
        };
        // No pairing → forbidden.
        let err = nostr_pull_handler()(state.clone(), head, enc(&req))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.federation.forbidden");

        // Authorized but no account row → empty page, cursor echoed.
        authorize(&state, &actor, &head).await;
        let reply: FedNostrPullReply = decode_strict(
            &nostr_pull_handler()(state.clone(), head, enc(&req))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(reply.events.is_empty());
        assert_eq!((reply.up_to_stored_at, reply.up_to_id.as_str()), (0, ""));
    }
}

#[cfg(test)]
mod require_foreign_member_tests {
    use std::sync::Arc;

    use super::require_foreign_member;
    use crate::db::CacheDb;
    use crate::db::channels::RebindPower;
    use crate::routes::AppState;

    /// First authenticated contact from the bound home nest PINS the binding
    /// (`federation.md` § Cross-nest shared folders + channel append, the TOFU
    /// bullet): a mismatched origin is refused and stamps nothing;
    /// the bound origin's first served call stamps `confirmed_at`; the stamp
    /// is idempotent. The pin's effect on the rebind arm is pinned in
    /// `db/channels.rs::confirmed_binding_pins_home_nest_against_standing_rebind`,
    /// and the full production seam (a real federated drain confirming the
    /// binding on the channel's home nest) in the tier_3
    /// `conformance_cross_nest_conversations_client.rs`.
    #[tokio::test]
    async fn first_authenticated_contact_confirms_the_binding() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let ch = [0x61u8; 32];
        let member = [0x62u8; 32];
        let home = [0xB1u8; 32];
        let other = [0xB2u8; 32];
        db.register_foreign_channel_member(&ch, &member, &home, None, RebindPower::InsertOnly)
            .await
            .unwrap();

        // A nest that is NOT the recorded home: refused, and no stamp.
        require_foreign_member(&state, &ch, &member, &other)
            .await
            .unwrap_err();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((home, false)),
            "a refused foreign caller must not confirm the binding"
        );

        // The bound home nest's first served call stamps the pin.
        require_foreign_member(&state, &ch, &member, &home)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((home, true)),
            "the bound nest's first served call must stamp the pin"
        );

        // Idempotent on every later call.
        require_foreign_member(&state, &ch, &member, &home)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((home, true))
        );
    }
}

#[cfg(test)]
mod map_inbox_rejection_tests {
    use super::{InboxRejection, map_inbox_rejection};

    /// Pins the wire code `map_inbox_rejection` emits for every
    /// [`InboxRejection`] variant — the federation leg's half of the shared
    /// bucketing in `InboxRejection::disposition`. `inbox_handlers` carries
    /// the sibling pin (`rejection_to_error_tests`) for the same variants on
    /// the client leg — the two intentionally disagree on code, never on
    /// which variant lands in which family.
    #[test]
    fn every_variant_keeps_its_wire_code() {
        let cases: Vec<(InboxRejection, &str)> = vec![
            (
                InboxRejection::BadPayload("x".into()),
                "fauna.federation.invalid_params",
            ),
            (InboxRejection::Blocked, "fauna.federation.forbidden"),
            (InboxRejection::InboxClosed, "fauna.federation.forbidden"),
            (InboxRejection::ContactsOnly, "fauna.federation.forbidden"),
            (
                InboxRejection::KnockPending,
                "fauna.federation.invalid_params",
            ),
            (InboxRejection::UnknownMode, "fauna.federation.forbidden"),
            (
                InboxRejection::QuotaForbidden("x".into()),
                "fauna.federation.forbidden",
            ),
            (
                InboxRejection::RecipientUnknown,
                "fauna.federation.forbidden",
            ),
            (
                InboxRejection::QuotaTooLarge("x".into()),
                "fauna.federation.invalid_params",
            ),
            (
                InboxRejection::KnockTooLarge("x".into()),
                "fauna.federation.invalid_params",
            ),
            (
                InboxRejection::KnockQueueFull,
                "fauna.protocol.rate_limited",
            ),
            (InboxRejection::Storage, "fauna.protocol.internal"),
        ];
        for (rejection, want_code) in cases {
            let debug = format!("{rejection:?}");
            let got = map_inbox_rejection(rejection);
            assert_eq!(got.code, want_code, "{debug} wire code changed");
        }
    }
}

// ── welcome.deliver origin-URL resolution tests ──────
//
// Each test builds one `AppState`, seeds the `nest_addresses` directory, and
// invokes the handler closure directly with a chosen verified `origin_nest_id`
// — the same direct-invocation shape as `nostr_fed_tests`.
#[cfg(test)]
mod welcome_origin_url_tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use std::sync::Arc;

    async fn test_box() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        Arc::new(AppState::for_test(db))
    }

    fn enc<T: Serialize>(v: &T) -> Bytes {
        Bytes::from(encode_canonical(v).unwrap().to_vec())
    }

    /// Shorthand for the welcome-relay request the origin-URL tests drive —
    /// only the fields those tests vary.
    fn welcome_req(
        recipient: &[u8; 32],
        declared_origin: Option<&str>,
    ) -> FedWelcomeDeliverRequest {
        FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(recipient),
            channel_id: Some(hex::encode([0x0Au8; 32])),
            welcome_bytes: vec![1, 2, 3],
            channel_type: Some("dm".to_string()),
            group_id: None,
            origin_nest_url: declared_origin.map(str::to_string),
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        }
    }

    /// The `nest_url` the recipient's one stored Welcome envelope carries.
    async fn delivered_welcome_nest_url(
        state: &Arc<AppState>,
        recipient: &[u8; 32],
    ) -> Option<String> {
        let rows = state.db.list_inbox_all(recipient).await.unwrap();
        assert_eq!(rows.len(), 1, "exactly one welcome landed");
        let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&rows[0].1).unwrap();
        env.decode_welcome().unwrap().nest_url
    }

    /// **The relayed Welcome's home URL is bound to the VERIFIED origin, not
    /// the request's declared string**. `req.origin_nest_url` is peer-declared; the connection's
    /// handshake-verified `origin_nest_id` is not. Once the directory holds a
    /// dial-**proven** address for that identity, the handler forwards it and
    /// the per-request declaration can no longer point the recipient's drain
    /// at an arbitrary URL — it is retained only as an unproven sighting.
    #[tokio::test]
    async fn welcome_deliver_forwards_the_proven_origin_address_over_the_declared_one() {
        let state = test_box().await;
        let origin = [0x77u8; 32];
        let recipient = [0x42u8; 32];
        state
            .db
            .create_user(&recipient, "free", "bob")
            .await
            .unwrap();
        state
            .db
            .record_nest_address(&origin, "https://origin.example", true)
            .await
            .unwrap();

        welcome_deliver_handler()(
            state.clone(),
            origin,
            enc(&welcome_req(&recipient, Some("https://attacker.example"))),
        )
        .await
        .expect("welcome delivers");

        assert_eq!(
            delivered_welcome_nest_url(&state, &recipient)
                .await
                .as_deref(),
            Some("https://origin.example"),
            "the peer-declared URL was forwarded verbatim over the verified \
             origin's dial-proven address"
        );
        // The declaration survives only as an unproven sighting: the proven
        // address still sorts first for every directory reader.
        let candidates = state
            .db
            .resolve_foreign_nest_urls(&origin, 10)
            .await
            .unwrap();
        assert_eq!(
            candidates.first().map(String::as_str),
            Some("https://origin.example")
        );
        assert!(
            candidates.iter().any(|u| u == "https://attacker.example"),
            "the declared URL should be retained as an (unproven) sighting"
        );
    }

    /// First contact — no directory entry for the verified origin — forwards
    /// the declared URL (an honest first cross-nest invite must route) and
    /// records it as an unproven sighting bound to that identity.
    #[tokio::test]
    async fn welcome_deliver_first_contact_forwards_the_declared_url_and_records_the_sighting() {
        let state = test_box().await;
        let origin = [0x66u8; 32];
        let recipient = [0x43u8; 32];
        state
            .db
            .create_user(&recipient, "free", "bob")
            .await
            .unwrap();

        welcome_deliver_handler()(
            state.clone(),
            origin,
            enc(&welcome_req(&recipient, Some("https://first.example"))),
        )
        .await
        .expect("welcome delivers");

        assert_eq!(
            delivered_welcome_nest_url(&state, &recipient)
                .await
                .as_deref(),
            Some("https://first.example")
        );
        let candidates = state
            .db
            .resolve_foreign_nest_urls(&origin, 10)
            .await
            .unwrap();
        assert_eq!(
            candidates,
            vec!["https://first.example".to_string()],
            "first contact records the declared URL as a sighting"
        );
    }

    /// A declared URL that IS one of the verified origin's dial-proven
    /// addresses is forwarded as declared — among its own proven addresses the
    /// origin's current self-claim wins (a genuine migration between two
    /// proven addresses must not be pinned to the older one).
    #[tokio::test]
    async fn welcome_deliver_declared_url_matching_a_proven_row_is_forwarded() {
        let state = test_box().await;
        let origin = [0x55u8; 32];
        let recipient = [0x44u8; 32];
        state
            .db
            .create_user(&recipient, "free", "bob")
            .await
            .unwrap();
        state
            .db
            .record_nest_address(&origin, "https://old.example", true)
            .await
            .unwrap();
        state
            .db
            .record_nest_address(&origin, "https://new.example", true)
            .await
            .unwrap();

        welcome_deliver_handler()(
            state.clone(),
            origin,
            enc(&welcome_req(&recipient, Some("https://new.example"))),
        )
        .await
        .expect("welcome delivers");

        assert_eq!(
            delivered_welcome_nest_url(&state, &recipient)
                .await
                .as_deref(),
            Some("https://new.example")
        );
    }
}
