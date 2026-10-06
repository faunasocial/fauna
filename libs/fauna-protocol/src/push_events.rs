//! Typed push event payloads. Per spec § 2.5.
//!
//! Each struct corresponds to a single `Push.kind` string. The
//! `PushEvent` enum (defined later in this file as types are added)
//! is the application-facing typed union; an `Unknown` variant catches
//! fork-extension kinds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

use crate::ByteBuf;
use crate::notifications::NotifType;
use crate::unknown::Unknown;

/// A 32-byte `ActorId` as 64 lowercase hex chars — rides as a CBOR **text
/// string** (major type 3), the second of the two actor-id wire shapes
/// (`ByteBuf` and `ActorId` = byte string, this = text); push payloads chose
/// text for human readability. The rule: `docs/goal/architecture/serialization.md`
/// § Canonical IPLD dag-cbor, "Fixed-size byte arrays".
pub type ActorIdHex = String;
pub type ContentIdHex = String;

/// `fauna.knock` — contact request notification.
///
/// `Default` exists so a construction site names only the fields it cares
/// about (`..Default::default()`): a later optional field then lands without
/// touching every site.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnockPayload {
    pub sender_id: ActorIdHex,
    /// The knocker's own message text.
    pub summary: String,
    /// The knock row's sentence as a catalog key plus data args — the same
    /// `notifications.row_knock` body the knock's notification row carries
    /// ([`crate::notifications::NotifItem::body`]), so an OS-level knock
    /// notification is localized exactly like the row it announces
    /// (`behavior/notifications.md` § Localized body). Absent when the knock carries no body; the client then falls back to
    /// the summary.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub body: Option<crate::LocalizedText>,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.account.update` — account state change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUpdatedPayload {
    pub changes: Vec<String>,
    pub timestamp: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.notification` — unified notification record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationPayload {
    pub notification_id: i64,
    /// A string on the wire, read as the shared
    /// [`NotifType`](crate::notifications::NotifType) — the same field, same
    /// carrying arm, as [`crate::notifications::NotifItem::notif_type`].
    pub notif_type: NotifType,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sender_id: Option<ActorIdHex>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_id: Option<ContentIdHex>,
    pub summary: String,
    /// The localized twin of `summary` — the same field, same rule, as
    /// [`crate::notifications::NotifItem::body`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub body: Option<crate::LocalizedText>,
    pub timestamp: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.wake` — P2P tunnel signaling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerWakePayload {
    pub requester_actor_id: ActorIdHex,
    pub requester_endpoint: String,
    pub nonce: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.calendar.changed` — a durable write landed in one of the actor's
/// calendars (event PUT create/update, event DELETE, calendar
/// provision/metadata update — the `fauna.bridges.*` CalDAV write RPCs,
/// whether driven by a Fauna app or by an external MUA through the MDA).
/// Routed to the **calendar owner's own connected clients** only
/// (`notify_push`'s single-actor axis; the roster is sealed, so no other
/// recipient is even expressible). Cross-actor RSVP/invite propagation rides
/// the iMIP `REPLY` email path (`docs/goal/behavior/caldav-server.md` § The
/// one operation with a cost), whose inbox landing fires
/// `fauna.mail.received` at the organizer — the organizer's client merge +
/// re-PUT then fires this kind at the organizer's own devices.
///
/// Best-effort nudge: consumers re-fetch/re-sync the calendar surface
/// (`sync_calendar_since` or a full re-list); the quick-appearance poll
/// (where built) and the reconnect re-pull are the correctness backstop.
///
/// Minimal payload by design: `actor_id` + `calendar_id` are exactly what the
/// sealed write path already carries in plaintext — this kind adds **zero**
/// new plaintext to the wire. Event-level scoping (`uid_hash`) is a possible
/// additive future field, deliberately omitted from v1. Per
/// `docs/goal/architecture/transport.md` § Push events (ratified 2026-07-17).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarChangedPayload {
    pub actor_id: ActorIdHex,
    /// Hex-encoded 32-byte calendar id (the sealed store's plaintext routing
    /// key, client-assigned at `provision_calendar`).
    pub calendar_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.addressbook.changed` — a durable write landed in one of the actor's
/// address books (card PUT create/update, card DELETE, book
/// provision/metadata update — the `fauna.bridges.*` CardDAV write RPCs,
/// whether driven by a Fauna app or by an external CardDAV MUA through the
/// MDA). The carddav twin of [`CalendarChangedPayload`], ratified as part of
/// the content-index third-ingest-class ruling: the calendar verdict transfers
/// point for point.
///
/// Routed to the **address book owner's own connected clients** only
/// (`notify_push`'s single-actor axis; the roster is sealed, so no other
/// recipient is even expressible). There is no cross-actor arm at all — an
/// address book has no invitee analogue to a calendar's attendees.
///
/// Best-effort nudge: the index builder's attach-time reconcile walk and the
/// address-book pages' reconnect re-pulls and nav-in re-reads are the
/// correctness backstop. Clients classify it as [`StaleSurfaces::address_book`].
///
/// Minimal payload by design: `actor_id` + `addressbook_id` are exactly what
/// the sealed write path already carries in plaintext — this kind adds **zero**
/// new plaintext to the wire. Card-level scoping (`uid_hash`) is a possible
/// additive future field, deliberately omitted from v1, exactly as the calendar
/// payload omits its event scoping. Per
/// `docs/goal/architecture/transport.md` § Push events (ratified 2026-08-05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddressBookChangedPayload {
    pub actor_id: ActorIdHex,
    /// Hex-encoded 32-byte address book id (the sealed store's plaintext
    /// routing key, client-assigned at `provision_addressbook`).
    pub addressbook_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.conversations.channel.message` — MLS encrypted channel data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMessagePayload {
    pub channel_id: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.conversations.welcome.received` — MLS Welcome bytes delivered
/// inline. Recipient feeds `welcome_bytes` to `MlsManager::process_welcome`;
/// no follow-up inbox fetch is required for the typed-push path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WelcomePayload {
    #[serde(with = "serde_bytes")]
    pub welcome_bytes: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub nest_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub group_id: Option<String>,
    /// The sharing actor (hex ActorId) — mirrors [`crate::inbox::WelcomeInbox::shared_by`]
    /// so the best-effort push carries exactly what the durable-drain envelope does.
    /// Nest-stamped from the authenticated `welcome.deliver` caller for
    /// `channel_type == "folder"`; drives the recipient contact gate + the
    /// "Shared by ‹handle›" surface (`docs/goal/ui/folders.md` § Sharing).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by: Option<String>,
    /// The sharer's handle — mirrors
    /// [`crate::inbox::WelcomeInbox::shared_by_handle`] (and its join rule:
    /// bare for a local sharer, paired with [`Self::shared_by_domain`] for a
    /// verified cross-nest one) so the best-effort push carries exactly what
    /// the durable-drain envelope does.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by_handle: Option<String>,
    /// The cross-nest sharer's handle domain — mirrors
    /// [`crate::inbox::WelcomeInbox::shared_by_domain`]. `None` same-nest.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_by_domain: Option<String>,
    /// The shared folder's name — mirrors [`crate::inbox::WelcomeInbox::set_name`]
    /// so the best-effort push carries exactly what the durable-drain envelope
    /// does. Nest-resolved from the claimed set's own row (never sender-asserted);
    /// populated for `channel_type == "folder"`. Display-only.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name: Option<String>,
    /// [`Self::set_name`], sealed — mirrors
    /// [`crate::inbox::WelcomeInbox::set_name_sealed`] so the best-effort push
    /// carries exactly what the durable-drain envelope does. Path-sealing
    /// S5c-2.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name_sealed: Option<ByteBuf>,
    /// The convergent salt [`Self::set_name_sealed`] opens under — mirrors
    /// [`crate::inbox::WelcomeInbox::set_name_hash`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub set_name_hash: Option<ByteBuf>,
    /// The recipient's access grant on the shared set — mirrors
    /// [`crate::inbox::WelcomeInbox::access`] so the best-effort push carries
    /// exactly what the durable-drain envelope does. Nest-resolved from the home
    /// nest's role row; **advisory-for-UI only, never an authz input**.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub access: Option<String>,
    /// The home nest's deployment `nest_actor_id` — mirrors
    /// [`crate::inbox::WelcomeInbox::home_nest_actor_id`] (the byte-plane SPKI-pin
    /// trust root for a cross-nest recipient). `None` for same-nest / relay-unaware.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub home_nest_actor_id: Option<String>,
    /// The folder's residency — mirrors [`crate::inbox::WelcomeInbox::residency`].
    /// `None` = not stated, never *full*.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub residency: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.inbox.item` — typed pointer to an HTTP-fetched blob.
/// Replaces the old "raw bytes" InboxItem fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItemPayload {
    pub content_id: ContentIdHex,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind_hint: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.protocol.resync_required` — backpressure-driven resync marker.
/// Sent by the server when push events were dropped on overflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResyncRequiredPayload {
    pub dropped_count: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.mail.received` — a per-record mail arrival for the recipient
/// actor. The per-kind arrival push the `SegmentsChangedPayload` doc
/// below refers to (segment-lifecycle events ≠ per-record arrivals).
/// Fired by the nest's mail-ingest path on every newly-stored inbound
/// message — external bridge inbound AND in-domain `fauna.email.send`
/// local delivery, which share one ingest path. The inbound twin of
/// `fauna.conversations.channel.message`: a connected client reacts by
/// fetching `fauna.email.inbox.fetch` (INBOX-scoped, so a push for a
/// `Junk`-routed arrival self-filters to a no-op fetch); the
/// custodian pull reacts by waking its
/// segment-backup debounce. Best-effort — dropped if the recipient has
/// no live WS connection; the client's periodic inbox poll is the
/// correctness backstop. Per `smtp-server.md` § Inbound client receive.
///
/// Minimal payload: `actor_id` only. The body is informational — both
/// consumers re-fetch/re-diff (the client re-reads `inbox.fetch`, the
/// coordinator re-runs the segment diff), so no uid/mailbox discriminator
/// is carried in v1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailReceivedPayload {
    pub actor_id: ActorIdHex,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.mail.flags_changed` — a flag write changed at least one of the
/// actor's `INBOX` rows, whoever made it: a mail client through the MDA
/// (`fauna.bridges.store_flags`) or another Fauna device through
/// `fauna.email.inbox.mark_seen`. A wake, not a payload, exactly like
/// [`MailReceivedPayload`]: a connected app answers it with one
/// `fauna.email.inbox.flag_changes` call, and makes that call on its periodic
/// backstop too. Best-effort — dropped when the actor has no live session.
/// Per `mail-app-surface.md` § Read state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailFlagsChangedPayload {
    pub actor_id: ActorIdHex,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.push.notification` — one device's push, delivered over its own live
/// connection: the `ws-device` transport (`apps/common.md` § Push
/// Notifications → *Transports*). The nest sends it, instead of dialling any
/// relay, to every live connection of the actor that announced the
/// subscription row's `device_id` (`fauna.push.presence`); nothing is queued
/// when none is live. The strings are the same generic ones a `web-push` row
/// carries — the sync agent that posts it links no MLS, so it can render no
/// more — and `url` is the deep link a tap opens. An app that receives it while
/// open ignores it (its own focus rule owns the banner).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushNotificationPayload {
    pub title: String,
    pub body: String,
    pub url: String,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.sync.chunk.wanted` — the relay's ask to a connection that announced
/// it serves a folder (`fauna.sync.serve.announce`; `file-sync.md` § Relay
/// serving, step (2)). Sent on that one connection only, when a reader's
/// `GET /api/v1/chunks/{hash}?folder=` misses the nest's store. The seat
/// answers on the bulk rail, never in a WS-RPC frame (the 2 MiB frame bound
/// sits below the 8 MiB chunk ceiling): `POST /api/v1/chunks/relay/{request_id}`
/// with the stored chunk as the body, or `DELETE` on the same path when it
/// holds no such chunk (`fauna_nest_http::paths::chunk_store::chunk_relay_answer`).
/// Lossy like every push: a seat that never answers is passed over at the
/// fetch deadline.
pub const KIND_SYNC_CHUNK_WANTED: &str = "fauna.sync.chunk.wanted";

/// The payload of [`KIND_SYNC_CHUNK_WANTED`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncChunkWantedPayload {
    /// The nest's handle for this ask — the answer route's path segment.
    pub request_id: u64,
    /// The folder the ask is for, as the `FolderRef` wire string the seat
    /// announced it under (`local:<folders.id>`). The seat answers only from
    /// that folder's own state.
    pub folder: String,
    /// Hex-encoded **store key** of the wanted chunk — the address the stored
    /// bytes hash to (`ChunkManifest::store_keys()`). The nest checks the
    /// answer against it before serving.
    pub store_key: String,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.sync.changed` — a new sync record landed in a shared/synced file
/// set. The download-half twin of the mail/calendar arrival pushes: the nest's
/// record path (`fauna.sync.changes.record`, and the federation relay into a
/// set homed here) fires this at every **same-nest** participant of the set
/// (owner + roster members) after a durable `sync_changes` insert, so their
/// resident engines schedule an immediate off-cadence pull instead of waiting
/// out the rescan interval (minutes). Best-effort — dropped if a participant
/// has no live WS; the periodic reconcile is the correctness backstop, so a
/// missed push costs only latency (offline devices catch up on cadence).
///
/// Minimal payload: the set's address — its `folder_hash`, plus the `folder`
/// name while the nest still rests one (`path-sealing.md` § the set-name
/// plane). The nest fans out
/// per-participant, so the recipient is implicit and no writer/actor id is
/// needed to route — the engine pulls the whole set and re-diffs. The name is
/// the identifier every same-nest participant binds the set under and every
/// sync IPC verb / engine registry already keys on, and the nest holds it on the
/// set's own row — so the nudge routes with **no id→name resolution on the
/// latency path** (the numeric `folders.id` could be added as an additive
/// field later, when the client engine seam is re-keyed by id, without a wire
/// break). **Same-nest only**: a cross-nest nudge would grow the closed
/// federation kind inventory and needs its own design pass. Per
/// `docs/goal/behavior/file-sync.md` § Remote-change nudge (ratified
/// 2026-07-20) and `docs/goal/architecture/transport.md` § Push events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncChangedPayload {
    /// The set name whose engine should pull — the same name every same-nest
    /// participant binds under and every sync IPC verb keys on. Blank for a
    /// sealed set once the nest no longer rests its plaintext name; the set is
    /// then addressed by [`Self::folder_hash`] alone.
    pub folder: String,
    /// The set's hash address (`fauna_core::path_crypto::set_name_hash` of its
    /// name — the row's `folders.name_hash`), the address a receiver matches
    /// its bindings and rows on (`path-sealing.md` § the set-name plane).
    /// Absent on a scope-tagged plane nudge, whose [`Self::scope`] names what
    /// moved. Additive: a receiver that ignores it matches by `folder`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    /// **Scope tag** (W2.3 (account-data-plane.md § Workstreams),
    /// `account-sync-plane.md` § Feeds and cursors → *Feed row + wire
    /// evolution* — *"scope-tagged so a replica pulls only the scope that
    /// moved"*). Present on the generalized
    /// plane's nudges, naming the scope whose feed advanced
    /// (`fauna_protocol::account_state::ACCOUNT_STATE_SCOPE` today). Absent on
    /// every shipped folder nudge, where [`Self::folder`] already names the
    /// scope that moved — so this is additive in the strict sense: no existing
    /// nudge changes shape, and a client that ignores it pulls the set it
    /// always pulled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl SyncChangedPayload {
    /// Whether this nudge names the set a receiver knows as `name` — by
    /// [`Self::folder_hash`] when the nest sent one (the only address a sealed
    /// set's nudge carries), else by the plaintext `folder`. The one match
    /// every receiver makes: the agent's binding lookup and each app's
    /// expanded-row gate.
    pub fn names_set(&self, name: &str) -> bool {
        match &self.folder_hash {
            Some(hash) => {
                hash.as_slice() == fauna_core::path_crypto::set_name_hash(name).as_slice()
            }
            None => !self.folder.is_empty() && self.folder == name,
        }
    }

    /// The scope-tagged nudge as a third-party principal receives it: the
    /// scope that moved and nothing else — the required `folder` empty, no
    /// hash address
    /// (`transport.md` § Push events → *Third-party event doors*).
    #[must_use]
    pub fn scope_nudge(scope: &str) -> Self {
        Self {
            scope: Some(scope.to_string()),
            ..Default::default()
        }
    }
}

/// `fauna.events.poll` — the events doors' one kind (`transport.md` § Push
/// events → *Third-party event doors*): which of the principal's scopes moved
/// past `cursor`. Answered at once over WS-RPC; the HTTP door
/// `GET /api/v1/events` wraps it as a long-poll.
pub const KIND_EVENTS_POLL: &str = "fauna.events.poll";

/// `fauna.events.poll` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventsPollRequest {
    /// The `cursor` of the previous reply; absent (or 0) = from the start, so
    /// the first poll names every reachable scope that holds a row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.events.poll` reply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventsPollReply {
    /// One [`SyncChangedPayload::scope_nudge`] per reachable scope whose feed
    /// advanced past the request's cursor, in feed order. Never content.
    pub frames: Vec<SyncChangedPayload>,
    /// The nest-log position this reply covers — the feed coordinate
    /// `fauna.sync.changes.list`'s `since` walks. The next poll sends it back;
    /// a reply with no frames echoes the request's cursor.
    pub cursor: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.segments.changed` — segment-store-level events the per-kind
/// arrival pushes (`fauna.mail.received` etc.) don't cover: finalize,
/// compaction in/out, tombstone. Per-record appends still ride the
/// existing per-kind push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentsChangedPayload {
    /// `"mail"` (Plan 5); `"conv"` / `"cal"` / `"post"` once those
    /// kinds roll out on the segment store.
    pub kind: String,
    pub actor_id: ActorIdHex,
    pub segment_id: u32,
    pub change: SegmentChange,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentChange {
    /// The currently-open segment for the actor's open bucket closed
    /// (rotation on bucket change, or explicit `finalize_open`).
    Finalized,
    /// A compaction run produced this segment by merging inputs from
    /// the same bucket.
    CompactedIn,
    /// A compaction run consumed this segment as input and moved it to
    /// the per-kind `tombstoned_segments` list (awaiting 14-d retention
    /// GC).
    CompactedOut,
    /// The segment moved to `tombstoned_segments` via an explicit
    /// tombstone path (not through compaction).
    Tombstoned,
}

/// `fauna.delegation.lease_changed` — the task-delegation lease for `task_kind`
/// changed hands (a heartbeat recorded a *different* holder). A best-effort
/// nudge to the actor's connected clients to re-observe that kind's lease
/// promptly (via `fauna.delegation.observe`) instead of waiting for their poll;
/// the observe poll is the correctness backstop. Fanned to **all** the actor's
/// connections (including the one that caused the change — it ignores its own
/// echo). **No lease state inline** — the receiver re-reads it, so a dropped
/// push self-heals. Per `docs/goal/behavior/participants.md` § Coordination
/// primitive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseChangedPayload {
    pub task_kind: String,
    /// Forward-compat: preserved unknown CBOR-map keys (per § 2.3 rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Application-facing typed union ────────────────────────────────

/// Decoded push event. Matches `Push.kind` to a typed payload, falls
/// through to `Unknown` for fork-extension or future-version kinds.
///
/// `Serialize` is **untagged on purpose**: serializing a `PushEvent` yields the
/// variant's payload object *bare*, with no enum wrapper. Paired with
/// [`PushEvent::kind`], that is exactly the `(kind, payload)` face the wasm
/// client hands the web SPA (`fauna_rpc_wasm`'s `set_on_push_event`) — so a new
/// variant reaches JS with no per-variant match to update anywhere. There is
/// deliberately **no** `Deserialize`: untagged deserialization would be
/// ambiguous across payloads that share a field shape, and the wire direction is
/// already owned by [`PushEvent::from_push`], which dispatches on the kind
/// string.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PushEvent {
    Knock(KnockPayload),
    AccountUpdated(AccountUpdatedPayload),
    Notification(NotificationPayload),
    PeerWake(PeerWakePayload),
    /// `fauna.calendar.changed` — a durable write landed in one of the
    /// actor's calendars; own-device fanout. See [`CalendarChangedPayload`].
    CalendarChanged(CalendarChangedPayload),
    /// `fauna.addressbook.changed` — a durable write landed in one of the
    /// actor's address books; own-device fanout. See
    /// [`AddressBookChangedPayload`].
    AddressBookChanged(AddressBookChangedPayload),
    ChannelMessage(ChannelMessagePayload),
    Welcome(WelcomePayload),
    InboxItem(InboxItemPayload),
    ResyncRequired(ResyncRequiredPayload),
    SegmentsChanged(SegmentsChangedPayload),
    MailReceived(MailReceivedPayload),
    /// `fauna.mail.flags_changed` — an `INBOX` flag write for the actor; a
    /// wake for `fauna.email.inbox.flag_changes`. See
    /// [`MailFlagsChangedPayload`].
    MailFlagsChanged(MailFlagsChangedPayload),
    /// `fauna.sync.changed` — a sync record landed in a shared/synced set;
    /// same-nest participant fan-out. A receiving engine pulls the set
    /// immediately. See [`SyncChangedPayload`].
    SyncChanged(SyncChangedPayload),
    /// `fauna.bridges.push.mailbox_state` — IDLE/NOTIFY mailbox-state
    /// change. Routed to MDA WS connections that registered interest
    /// via `fauna.bridges.subscribe_mailbox_state`. Per
    /// `docs/goal/behavior/imap-server.md` § IDLE / NOTIFY.
    BridgeMailboxState(crate::bridge_routing::BridgeMailboxStatePush),
    /// `fauna.bridges.config_changed` — bridge-relevant configuration
    /// changed (mail-enable toggle, local-domain add/remove, policy edit).
    /// Routed to every approved bridge's WS connection; the bridge re-fetches
    /// `fauna.bridges.fetch_config` and re-applies in-process. Per
    /// `docs/goal/behavior/mail-bridge-lifecycle.md` § Running.
    BridgeConfigChanged(crate::bridge_routing::BridgeConfigChangedPush),
    /// `fauna.bridges.atproto.sessions_changed` — an account's external-app
    /// state changed (session/credential revoke, kill-switch flip). Routed to
    /// every approved `atproto.pds` bridge; the bridge drops its cached
    /// sessions/flag for that account and re-fetches (nudge + poll-fallback,
    /// the `mailbox_state` pattern). Per `atproto-pds-full.md` § WS-RPC kind
    /// surface → Push events.
    BridgeAtprotoSessionsChanged(crate::atproto_pds::BridgeAtprotoSessionsChangedPush),
    /// `fauna.bridges.atproto.projection_ready` — projection-relevant content
    /// landed for a local account (public-post create, post-delete tombstone,
    /// profile set). Routed to every approved `atproto.pds` bridge, which
    /// pulls `fauna.bridges.atproto.fetch_public_posts` / `fetch_profile`
    /// from its stored cursor (nudge + poll-fallback, the `outbound_ready`
    /// pattern). Per `atproto-pds-bridge.md` § Where logic lives.
    BridgeAtprotoProjectionReady(crate::atproto_pds::BridgeAtprotoProjectionReadyPush),
    /// `fauna.bridges.atproto.issuer_key_rotated` — the admin rotated the
    /// NEST's OAuth issuer key set, through either arm (TP5 / S2d leg 1).
    /// Routed to every approved `atproto.pds` bridge, which re-fetches
    /// `fauna.bridges.atproto.fetch_issuer_jwks` and replaces the set it
    /// verifies nest-minted access tokens against. Hint-less, best-effort
    /// (nudge + poll-fallback, the `projection_ready` pattern) — but see that
    /// push's own doc for why the pickup latency matters here.
    BridgeAtprotoIssuerKeyRotated(crate::atproto_pds::BridgeAtprotoIssuerKeyRotatedPush),
    /// `fauna.atproto.consent_requested` — an external OAuth client is asking
    /// to act as this account (F4 rung 2). Own-device fanout to the account's
    /// own logged-in clients, which render it as the approval card carrying the
    /// binding code. Per `atproto-pds-full.md` § WS-RPC kind surface → Push
    /// events; poll-fallback is
    /// `fauna.bridges.atproto.list_pending_consents`.
    AtprotoConsentRequested(crate::atproto_pds::AtprotoConsentRequestedPush),
    /// `fauna.bridges.atproto.permission_set_requested` — the nest's
    /// `/oauth/par` asks the PDS bridge to resolve one `include:<NSID>`. A
    /// request rather than a nudge: the bridge answers with the BRIDGE-class
    /// kind `fauna.bridges.atproto.deliver_permission_set`, correlated by
    /// `request_id`, and the nest's deadline is the only fallback.
    BridgeAtprotoPermissionSetRequested(
        crate::atproto_pds::BridgeAtprotoPermissionSetRequestedPush,
    ),
    /// `fauna.bridges.outbound_ready` — a remote-recipient row was just
    /// enqueued nest-side (interactive `fauna.email.send`). Routed to the
    /// MTA-role bridge's WS connection to nudge its outbound worker to drain
    /// promptly instead of waiting for the next `fetch_outbound_due` poll.
    /// Best-effort; the poll is the backstop. Per
    /// `docs/goal/behavior/smtp-server.md` § Outbound delivery.
    BridgeOutboundReady(crate::bridge_routing::BridgeOutboundReadyPush),
    /// `fauna.bridges.rescore_ready` — a freshly-delivered mail item seeded a
    /// new per-user re-score obligation at ingest (a `labeler:<hex>` backlog
    /// row). Routed to the MDA-role / content-processor bridge to nudge its
    /// re-score drain worker to run promptly instead of waiting for the next
    /// startup / `config_changed` / 12 h trigger. Best-effort; the drain's
    /// periodic triggers are the backstop. Per
    /// `docs/goal/architecture/content-scoring.md` § Timing.
    BridgeRescoreReady(crate::bridge_routing::BridgeRescoreReadyPush),
    /// `fauna.bridges.spam_baseline_publish` — an admin `publish_spam_baseline`
    /// opened a pending run over client-sealed contributor copies. Routed to
    /// the MDA-role / content-processor bridge, which pulls the grant-gated
    /// worklist (`fauna.capabilities.spam_baseline_worklist`), unseal-merges
    /// the copies off-box, and submits its merged half
    /// (`fauna.capabilities.submit_spam_baseline`) before the publish
    /// handler's bounded await elapses. Per `docs/goal/behavior/mail-spam.md`
    /// § Encrypted-mode interaction (ratified 2026-07-13).
    BridgeSpamBaselinePublish(crate::bridge_routing::BridgeSpamBaselinePublishPush),
    /// `fauna.bridges.push.spam_model_updated` — the actor's training history
    /// changed (a sealed `put_spam_model` carrying a history insert or delete). Routed
    /// to the actor's own connected clients so their open `mail-spam` page
    /// refreshes the training-history list. Per `docs/goal/behavior/mail-spam.md`
    /// §§ Training signal sources, Undo.
    BridgeSpamModelUpdated(crate::bridge_routing::BridgeSpamModelUpdatedPush),
    /// `fauna.bridges.push.spam_model_reset` — the actor reset their per-user
    /// spam model (model + history deleted). Routed to the actor's own clients
    /// so other open surfaces clear their history list. Per
    /// `docs/goal/behavior/mail-spam.md` § Reset.
    BridgeSpamModelReset(crate::bridge_routing::BridgeSpamModelResetPush),
    /// `fauna.delegation.lease_changed` — a task-delegation lease changed hands;
    /// the actor's clients re-observe via `fauna.delegation.observe`. Per
    /// `docs/goal/behavior/participants.md` § Coordination primitive.
    LeaseChanged(LeaseChangedPayload),
    /// `fauna.bridges.push.import_progress` — mailbox-import counters moved.
    /// Routed to the importer's own connected clients after each
    /// `import_message` / `import_message_batch`. Per
    /// `docs/goal/behavior/mailbox-migration.md` § Progress lives nest-side.
    BridgeImportProgress(crate::bridge_routing::BridgeImportProgressPush),
    /// `fauna.bridges.push.import_error` — an import session recorded a
    /// session-fatal error (`fail_import_session`).
    BridgeImportError(crate::bridge_routing::BridgeImportErrorPush),
    /// `fauna.bridges.push.import_complete` — an import session finalized;
    /// the payload carries the end-of-import summary counters.
    BridgeImportComplete(crate::bridge_routing::BridgeImportCompletePush),
    /// `fauna.bridges.push.export_progress` — mailbox-export counters moved.
    /// Routed to the exporter's own connected clients after each accepted
    /// `upload_export_chunk`. Per `docs/goal/behavior/mail-export.md`
    /// § Session row model.
    BridgeExportProgress(crate::bridge_routing::BridgeExportProgressPush),
    /// `fauna.bridges.push.export_error` — an export session recorded a
    /// session-fatal error.
    BridgeExportError(crate::bridge_routing::BridgeExportErrorPush),
    /// `fauna.bridges.push.export_complete` — an export session finalized and
    /// its blob carries a terminator frame, so the download is openable
    /// (§ Blob shape on disk). Carries the actor-authenticated download path.
    BridgeExportComplete(crate::bridge_routing::BridgeExportCompletePush),
    /// `fauna.bridges.push.conversation_changed` — a bridged room took a
    /// deposit, a shape or membership change, or a receipt
    /// (`apps/bridges.md` § Bridge-kind catalogue → Phase G). The nudge to the
    /// account's own clients; carries the room id and no content.
    BridgeConversationChanged(crate::bridged_conversations::ConversationChangedPush),
    /// `fauna.push.notification` — a `ws-device` push row's delivery over the
    /// device's own live connection. See [`PushNotificationPayload`].
    PushNotification(PushNotificationPayload),
    /// `fauna.sync.chunk.wanted` — the relay asks a connection that announced
    /// a folder for one of its chunks. See [`SyncChunkWantedPayload`].
    SyncChunkWanted(SyncChunkWantedPayload),
    Unknown(Unknown),
}

/// The client data surfaces a push has made stale — the fleet's one answer to
/// *"what do I have to re-read now?"*.
///
/// Every app used to derive this for itself. tui's `Resync` carried the tax in
/// its own doc comment — *"the tui twin of linux's central `handle_ws_event`
/// re-fetch set: keep the two in step when either grows a surface"* — and
/// `transport.md` § Push events tracks the same invariant in prose for all
/// seven, repeating the phrase "matching the linux re-fetch set" per app. Asking
/// seven hand-written matches to agree is not a mechanism, and it had already
/// failed twice by the time this type landed: linux's reconnect sweep was
/// missing `CalendarChanged`'s backstop (patched by hand, its comment still says
/// so), and both of linux's recovery paths were missing [`Self::media`] and
/// [`Self::atproto`] outright.
///
/// **What this type is not.** It answers staleness, never *side effects*. A
/// desktop toast, the sync engine's `pull_set_now` nudge, and the
/// payload-parameterized per-folder device fetch all stay at the call site,
/// because they are per-app or per-payload. What is stale is neither: it follows
/// from the wire kind alone, so it belongs here beside the kind.
///
/// **Where the flags stop.** These are *logical* surfaces, not any app's widget
/// tree. An app folds them onto whatever it actually re-reads — tui serves
/// [`Self::knocks`] and [`Self::contacts`] from one roster op, linux serves
/// `contacts` with a contacts + member-reviews pair — and an app that has not
/// built a surface simply ignores its flag. That is why an app with no Media
/// page needs no arm here, and why adding one costs nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StaleSurfaces {
    /// The post feed. **No push feeds it** — so it is stale only after a
    /// reconnect, never from a push (see [`Self::on_reconnect`]). [`Self::family`]
    /// is the other such surface.
    pub feed: bool,
    /// The unified notification list.
    pub notifications: bool,
    /// Pending knocks (contact requests). Its own flag rather than a fold into
    /// [`Self::contacts`]: a knock arriving changes the knock list and nothing
    /// about the roster, and linux re-reads the two separately.
    pub knocks: bool,
    /// The contact roster, and any member-review queue an app hangs off it.
    pub contacts: bool,
    /// The account cell — quota, tier, handle.
    pub account: bool,
    /// The Settings → AT Protocol page, whose state is the pending-consent card
    /// list. Its own flag rather than a fold into [`Self::account`]: that one
    /// re-reads the quota and nothing else, while the consent cards come only
    /// from `list_pending_consents`.
    pub atproto: bool,
    /// The Events page — the calendar list and the events inside it.
    pub events: bool,
    /// The Contacts page's Address Book segment — the actor's CardDAV books and
    /// the cards inside the open one. The carddav twin of [`Self::events`].
    ///
    /// ⚠ Apps page-gate this one, for [`Self::media`]'s reason: the nest fires
    /// one `fauna.addressbook.changed` per durable card write, so a contacts
    /// app's first sync — which PUTs every card on the phone — emits hundreds,
    /// and each would otherwise re-read and unseal every card for a page nobody
    /// is looking at. Arriving-while-elsewhere needs no refresh: entering the
    /// Address Book already re-reads it.
    pub address_book: bool,
    /// The Media page's cross-*set* aggregate (`fauna.media.list`).
    ///
    /// ⚠ Apps page-gate this one, and that is a correctness requirement rather
    /// than an optimization: the nest fires one `SyncChanged` per recorded file,
    /// so an initial folder sync emits thousands, and each would otherwise
    /// re-read every set for a page nobody is looking at. Arriving-while-
    /// elsewhere needs no refresh — every app already re-reads on nav-to-Media.
    pub media: bool,
    /// The ward's supervision read — `fauna.family.status`: the
    /// `supervised-indicator`, the `family-tab` gate, and the three
    /// client-enforced guardian pillars (the content floor, `content_notify`,
    /// the screen-time policy). Like [`Self::feed`], **no push feeds it**: a
    /// guardian's policy edit reaches the ward only by re-reading, and
    /// `family-client-enforcement.md` § Content policy's unfetched-policy
    /// ruling names exactly two moments that read fires — cold launch and WS
    /// reconnect. The second one is this flag (see [`Self::on_reconnect`]);
    /// an app that hand-derives its sweep fires the same read from its own
    /// reconnect trigger. The failed-read arms are what make the re-fire safe:
    /// every app's status producer keeps last-known state on an error, so the
    /// sweep can only ever move enforcement state on a successful reply.
    pub family: bool,
}

impl StaleSurfaces {
    /// Nothing is stale. The answer for every kind whose consumer lives
    /// somewhere other than a snapshot surface.
    pub const NONE: Self = Self {
        feed: false,
        notifications: false,
        knocks: false,
        contacts: false,
        account: false,
        atproto: false,
        events: false,
        address_book: false,
        media: false,
        family: false,
    };

    /// **Everything.** A reconnect resets the push `seq` to 0
    /// (`transport.md` § Push events), so the gap may have swallowed any push at
    /// all — including the feed's, which nothing else recovers, and the two
    /// surfaces no push feeds at all (feed, family), which only this sweep ever
    /// refreshes. The invariant worth keeping: this covers every kind's own
    /// set, pinned by `a_reconnect_covers_every_kind`; that the family read
    /// rides it, and nothing else, is pinned by
    /// `a_reconnect_stales_the_family_surface_and_no_push_does`.
    pub const fn on_reconnect() -> Self {
        Self {
            feed: true,
            notifications: true,
            knocks: true,
            contacts: true,
            account: true,
            atproto: true,
            events: true,
            address_book: true,
            media: true,
            family: true,
        }
    }

    /// The same classification [`PushEvent::invalidates`] answers, keyed by the
    /// wire kind string instead of the decoded variant — the entry point for a
    /// boundary that never sees the Rust enum: the wasm client hands the SPA a
    /// bare `(kind, payload)` (`PushEvent` is `#[serde(untagged)]` with no
    /// `Deserialize`, see its doc comment), and the UniFFI apps classify off
    /// `FfiPushEvent`, which flattens most kinds away into `Other { kind }`.
    /// Both boundaries already carry the kind string, so this is the one place
    /// they both call instead of each re-deriving the mapping — which is
    /// exactly the failure this type exists to close (see the type's own doc
    /// comment).
    ///
    /// ⚠ **This match cannot share [`PushEvent::invalidates`]'s compile-time
    /// exhaustiveness** — a `&str` pattern has no way to be checked against
    /// future `PushEvent` variants, so a new surface-affecting kind that forgets
    /// an arm here would silently fall through to [`Self::NONE`] instead of
    /// failing to compile. The property `for_kind(event.kind()) ==
    /// event.invalidates()` is enforced by a test instead
    /// (`for_kind_matches_invalidates`) — the same trade this file already
    /// makes for [`Self::on_reconnect`]'s coverage property, pinned by
    /// `a_reconnect_covers_every_kind` rather than by the type system. **Adding
    /// a surface to [`PushEvent::invalidates`] means adding the same kind string
    /// here too.**
    pub fn for_kind(kind: &str) -> Self {
        match kind {
            "fauna.knock" => Self {
                knocks: true,
                ..Self::NONE
            },
            "fauna.account.update" => Self {
                account: true,
                ..Self::NONE
            },
            "fauna.notification" => Self {
                notifications: true,
                ..Self::NONE
            },
            "fauna.calendar.changed" => Self {
                events: true,
                ..Self::NONE
            },
            "fauna.addressbook.changed" => Self {
                address_book: true,
                ..Self::NONE
            },
            "fauna.atproto.consent_requested" => Self {
                atproto: true,
                ..Self::NONE
            },
            "fauna.sync.changed" => Self {
                media: true,
                ..Self::NONE
            },
            "fauna.protocol.resync_required" => Self {
                feed: false,
                family: false,
                ..Self::on_reconnect()
            },
            _ => Self::NONE,
        }
    }

    /// Whether `self` stales at least everything `other` does — the subset test
    /// the sweep invariants are written against.
    pub const fn covers(&self, other: &Self) -> bool {
        (self.feed || !other.feed)
            && (self.notifications || !other.notifications)
            && (self.knocks || !other.knocks)
            && (self.contacts || !other.contacts)
            && (self.account || !other.account)
            && (self.atproto || !other.atproto)
            && (self.events || !other.events)
            && (self.address_book || !other.address_book)
            && (self.media || !other.media)
            && (self.family || !other.family)
    }

    /// Both sets' surfaces — for a caller accumulating several pushes into one
    /// refresh pass.
    pub const fn union(self, other: Self) -> Self {
        Self {
            feed: self.feed || other.feed,
            notifications: self.notifications || other.notifications,
            knocks: self.knocks || other.knocks,
            contacts: self.contacts || other.contacts,
            account: self.account || other.account,
            atproto: self.atproto || other.atproto,
            events: self.events || other.events,
            address_book: self.address_book || other.address_book,
            media: self.media || other.media,
            family: self.family || other.family,
        }
    }

    /// Whether anything at all is stale — lets a caller skip a refresh pass.
    pub const fn any(&self) -> bool {
        !self.covers_nothing()
    }

    const fn covers_nothing(&self) -> bool {
        !self.feed
            && !self.notifications
            && !self.knocks
            && !self.contacts
            && !self.account
            && !self.atproto
            && !self.events
            && !self.address_book
            && !self.media
            && !self.family
    }
}

impl PushEvent {
    /// The client surfaces this push has made stale.
    ///
    /// **The match below is exhaustive on purpose — no wildcard arm.** That is
    /// the whole mechanism: this file is the one place a `PushEvent` variant is
    /// ever added, so a new kind that forgets to declare its surfaces is a
    /// compile error here rather than a surface that silently never refreshes on
    /// one app. A kind whose consumer lives elsewhere still gets an arm, saying
    /// [`StaleSurfaces::NONE`] out loud.
    pub fn invalidates(&self) -> StaleSurfaces {
        match self {
            PushEvent::Knock(_) => StaleSurfaces {
                knocks: true,
                ..StaleSurfaces::NONE
            },
            PushEvent::AccountUpdated(_) => StaleSurfaces {
                account: true,
                ..StaleSurfaces::NONE
            },
            PushEvent::Notification(_) => StaleSurfaces {
                notifications: true,
                ..StaleSurfaces::NONE
            },
            // A durable write landed in one of this actor's calendars — own
            // other device, or an external MUA through the MDA.
            PushEvent::CalendarChanged(_) => StaleSurfaces {
                events: true,
                ..StaleSurfaces::NONE
            },
            // Its carddav twin: a durable card or book write landed in one of
            // this actor's address books, from another device or an external
            // contacts app through the MDA. The index-side consumer (the
            // contacts reconcile walk) keeps its own trigger in the shared
            // receive loop; this flag is the Address Book page's.
            PushEvent::AddressBookChanged(_) => StaleSurfaces {
                address_book: true,
                ..StaleSurfaces::NONE
            },
            // An external ATProto app is asking to act as this account (F4 rung
            // 2) and a browser is blocked waiting on the answer. Deliberately a
            // *re-list*, not a fold of the payload: `list_pending_consents` is
            // the one place the card set comes from, so a push and a poll can
            // never disagree about what is waiting — and an unassigned request,
            // which fans out to nobody, is only ever found that way.
            PushEvent::AtprotoConsentRequested(_) => StaleSurfaces {
                atproto: true,
                ..StaleSurfaces::NONE
            },
            // A sync record landed in a set this actor participates in — the
            // remote-change nudge (`file-sync.md` § Remote-change nudge), which
            // the nest fires at *every* connected participant, this device
            // included. Media is the one user-facing surface over set
            // *contents*; the engine pull and the per-folder device roster are
            // side effects of the same push, and stay at the call site.
            PushEvent::SyncChanged(_) => StaleSurfaces {
                media: true,
                ..StaleSurfaces::NONE
            },
            // The nest's outbound channel for this subscriber overflowed and it
            // dropped pushes (§ Backpressure and `ResyncRequired`). Everything a
            // *dropped* push could have staled is therefore stale — which is
            // every surface but the two no push feeds (the feed, the family
            // read): a dropped push cannot have staled what no push touches.
            PushEvent::ResyncRequired(_) => StaleSurfaces {
                feed: false,
                family: false,
                ..StaleSurfaces::on_reconnect()
            },

            // ── Kinds whose consumer is not a snapshot surface ──
            //
            // MLS channel data, Welcomes and per-record mail arrivals are owned
            // by the shared conversations receive loop, which holds its own
            // `subscribe_kind`; re-pulling here would double-drive the one MLS
            // engine.
            PushEvent::ChannelMessage(_)
            | PushEvent::Welcome(_)
            | PushEvent::MailReceived(_)
            | PushEvent::MailFlagsChanged(_) => StaleSurfaces::NONE,
            // P2P wake — consumed by the p2p service, never a UI surface.
            PushEvent::PeerWake(_) => StaleSurfaces::NONE,
            // Nest-side planes whose client legs are not built: the inbox item
            // feed, segment-store lifecycle, and
            // delegation leases. Each lands here as an explicit no-op until a
            // session wires its surface.
            PushEvent::InboxItem(_)
            | PushEvent::SegmentsChanged(_)
            | PushEvent::LeaseChanged(_) => StaleSurfaces::NONE,
            // Every `fauna.bridges.*` kind is routed to an approved *bridge's*
            // connection, not a user app's — a bridge is not a client with
            // surfaces. Harmless if one is ever seen here.
            PushEvent::BridgeMailboxState(_)
            | PushEvent::BridgeConfigChanged(_)
            | PushEvent::BridgeAtprotoSessionsChanged(_)
            | PushEvent::BridgeAtprotoProjectionReady(_)
            | PushEvent::BridgeAtprotoIssuerKeyRotated(_)
            | PushEvent::BridgeAtprotoPermissionSetRequested(_)
            | PushEvent::BridgeOutboundReady(_)
            | PushEvent::BridgeRescoreReady(_)
            | PushEvent::BridgeSpamBaselinePublish(_)
            | PushEvent::BridgeImportProgress(_)
            | PushEvent::BridgeImportError(_)
            | PushEvent::BridgeImportComplete(_)
            // The mail-export wizard's own page re-reads from the push
            // payload's counters, not from a list re-fetch — the session row
            // it renders is the one the push describes. Nothing else on any
            // app goes stale, so no surface is named here.
            | PushEvent::BridgeExportProgress(_)
            | PushEvent::BridgeExportError(_)
            | PushEvent::BridgeExportComplete(_) => StaleSurfaces::NONE,
            // A bridged room changed. Its consumer is the shared conversations
            // receive loop's bridged poll (`poll_inbound_bridged`), which holds
            // its own subscription — the `MailReceived` arm's reason.
            PushEvent::BridgeConversationChanged(_) => StaleSurfaces::NONE,
            // A banner for the sync agent to post while no app is attached; the
            // event it announces arrives on its own kind, which is what makes
            // anything stale. An open app ignores this one.
            PushEvent::PushNotification(_) => StaleSurfaces::NONE,
            // A relay ask for the serving engine; it changes no data an app
            // shows (the reader's own GET is what the bytes answer).
            PushEvent::SyncChunkWanted(_) => StaleSurfaces::NONE,
            // The actor's own spam model changed. Routed to their clients for
            // the open `mail-spam` page's training-history list — a surface no
            // app has flagged here yet; it joins the list above when one does.
            PushEvent::BridgeSpamModelUpdated(_) | PushEvent::BridgeSpamModelReset(_) => {
                StaleSurfaces::NONE
            }
            // A wire kind this build has never heard of — logged at the push
            // pump, stale-making nowhere.
            PushEvent::Unknown(_) => StaleSurfaces::NONE,
        }
    }
}

impl PushEvent {
    /// Classify a `Push` frame's kind+payload into a typed variant.
    /// Unknown kinds become `PushEvent::Unknown`.
    pub fn from_push(kind: &str, payload: Value) -> Self {
        // Convert payload to bytes once for the typed-decode attempts.
        let payload_bytes = match crate::codec::encode_canonical(&payload) {
            Ok(b) => b,
            Err(_) => {
                return PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                });
            }
        };
        match kind {
            "fauna.knock" => fauna_cbor::decode_strict::<KnockPayload>(&payload_bytes)
                .map(PushEvent::Knock)
                .unwrap_or_else(|_| {
                    PushEvent::Unknown(Unknown {
                        kind: kind.to_string(),
                        payload,
                    })
                }),
            "fauna.account.update" => {
                fauna_cbor::decode_strict::<AccountUpdatedPayload>(&payload_bytes)
                    .map(PushEvent::AccountUpdated)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.notification" => {
                fauna_cbor::decode_strict::<NotificationPayload>(&payload_bytes)
                    .map(PushEvent::Notification)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.peer.wake" => fauna_cbor::decode_strict::<PeerWakePayload>(&payload_bytes)
                .map(PushEvent::PeerWake)
                .unwrap_or_else(|_| {
                    PushEvent::Unknown(Unknown {
                        kind: kind.to_string(),
                        payload,
                    })
                }),
            "fauna.calendar.changed" => {
                fauna_cbor::decode_strict::<CalendarChangedPayload>(&payload_bytes)
                    .map(PushEvent::CalendarChanged)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.addressbook.changed" => {
                fauna_cbor::decode_strict::<AddressBookChangedPayload>(&payload_bytes)
                    .map(PushEvent::AddressBookChanged)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.conversations.channel.message" => {
                fauna_cbor::decode_strict::<ChannelMessagePayload>(&payload_bytes)
                    .map(PushEvent::ChannelMessage)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.conversations.welcome.received" => {
                fauna_cbor::decode_strict::<WelcomePayload>(&payload_bytes)
                    .map(PushEvent::Welcome)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.inbox.item" => fauna_cbor::decode_strict::<InboxItemPayload>(&payload_bytes)
                .map(PushEvent::InboxItem)
                .unwrap_or_else(|_| {
                    PushEvent::Unknown(Unknown {
                        kind: kind.to_string(),
                        payload,
                    })
                }),
            "fauna.protocol.resync_required" => {
                fauna_cbor::decode_strict::<ResyncRequiredPayload>(&payload_bytes)
                    .map(PushEvent::ResyncRequired)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.segments.changed" => {
                fauna_cbor::decode_strict::<SegmentsChangedPayload>(&payload_bytes)
                    .map(PushEvent::SegmentsChanged)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.mail.received" => {
                fauna_cbor::decode_strict::<MailReceivedPayload>(&payload_bytes)
                    .map(PushEvent::MailReceived)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.mail.flags_changed" => {
                fauna_cbor::decode_strict::<MailFlagsChangedPayload>(&payload_bytes)
                    .map(PushEvent::MailFlagsChanged)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.sync.changed" => fauna_cbor::decode_strict::<SyncChangedPayload>(&payload_bytes)
                .map(PushEvent::SyncChanged)
                .unwrap_or_else(|_| {
                    PushEvent::Unknown(Unknown {
                        kind: kind.to_string(),
                        payload,
                    })
                }),
            "fauna.bridges.push.mailbox_state" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeMailboxStatePush,
            >(&payload_bytes)
            .map(PushEvent::BridgeMailboxState)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.config_changed" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeConfigChangedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeConfigChanged)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.atproto.sessions_changed" => fauna_cbor::decode_strict::<
                crate::atproto_pds::BridgeAtprotoSessionsChangedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeAtprotoSessionsChanged)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.atproto.projection_ready" => fauna_cbor::decode_strict::<
                crate::atproto_pds::BridgeAtprotoProjectionReadyPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeAtprotoProjectionReady)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.atproto.issuer_key_rotated" => fauna_cbor::decode_strict::<
                crate::atproto_pds::BridgeAtprotoIssuerKeyRotatedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeAtprotoIssuerKeyRotated)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.atproto.consent_requested" => fauna_cbor::decode_strict::<
                crate::atproto_pds::AtprotoConsentRequestedPush,
            >(&payload_bytes)
            .map(PushEvent::AtprotoConsentRequested)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.atproto.permission_set_requested" => fauna_cbor::decode_strict::<
                crate::atproto_pds::BridgeAtprotoPermissionSetRequestedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeAtprotoPermissionSetRequested)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.outbound_ready" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeOutboundReadyPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeOutboundReady)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.rescore_ready" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeRescoreReadyPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeRescoreReady)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.spam_baseline_publish" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeSpamBaselinePublishPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeSpamBaselinePublish)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.spam_model_updated" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeSpamModelUpdatedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeSpamModelUpdated)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.spam_model_reset" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeSpamModelResetPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeSpamModelReset)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.delegation.lease_changed" => {
                fauna_cbor::decode_strict::<LeaseChangedPayload>(&payload_bytes)
                    .map(PushEvent::LeaseChanged)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            "fauna.bridges.push.import_progress" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeImportProgressPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeImportProgress)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.import_error" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeImportErrorPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeImportError)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.import_complete" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeImportCompletePush,
            >(&payload_bytes)
            .map(PushEvent::BridgeImportComplete)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.export_progress" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeExportProgressPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeExportProgress)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.export_error" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeExportErrorPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeExportError)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.bridges.push.export_complete" => fauna_cbor::decode_strict::<
                crate::bridge_routing::BridgeExportCompletePush,
            >(&payload_bytes)
            .map(PushEvent::BridgeExportComplete)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            crate::bridged_conversations::PUSH_CONVERSATION_CHANGED => fauna_cbor::decode_strict::<
                crate::bridged_conversations::ConversationChangedPush,
            >(&payload_bytes)
            .map(PushEvent::BridgeConversationChanged)
            .unwrap_or_else(|_| {
                PushEvent::Unknown(Unknown {
                    kind: kind.to_string(),
                    payload,
                })
            }),
            "fauna.push.notification" => {
                fauna_cbor::decode_strict::<PushNotificationPayload>(&payload_bytes)
                    .map(PushEvent::PushNotification)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            KIND_SYNC_CHUNK_WANTED => {
                fauna_cbor::decode_strict::<SyncChunkWantedPayload>(&payload_bytes)
                    .map(PushEvent::SyncChunkWanted)
                    .unwrap_or_else(|_| {
                        PushEvent::Unknown(Unknown {
                            kind: kind.to_string(),
                            payload,
                        })
                    })
            }
            _ => PushEvent::Unknown(Unknown {
                kind: kind.to_string(),
                payload,
            }),
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            PushEvent::Knock(_) => "fauna.knock",
            PushEvent::AccountUpdated(_) => "fauna.account.update",
            PushEvent::Notification(_) => "fauna.notification",
            PushEvent::PeerWake(_) => "fauna.peer.wake",
            PushEvent::CalendarChanged(_) => "fauna.calendar.changed",
            PushEvent::AddressBookChanged(_) => "fauna.addressbook.changed",
            PushEvent::ChannelMessage(_) => "fauna.conversations.channel.message",
            PushEvent::Welcome(_) => "fauna.conversations.welcome.received",
            PushEvent::InboxItem(_) => "fauna.inbox.item",
            PushEvent::ResyncRequired(_) => "fauna.protocol.resync_required",
            PushEvent::SegmentsChanged(_) => "fauna.segments.changed",
            PushEvent::MailReceived(_) => "fauna.mail.received",
            PushEvent::MailFlagsChanged(_) => "fauna.mail.flags_changed",
            PushEvent::SyncChanged(_) => "fauna.sync.changed",
            PushEvent::BridgeMailboxState(_) => "fauna.bridges.push.mailbox_state",
            PushEvent::BridgeConfigChanged(_) => "fauna.bridges.config_changed",
            PushEvent::BridgeAtprotoSessionsChanged(_) => "fauna.bridges.atproto.sessions_changed",
            PushEvent::BridgeAtprotoProjectionReady(_) => "fauna.bridges.atproto.projection_ready",
            PushEvent::BridgeAtprotoIssuerKeyRotated(_) => {
                "fauna.bridges.atproto.issuer_key_rotated"
            }
            PushEvent::AtprotoConsentRequested(_) => "fauna.atproto.consent_requested",
            PushEvent::BridgeAtprotoPermissionSetRequested(_) => {
                "fauna.bridges.atproto.permission_set_requested"
            }
            PushEvent::BridgeOutboundReady(_) => "fauna.bridges.outbound_ready",
            PushEvent::BridgeRescoreReady(_) => "fauna.bridges.rescore_ready",
            PushEvent::BridgeSpamBaselinePublish(_) => "fauna.bridges.spam_baseline_publish",
            PushEvent::BridgeSpamModelUpdated(_) => "fauna.bridges.push.spam_model_updated",
            PushEvent::BridgeSpamModelReset(_) => "fauna.bridges.push.spam_model_reset",
            PushEvent::LeaseChanged(_) => "fauna.delegation.lease_changed",
            PushEvent::BridgeImportProgress(_) => "fauna.bridges.push.import_progress",
            PushEvent::BridgeImportError(_) => "fauna.bridges.push.import_error",
            PushEvent::BridgeImportComplete(_) => "fauna.bridges.push.import_complete",
            PushEvent::BridgeExportProgress(_) => "fauna.bridges.push.export_progress",
            PushEvent::BridgeExportError(_) => "fauna.bridges.push.export_error",
            PushEvent::BridgeExportComplete(_) => "fauna.bridges.push.export_complete",
            PushEvent::BridgeConversationChanged(_) => {
                crate::bridged_conversations::PUSH_CONVERSATION_CHANGED
            }
            PushEvent::PushNotification(_) => "fauna.push.notification",
            PushEvent::SyncChunkWanted(_) => KIND_SYNC_CHUNK_WANTED,
            PushEvent::Unknown(u) => &u.kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn knock_round_trip() {
        let p = KnockPayload {
            sender_id: "abcd1234".into(),
            summary: "wants to connect".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: KnockPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    /// `body` round-trips, and a knock without one encodes exactly as it did
    /// before the field existed — no `body` key at all, so an older client's
    /// decode is byte-for-byte unchanged.
    #[test]
    fn knock_body_round_trips_and_a_bodyless_knock_omits_the_key() {
        let with_body = KnockPayload {
            sender_id: "abcd1234".into(),
            summary: "hi".into(),
            body: Some(
                crate::LocalizedText::new("notifications.row_knock")
                    .with_arg("sender", "abcd1234")
                    .with_arg("message", "hi"),
            ),
            ..Default::default()
        };
        let bytes = encode_canonical(&with_body).unwrap();
        let decoded: KnockPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, with_body);

        let bodyless = KnockPayload {
            body: None,
            ..with_body
        };
        let map: Value = fauna_cbor::decode_strict(&encode_canonical(&bodyless).unwrap()).unwrap();
        let Value::Map(entries) = map else {
            panic!("a knock payload is a CBOR map");
        };
        assert!(
            !entries.contains_key("body"),
            "a bodyless knock must not carry a `body` key; got {entries:?}"
        );
    }

    /// The compat direction that matters for a rolling fleet: a client built
    /// before `body` existed decodes a newer nest's knock and keeps the field
    /// in its catch-all rather than failing.
    #[test]
    fn an_older_decoder_keeps_a_knock_body_in_extra() {
        #[derive(Deserialize)]
        struct KnockBeforeBody {
            sender_id: String,
            summary: String,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }
        let bytes = encode_canonical(&KnockPayload {
            sender_id: "abcd1234".into(),
            summary: "hi".into(),
            body: Some(crate::LocalizedText::new("notifications.row_knock")),
            ..Default::default()
        })
        .unwrap();
        let old: KnockBeforeBody = decode(&bytes).unwrap();
        assert_eq!(old.sender_id, "abcd1234");
        assert_eq!(old.summary, "hi");
        assert!(old.extra.contains_key("body"), "got {:?}", old.extra);
    }

    #[test]
    fn account_updated_round_trip() {
        let p = AccountUpdatedPayload {
            changes: vec!["handle".into(), "tier".into()],
            timestamp: 1710000000,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: AccountUpdatedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn notification_round_trip_full() {
        let p = NotificationPayload {
            notification_id: 7,
            notif_type: "comment".into(),
            source: "alice".into(),
            sender_id: Some("abc".into()),
            content_id: Some("def".into()),
            summary: "alice replied".into(),
            body: None,
            timestamp: 1710000000,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: NotificationPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn notification_round_trip_partial() {
        let p = NotificationPayload {
            notification_id: 7,
            notif_type: "system".into(),
            source: "system".into(),
            sender_id: None,
            content_id: None,
            summary: "welcome".into(),
            body: None,
            timestamp: 1710000000,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: NotificationPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
        assert_eq!(decoded.sender_id, None);
    }

    #[test]
    fn atproto_projection_ready_round_trip_with_hint() {
        let p = crate::atproto_pds::BridgeAtprotoProjectionReadyPush {
            actor_id: Some(serde_bytes::ByteBuf::from(vec![0x5Au8; 32])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: crate::atproto_pds::BridgeAtprotoProjectionReadyPush = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn atproto_projection_ready_round_trip_without_hint() {
        let p = crate::atproto_pds::BridgeAtprotoProjectionReadyPush::default();
        assert_eq!(p.actor_id, None);
        let bytes = encode_canonical(&p).unwrap();
        let decoded: crate::atproto_pds::BridgeAtprotoProjectionReadyPush = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    /// The wire kind string classifies into the typed variant and `kind()`
    /// round-trips it back — the pair every push consumer relies on.
    #[test]
    fn atproto_projection_ready_from_push_classifies() {
        let p = crate::atproto_pds::BridgeAtprotoProjectionReadyPush {
            actor_id: Some(serde_bytes::ByteBuf::from(vec![0x5Au8; 32])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let value: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.atproto.projection_ready", value);
        assert_eq!(event.kind(), "fauna.bridges.atproto.projection_ready");
        match event {
            PushEvent::BridgeAtprotoProjectionReady(decoded) => assert_eq!(decoded, p),
            other => panic!("expected BridgeAtprotoProjectionReady, got {other:?}"),
        }
    }

    #[test]
    fn peer_wake_round_trip() {
        let p = PeerWakePayload {
            requester_actor_id: "abc".into(),
            requester_endpoint: "1.2.3.4:51820".into(),
            nonce: "abcdef".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: PeerWakePayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    /// The web push face hands JS `(kind, payload)` by serializing the
    /// `PushEvent` *itself* and leaning on `#[serde(untagged)]` to yield the
    /// variant's payload **bare** — no enum wrapper, and so no per-variant match
    /// to keep in step as kinds are added (`fauna_rpc_wasm::client::forward_pushes`).
    /// If that ever regressed — a tag introduced, a variant wrapped — every web
    /// push consumer would silently start reading the wrong shape, with no
    /// compile error anywhere. Lock the property: serializing the enum must be
    /// byte-identical to serializing the payload alone.
    #[test]
    fn push_event_serializes_untagged_as_its_bare_payload() {
        let p = CalendarChangedPayload {
            actor_id: "abc".into(),
            calendar_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        };

        let bare = encode_canonical(&p).unwrap();
        let via_enum = encode_canonical(&PushEvent::CalendarChanged(p.clone())).unwrap();
        assert_eq!(
            via_enum, bare,
            "PushEvent must serialize untagged (the payload object bare) — the web \
             push face depends on it"
        );

        // ...and under the kind string a native app matches on, since web and
        // native consume the same push set (`transport.md` § Push events).
        assert_eq!(
            PushEvent::CalendarChanged(p).kind(),
            "fauna.calendar.changed"
        );
    }

    /// The retired `fauna.event.rsvp` kind (dead-registered 2026-07-17, off the
    /// wire 2026-09-24 with the compat-remnant sweep) decodes as `Unknown` —
    /// a push of that kind is ignored like every other unmodelled kind.
    #[test]
    fn the_retired_rsvp_kind_decodes_as_unknown() {
        let payload = Value::Map(BTreeMap::from([
            ("event_id".to_string(), Value::String("evt1".into())),
            ("actor_id".to_string(), Value::String("abc".into())),
            ("status".to_string(), Value::String("accepted".into())),
        ]));
        let ev = PushEvent::from_push("fauna.event.rsvp", payload);
        assert!(
            matches!(&ev, PushEvent::Unknown(u) if u.kind == "fauna.event.rsvp"),
            "{ev:?}"
        );
    }

    #[test]
    fn channel_message_round_trip() {
        let p = ChannelMessagePayload {
            channel_id: "chan1".into(),
            data: vec![0x01, 0x02, 0x03],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: ChannelMessagePayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn welcome_round_trip_minimal() {
        let p = WelcomePayload {
            welcome_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            ..Default::default()
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: WelcomePayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn welcome_round_trip_cross_nest() {
        let p = WelcomePayload {
            welcome_bytes: vec![0x01, 0x02, 0x03],
            channel_id: Some("chan1".into()),
            nest_url: Some("https://other.nest/".into()),
            channel_type: Some("folder".into()),
            group_id: Some("grp1".into()),
            shared_by: Some("ab".repeat(32)),
            shared_by_handle: Some("bob".into()),
            shared_by_domain: Some("other.nest".into()),
            set_name: Some("Holiday Photos".into()),
            set_name_sealed: Some(ByteBuf::from(vec![0xEDu8; 40])),
            set_name_hash: Some(ByteBuf::from(vec![0x5Au8; 32])),
            access: Some("writer".into()),
            home_nest_actor_id: Some("ef".repeat(32)),
            residency: Some("metadata_only".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: WelcomePayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn push_event_welcome_typed_decode() {
        let p = WelcomePayload {
            welcome_bytes: vec![0xaa, 0xbb],
            channel_id: Some("c1".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&p).unwrap();
        let payload: Value = fauna_cbor::decode_strict(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.conversations.welcome.received", payload);
        match event {
            PushEvent::Welcome(w) => {
                assert_eq!(w.welcome_bytes, vec![0xaa, 0xbb]);
                assert_eq!(w.channel_id.as_deref(), Some("c1"));
            }
            other => panic!("expected Welcome, got {:?}", other),
        }
    }

    #[test]
    fn inbox_item_round_trip() {
        let p = InboxItemPayload {
            content_id: "abc".into(),
            kind_hint: Some("post".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: InboxItemPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn resync_required_round_trip() {
        let p = ResyncRequiredPayload {
            dropped_count: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: ResyncRequiredPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn push_event_unknown_kind_falls_through() {
        let payload = Value::Map(Default::default());
        let event = PushEvent::from_push("com.acme.experimental", payload.clone());
        match event {
            PushEvent::Unknown(u) => {
                assert_eq!(u.kind, "com.acme.experimental");
                assert_eq!(u.payload, payload);
            }
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn push_event_typed_decode() {
        let p = KnockPayload {
            sender_id: "abc".into(),
            summary: "hi".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&p).unwrap();
        let payload: Value = fauna_cbor::decode_strict(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.knock", payload);
        match event {
            PushEvent::Knock(k) => assert_eq!(k.sender_id, "abc"),
            other => panic!("expected Knock, got {:?}", other),
        }
    }

    #[test]
    fn segments_changed_payload_round_trips() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = SegmentsChangedPayload {
            kind: "mail".to_string(),
            actor_id: "11".repeat(32),
            segment_id: 7,
            change: SegmentChange::Finalized,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: SegmentsChangedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn segments_changed_classifies_via_from_push() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = SegmentsChangedPayload {
            kind: "mail".to_string(),
            actor_id: "22".repeat(32),
            segment_id: 3,
            change: SegmentChange::CompactedIn,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.segments.changed", cbor);
        match event {
            PushEvent::SegmentsChanged(p) => {
                assert_eq!(p.segment_id, 3);
                assert!(matches!(p.change, SegmentChange::CompactedIn));
            }
            other => panic!("expected SegmentsChanged, got {other:?}"),
        }
    }

    #[test]
    fn sync_changed_payload_round_trips() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = SyncChangedPayload {
            folder: "family-photos".to_string(),
            scope: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: SyncChangedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);
    }

    /// A sealed set's nudge carries no plaintext: the receiver matches it by
    /// the hash of the name it knows, and a blank `folder` names nothing.
    #[test]
    fn sync_changed_names_its_set_by_hash_first() {
        let hashed = SyncChangedPayload {
            folder_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("Holiday Photos").to_vec(),
            )),
            ..Default::default()
        };
        assert!(hashed.names_set("Holiday Photos"));
        assert!(!hashed.names_set("Work"));
        assert!(!hashed.names_set(""));
        let named = SyncChangedPayload {
            folder: "docs".into(),
            ..Default::default()
        };
        assert!(named.names_set("docs"));
        assert!(!named.names_set("photos"));
        assert!(!SyncChangedPayload::default().names_set(""));
    }

    #[test]
    fn sync_changed_classifies_via_from_push() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = SyncChangedPayload {
            folder: "docs".to_string(),
            scope: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&payload).unwrap();
        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.sync.changed", cbor);
        match event {
            PushEvent::SyncChanged(p) => assert_eq!(p.folder, "docs"),
            other => panic!("expected SyncChanged, got {other:?}"),
        }
    }

    #[test]
    fn sync_changed_kind_returns_correct_string() {
        let event = PushEvent::SyncChanged(SyncChangedPayload {
            folder: "notes".to_string(),
            scope: None,
            ..Default::default()
        });
        assert_eq!(event.kind(), "fauna.sync.changed");
    }

    #[test]
    fn bridge_config_changed_classifies_via_from_push() {
        use crate::bridge_routing::{BridgeConfigChangedPush, config_change_reason};
        let push = BridgeConfigChangedPush {
            reason: config_change_reason::SPAM_POLICY.to_string(),
        };
        let bytes = encode_canonical(&push).unwrap();
        let payload: Value = fauna_cbor::decode_strict(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.config_changed", payload);
        match event {
            PushEvent::BridgeConfigChanged(p) => {
                assert_eq!(p.reason, "spam_policy");
            }
            other => panic!("expected BridgeConfigChanged, got {other:?}"),
        }
        assert_eq!(
            PushEvent::BridgeConfigChanged(BridgeConfigChangedPush {
                reason: "mail_enabled".into(),
            })
            .kind(),
            "fauna.bridges.config_changed"
        );
    }

    #[test]
    fn bridge_outbound_ready_round_trips_and_classifies() {
        use crate::bridge_routing::BridgeOutboundReadyPush;
        use crate::codec::{decode_strict as decode, encode_canonical};
        // Payloadless push → empty CBOR map; round-trips and classifies.
        let push = BridgeOutboundReadyPush::default();
        let bytes = encode_canonical(&push).unwrap();
        let decoded: BridgeOutboundReadyPush = decode(&bytes).unwrap();
        assert_eq!(decoded, push);

        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.outbound_ready", cbor);
        assert!(
            matches!(event, PushEvent::BridgeOutboundReady(_)),
            "expected BridgeOutboundReady, got {event:?}"
        );
        assert_eq!(
            PushEvent::BridgeOutboundReady(BridgeOutboundReadyPush::default()).kind(),
            "fauna.bridges.outbound_ready"
        );
    }

    #[test]
    fn bridge_rescore_ready_round_trips_and_classifies() {
        use crate::bridge_routing::BridgeRescoreReadyPush;
        use crate::codec::{decode_strict as decode, encode_canonical};
        // Payloadless push → empty CBOR map; round-trips and classifies.
        let push = BridgeRescoreReadyPush::default();
        let bytes = encode_canonical(&push).unwrap();
        let decoded: BridgeRescoreReadyPush = decode(&bytes).unwrap();
        assert_eq!(decoded, push);

        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.rescore_ready", cbor);
        assert!(
            matches!(event, PushEvent::BridgeRescoreReady(_)),
            "expected BridgeRescoreReady, got {event:?}"
        );
        assert_eq!(
            PushEvent::BridgeRescoreReady(BridgeRescoreReadyPush::default()).kind(),
            "fauna.bridges.rescore_ready"
        );
    }

    #[test]
    fn bridge_spam_baseline_publish_round_trips_and_classifies() {
        use crate::bridge_routing::BridgeSpamBaselinePublishPush;
        use crate::codec::{decode_strict as decode, encode_canonical};
        use serde_bytes::ByteBuf;
        // Carries the pending-run id; round-trips and classifies.
        let push = BridgeSpamBaselinePublishPush {
            run_id: ByteBuf::from(vec![0xAB; 16]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&push).unwrap();
        let decoded: BridgeSpamBaselinePublishPush = decode(&bytes).unwrap();
        assert_eq!(decoded, push);

        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.spam_baseline_publish", cbor);
        match &event {
            PushEvent::BridgeSpamBaselinePublish(p) => {
                assert_eq!(p.run_id.as_ref(), &[0xAB; 16]);
            }
            other => panic!("expected BridgeSpamBaselinePublish, got {other:?}"),
        }
        assert_eq!(event.kind(), "fauna.bridges.spam_baseline_publish");
    }

    #[test]
    fn bridge_spam_model_pushes_round_trip_and_classify() {
        use crate::bridge_routing::{BridgeSpamModelResetPush, BridgeSpamModelUpdatedPush};
        use crate::codec::{decode_strict as decode, encode_canonical};

        let updated = BridgeSpamModelUpdatedPush {
            actor_id: vec![5u8; 32],
        };
        let bytes = encode_canonical(&updated).unwrap();
        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.push.spam_model_updated", cbor);
        assert_eq!(event.kind(), "fauna.bridges.push.spam_model_updated");
        match &event {
            PushEvent::BridgeSpamModelUpdated(p) => assert_eq!(p.actor_id, vec![5u8; 32]),
            other => panic!("expected BridgeSpamModelUpdated, got {other:?}"),
        }

        let reset = BridgeSpamModelResetPush {
            actor_id: vec![6u8; 32],
        };
        let bytes = encode_canonical(&reset).unwrap();
        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.bridges.push.spam_model_reset", cbor);
        assert_eq!(event.kind(), "fauna.bridges.push.spam_model_reset");
        match &event {
            PushEvent::BridgeSpamModelReset(p) => assert_eq!(p.actor_id, vec![6u8; 32]),
            other => panic!("expected BridgeSpamModelReset, got {other:?}"),
        }
    }

    #[test]
    fn segments_changed_kind_returns_correct_string() {
        let event = PushEvent::SegmentsChanged(SegmentsChangedPayload {
            kind: "mail".to_string(),
            actor_id: "00".repeat(32),
            segment_id: 1,
            change: SegmentChange::Tombstoned,
            extra: BTreeMap::new(),
        });
        assert_eq!(event.kind(), "fauna.segments.changed");
    }

    #[test]
    fn lease_changed_payload_round_trips_and_classifies() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = LeaseChangedPayload {
            task_kind: "backup-upload".to_string(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: LeaseChangedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);

        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.delegation.lease_changed", cbor);
        match event {
            PushEvent::LeaseChanged(p) => assert_eq!(p.task_kind, "backup-upload"),
            other => panic!("expected LeaseChanged, got {other:?}"),
        }
        assert_eq!(
            PushEvent::LeaseChanged(LeaseChangedPayload {
                task_kind: "index".into(),
                extra: BTreeMap::new(),
            })
            .kind(),
            "fauna.delegation.lease_changed"
        );
    }

    #[test]
    fn push_notification_payload_round_trips_and_classifies() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = PushNotificationPayload {
            title: "Fauna".into(),
            body: "New message".into(),
            url: "/app/inbox".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: PushNotificationPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);

        let cbor: Value = decode(&bytes).unwrap();
        match PushEvent::from_push("fauna.push.notification", cbor) {
            PushEvent::PushNotification(p) => assert_eq!(p.url, "/app/inbox"),
            other => panic!("expected PushNotification, got {other:?}"),
        }
        let event = PushEvent::PushNotification(payload);
        assert_eq!(event.kind(), "fauna.push.notification");
        assert_eq!(event.invalidates(), StaleSurfaces::NONE);
        assert_eq!(StaleSurfaces::for_kind(event.kind()), StaleSurfaces::NONE);
    }

    #[test]
    fn sync_chunk_wanted_payload_round_trips_and_classifies() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = SyncChunkWantedPayload {
            request_id: 42,
            folder: "local:7".into(),
            store_key: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: SyncChunkWantedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);

        let cbor: Value = decode(&bytes).unwrap();
        match PushEvent::from_push(KIND_SYNC_CHUNK_WANTED, cbor) {
            PushEvent::SyncChunkWanted(p) => assert_eq!(p.request_id, 42),
            other => panic!("expected SyncChunkWanted, got {other:?}"),
        }
        let event = PushEvent::SyncChunkWanted(payload);
        assert_eq!(event.kind(), "fauna.sync.chunk.wanted");
        assert_eq!(event.invalidates(), StaleSurfaces::NONE);
        assert_eq!(StaleSurfaces::for_kind(event.kind()), StaleSurfaces::NONE);
    }

    #[test]
    fn mail_flags_changed_payload_round_trips_and_classifies() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = MailFlagsChangedPayload {
            actor_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: MailFlagsChangedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);

        let cbor: Value = decode(&bytes).unwrap();
        match PushEvent::from_push("fauna.mail.flags_changed", cbor) {
            PushEvent::MailFlagsChanged(p) => assert_eq!(p.actor_id, "ab".repeat(32)),
            other => panic!("expected MailFlagsChanged, got {other:?}"),
        }
        assert_eq!(mail_flags_changed().kind(), "fauna.mail.flags_changed");
    }

    #[test]
    fn mail_received_payload_round_trips_and_classifies() {
        use crate::codec::{decode_strict as decode, encode_canonical};
        let payload = MailReceivedPayload {
            actor_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&payload).unwrap();
        let decoded: MailReceivedPayload = decode(&bytes).unwrap();
        assert_eq!(decoded, payload);

        // Round-trips through the kind-routed classifier too.
        let cbor: Value = decode(&bytes).unwrap();
        let event = PushEvent::from_push("fauna.mail.received", cbor);
        match event {
            PushEvent::MailReceived(p) => assert_eq!(p.actor_id, "ab".repeat(32)),
            other => panic!("expected MailReceived, got {other:?}"),
        }
        assert_eq!(
            PushEvent::MailReceived(MailReceivedPayload {
                actor_id: "00".repeat(32),
                extra: BTreeMap::new(),
            })
            .kind(),
            "fauna.mail.received"
        );
    }

    // ── StaleSurfaces: the one shared answer to "what did this push invalidate?" ──

    fn knock() -> PushEvent {
        PushEvent::Knock(KnockPayload {
            sender_id: "ab".repeat(32),
            summary: "hi".into(),
            ..Default::default()
        })
    }

    fn notification() -> PushEvent {
        PushEvent::Notification(NotificationPayload {
            notification_id: 1,
            notif_type: "mention".into(),
            source: "local".into(),
            sender_id: None,
            content_id: None,
            summary: "s".into(),
            body: None,
            timestamp: 0,
            extra: BTreeMap::new(),
        })
    }

    fn account_updated() -> PushEvent {
        PushEvent::AccountUpdated(AccountUpdatedPayload {
            changes: vec!["quota".into()],
            timestamp: 0,
            extra: BTreeMap::new(),
        })
    }

    fn calendar_changed() -> PushEvent {
        PushEvent::CalendarChanged(CalendarChangedPayload {
            actor_id: "ab".repeat(32),
            calendar_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        })
    }

    fn address_book_changed() -> PushEvent {
        PushEvent::AddressBookChanged(AddressBookChangedPayload {
            actor_id: "ab".repeat(32),
            addressbook_id: "ef".repeat(32),
            extra: BTreeMap::new(),
        })
    }

    fn sync_changed() -> PushEvent {
        PushEvent::SyncChanged(SyncChangedPayload {
            folder: "Holiday Photos".into(),
            scope: None,
            ..Default::default()
        })
    }

    fn resync_required() -> PushEvent {
        PushEvent::ResyncRequired(ResyncRequiredPayload {
            dropped_count: 7,
            extra: BTreeMap::new(),
        })
    }

    fn consent_requested() -> PushEvent {
        PushEvent::AtprotoConsentRequested(crate::atproto_pds::AtprotoConsentRequestedPush {
            consent: Default::default(),
            extra: BTreeMap::new(),
        })
    }

    fn mail_flags_changed() -> PushEvent {
        PushEvent::MailFlagsChanged(MailFlagsChangedPayload {
            actor_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        })
    }

    fn mail_received() -> PushEvent {
        PushEvent::MailReceived(MailReceivedPayload {
            actor_id: "00".repeat(32),
            extra: BTreeMap::new(),
        })
    }

    /// Every kind that carries a user-visible surface, and what it stales.
    #[test]
    fn each_push_kind_stales_its_own_surface() {
        assert_eq!(
            knock().invalidates(),
            StaleSurfaces {
                knocks: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            notification().invalidates(),
            StaleSurfaces {
                notifications: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            account_updated().invalidates(),
            StaleSurfaces {
                account: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            calendar_changed().invalidates(),
            StaleSurfaces {
                events: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            address_book_changed().invalidates(),
            StaleSurfaces {
                address_book: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            sync_changed().invalidates(),
            StaleSurfaces {
                media: true,
                ..StaleSurfaces::NONE
            }
        );
        assert_eq!(
            consent_requested().invalidates(),
            StaleSurfaces {
                atproto: true,
                ..StaleSurfaces::NONE
            }
        );
    }

    /// A kind whose consumer lives somewhere other than the snapshot surfaces
    /// stales nothing — the seam answers staleness, never side effects.
    #[test]
    fn a_kind_owned_by_another_consumer_stales_nothing() {
        // The shared conversations receive loop holds its own subscription.
        assert_eq!(mail_received().invalidates(), StaleSurfaces::NONE);
        // A wire kind this build has never heard of.
        assert_eq!(
            PushEvent::Unknown(Unknown {
                kind: "com.acme.experimental".into(),
                payload: Value::Map(Default::default()),
            })
            .invalidates(),
            StaleSurfaces::NONE
        );
    }

    /// The nest drops pushes on overflow and says so; the sweep that follows
    /// must cover every surface a *dropped* push would have staled. That is the
    /// whole purpose of the kind, and it is what linux was getting wrong for
    /// `media` and `bluesky` before this seam existed.
    #[test]
    fn resync_required_covers_every_push_fed_surface() {
        let sweep = resync_required().invalidates();
        for event in [
            knock(),
            notification(),
            account_updated(),
            calendar_changed(),
            address_book_changed(),
            sync_changed(),
            consent_requested(),
        ] {
            assert!(
                sweep.covers(&event.invalidates()),
                "ResyncRequired sweep misses a surface staled by {:?}",
                event.kind()
            );
        }
    }

    /// The feed and the family read are the surfaces no push feeds, so only a
    /// full reconnect recovers them — `ResyncRequired` deliberately leaves both
    /// alone and stales everything else.
    #[test]
    fn only_a_reconnect_stales_the_feed() {
        assert!(StaleSurfaces::on_reconnect().feed);
        assert!(!resync_required().invalidates().feed);
        assert_eq!(
            StaleSurfaces {
                feed: false,
                family: false,
                ..StaleSurfaces::on_reconnect()
            },
            resync_required().invalidates(),
        );
    }

    /// The anti-drift property: a reconnect may have missed any push at all, so
    /// its sweep is a superset of every kind's own set. A new variant that
    /// stales a surface the reconnect sweep does not cover fails here.
    #[test]
    fn a_reconnect_covers_every_kind() {
        let reconnect = StaleSurfaces::on_reconnect();
        for event in [
            knock(),
            notification(),
            account_updated(),
            calendar_changed(),
            address_book_changed(),
            sync_changed(),
            consent_requested(),
            resync_required(),
            mail_received(),
            mail_flags_changed(),
        ] {
            assert!(
                reconnect.covers(&event.invalidates()),
                "reconnect sweep misses a surface staled by {:?}",
                event.kind()
            );
        }
    }

    /// The family read (`fauna.family.status`) is a reconnect-only surface:
    /// `family-client-enforcement.md` § Content policy names "cold launch and
    /// WS reconnect" as the two moments it fires, and no push feeds it — so the
    /// sweep must stale it and no kind may. Before this flag existed the two
    /// apps deriving their sweep from this seam (linux, tui) never re-read the
    /// ward's policy after a reconnect, so a guardian's edit bound only at the
    /// ward's next login. `ResyncRequired` is a push, so it is on the "no kind" side
    /// for the same reason the feed is: a dropped push cannot have staled what
    /// no push touches.
    #[test]
    fn a_reconnect_stales_the_family_surface_and_no_push_does() {
        assert!(StaleSurfaces::on_reconnect().family);
        const { assert!(!StaleSurfaces::NONE.family) };
        for event in [
            knock(),
            notification(),
            account_updated(),
            calendar_changed(),
            address_book_changed(),
            sync_changed(),
            consent_requested(),
            resync_required(),
            mail_received(),
            mail_flags_changed(),
        ] {
            assert!(
                !event.invalidates().family,
                "{:?} stales the family read, but no push feeds it",
                event.kind()
            );
            assert!(
                !StaleSurfaces::for_kind(event.kind()).family,
                "for_kind({:?}) stales the family read, but no push feeds it",
                event.kind()
            );
        }
    }

    #[test]
    fn covers_is_a_real_subset_test() {
        let media = StaleSurfaces {
            media: true,
            ..StaleSurfaces::NONE
        };
        let account = StaleSurfaces {
            account: true,
            ..StaleSurfaces::NONE
        };
        assert!(StaleSurfaces::on_reconnect().covers(&media));
        assert!(media.covers(&media));
        assert!(media.covers(&StaleSurfaces::NONE));
        assert!(!media.covers(&account));
        assert!(!StaleSurfaces::NONE.covers(&media));
    }

    /// The anti-drift property `for_kind` itself cannot enforce (its `&str`
    /// match has no compile-time link to the enum): every kind that stales a
    /// surface through `invalidates()` must answer identically through
    /// `for_kind(kind())`, so the wasm/UniFFI boundaries — which only ever see
    /// the kind string — agree with the native/tui boundary that sees the
    /// typed enum. Covers every kind `for_kind` has a dedicated arm for, plus
    /// one representative NONE kind.
    #[test]
    fn for_kind_matches_invalidates() {
        for event in [
            knock(),
            notification(),
            account_updated(),
            calendar_changed(),
            address_book_changed(),
            sync_changed(),
            consent_requested(),
            resync_required(),
            mail_received(),
            mail_flags_changed(),
        ] {
            assert_eq!(
                StaleSurfaces::for_kind(event.kind()),
                event.invalidates(),
                "for_kind disagrees with invalidates() for {:?}",
                event.kind()
            );
        }
    }

    /// A kind neither `for_kind` nor `invalidates()` has ever heard of stales
    /// nothing on either boundary — the forward-compat default, not a panic.
    #[test]
    fn for_kind_defaults_to_none_for_an_unknown_kind() {
        assert_eq!(
            StaleSurfaces::for_kind("com.acme.experimental"),
            StaleSurfaces::NONE
        );
    }

    #[test]
    fn union_accumulates_surfaces() {
        let merged = knock()
            .invalidates()
            .union(calendar_changed().invalidates());
        assert!(merged.knocks && merged.events);
        assert!(!merged.media);
        assert!(merged.any());
        assert!(!StaleSurfaces::NONE.any());
    }
}
