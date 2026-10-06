//! `fauna.backup.nest_key.{grant,revoke}` + `fauna.backup.status` — the
//! nest-side segment-backup grant plane (nest-side segment backup, slice 2;
//! design tracked internally;
//! `docs/goal/architecture/key-material-hierarchy.md` § Path A-sibling-0).
//!
//! The user's own client derives `NestBackupKey` from its identity seed and
//! **grants the 32-byte key to its source nest** (at destination-enroll) so the
//! nest's in-process backup coordinator can seal that owner's segment files
//! before upload to blind destinations. `revoke` deletes it (the
//! freeze-the-backup affordance). `status` reads the enrolled flag + the
//! per-destination backup projection — the uniform 7-client Backups-page read
//! (`docs/goal/behavior/backup-destinations.md` § State & data shape) that replaces the old
//! source-side FFI status computation.
//!
//! All three are owner-minted USER-class kinds: the authenticated caller acts
//! only on its own key, gated nest-side in
//! `bridge_method_allowlist::is_permitted`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use fauna_cbor::Value;

/// `fauna.backup.nest_key.grant` request — grant the authenticated actor's own
/// `NestBackupKey` (32 bytes, client-derived from the identity seed) to the
/// source nest. Idempotent: re-granting (e.g. after an identity succession
/// re-derives the key under the successor seed) replaces the stored key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NestKeyGrantRequest {
    /// The owner's 32-byte `NestBackupKey`. The nest stores it plaintext and
    /// uses it to seal this owner's segments — sound because the key seals only
    /// data the nest already hosts (design record § Trust-domain analysis).
    pub nest_backup_key: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NestKeyGrantReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.nest_key.revoke` request — revoke the authenticated actor's
/// grant (delete the stored key). No fields beyond the forward-compat tail; the
/// owner is always the authenticated caller.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NestKeyRevokeRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NestKeyRevokeReply {
    /// `true` if a grant existed and was removed; `false` if none was stored
    /// (idempotent revoke).
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.status` request — read the authenticated actor's backup
/// status. No fields beyond the tail.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.status` reply — the uniform 7-client Backups-page projection
/// (`docs/goal/behavior/backup-destinations.md` § State & data shape) that replaces the old
/// source-side FFI status read. `enrolled` reflects a stored `NestBackupKey`
/// grant; `destinations` carries one row per configured destination, populated
/// once the in-process coordinator runs (slice 3 wires destinations + the
/// nest→destination auth arm), empty until then.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupStatusReply {
    pub enrolled: bool,
    pub destinations: Vec<BackupDestinationStatusItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One per-destination status row — the wire mirror of
/// `fauna_sync_engine::segment_backup::BackupDestinationStatus`.
///
/// **The row shape is uniform across destination kinds; the semantics invert**
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind → *Status projection
/// inverts, same rows*). For a peer nest the nest reports on its own pushing;
/// for a client-device custodian it reports what the device last told it
/// (`fauna.backup.custodian.checkin`) — `last_upload_time` becomes the last
/// caught-up check-in, and `backlog_count` becomes how far the nest's head runs
/// ahead of the device's acked high-water. Clients render the same row and pick
/// labels from the kind they already hold in their own config, so no row IDs
/// change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupDestinationStatusItem {
    /// The configured destination this row reports on
    /// (`BackupDestination::destination_id`).
    pub destination_id: String,
    /// Unix seconds of the last successful manifest-mirror upload to this
    /// destination, or `None` until the first mirror. On a client-device row:
    /// the last check-in at which the device reported itself caught up.
    pub last_upload_time: Option<u64>,
    /// Source segments still queued for upload ("N queued"). On a client-device
    /// row: the nest's head minus the device's acked high-water.
    pub backlog_count: u32,
    /// Client-device rows only: bytes the custodian reported holding, for the
    /// `backup-destination-usage` render. `None` on a nest row, and on a
    /// custodian that has never checked in.
    #[serde(default)]
    pub held_bytes: Option<u64>,
    /// Client-device rows only: [`CAP_STATE_OK`] or [`CAP_STATE_REACHED`].
    /// A custodian at its cap must read as *cap-reached*, never as an ordinary
    /// lag — it has stopped pulling rather than fallen behind, and the two are
    /// indistinguishable from `backlog_count` alone.
    #[serde(default)]
    pub cap_state: Option<String>,
    /// Client-device rows only: [`AUDIT_STATE_OK`] or [`AUDIT_STATE_FAILED`] —
    /// the custodian's verdict on its **own** local store, as of
    /// [`Self::last_audit_passed_at`].
    ///
    /// `None` on a nest row, and on a custodian that has not audited yet. That
    /// absence is deliberately *not* a failure: a device enrolled minutes ago
    /// has audited nothing, and rendering it as failing would alarm on every
    /// enrollment — the same reason `evaluate_overdue` refuses to call a
    /// never-audited destination overdue.
    #[serde(default)]
    pub audit_state: Option<String>,
    /// Client-device rows only: unix seconds of the last **passing** self-audit.
    ///
    /// Read with [`Self::audit_state`], never instead of it. A failing custodian
    /// keeps whatever its last pass was, so this field alone cannot distinguish
    /// *"verified an hour ago and healthy"* from *"verified an hour ago and
    /// rotten since"* — which is precisely the state the audit exists to
    /// surface.
    #[serde(default)]
    pub last_audit_passed_at: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The cap-state and audit-state wire values, defined one layer down in
/// `fauna-core` beside the destination-kind discriminators and re-exported here
/// so this crate stays the place a wire reader looks for them.
///
/// They live down there because the shared render layer
/// (`fauna_core::format::backup_usage_label` and its audit sibling) has to
/// compare against the canonical value and `fauna-core` cannot name this crate —
/// see the constants' own docs for why a duplicated string would be worse than
/// the indirection.
pub use fauna_core::data::{AUDIT_STATE_FAILED, AUDIT_STATE_OK, CAP_STATE_OK, CAP_STATE_REACHED};

/// **T** — the destination-side custody grace window: how long a *superseded* or
/// *tombstoned* backup generation is retained before its chunks may be reclaimed
/// (`docs/goal/architecture/segment-backup-protocol.md` § Custody grace window
/// (T), ratified 2026-07-23).
///
/// A custody writer's supersede power is delete power, and the writer is the
/// owner's **source nest** rather than their client — so a compromised source
/// could otherwise erase a user's whole backup with one sweep of junk records.
/// T is the window in which the owner's client can notice (the audit loop:
/// `AUDIT_*` in `backup-restore.md` § Background Tasks, all ≪ T by construction),
/// revoke the writer grant directly at the destination, and restore the good
/// generations — all without trusting the source.
///
/// A hard-coded constant, deliberately: no user or admin ever has a reason to
/// choose this value, so it is not a configuration surface (`docs/goal/principles.md`
/// — every value is either a constant or a client-UI choice, never a knob).
///
/// **Two readers, one value.** The nest's reclaim sweep retains by it and
/// reports it on the wire as [`GenerationListReply::grace_secs`], which is
/// what a client *renders* deadlines from — a destination on a nest with a
/// different window shows its real deadline. The client audit's
/// vanished-ledger rule (`fauna_client_backup::audit`) is the one reader that
/// may not take the wire value alone: the audited party's own word cannot
/// shorten the window its own retention is judged by, so there the reported
/// value is **floored** at this protocol minimum.
pub const BACKUP_CUSTODY_GRACE_SECS: i64 = 30 * 24 * 60 * 60;

// ── fauna.backup.custodian.checkin ────────────────────────────────────────────
//
// How a client-device custodian tells its source nest where it has got to. A
// small USER-class write on the device's own connection after each pull pass —
// the device reporting on itself, so none of the adversarial-writer machinery
// (the grace window T, destination-derived charging) applies here: there is no
// foreign writer. Wire owned by `message-segment-store.md` § Client-device
// custodian (pull).

/// `fauna.backup.custodian.checkin` request — a custodian device acking its
/// progress so the source nest can project this destination's status row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodianCheckinRequest {
    /// Which registered destination this device is checking in for (matches
    /// `BackupDestination::destination_id`). The nest cross-checks that the
    /// row is a client-device kind registered to this device.
    pub destination_id: String,
    /// How far the custodian has pulled and sealed. The nest computes the
    /// row's `backlog_count` as its own head minus this.
    pub high_water: u64,
    /// Bytes the custodian currently holds, for the usage render.
    pub held_bytes: u64,
    /// [`CAP_STATE_OK`] or [`CAP_STATE_REACHED`].
    pub cap_state: String,
    /// The custodian's verdict on its **own** local store —
    /// [`AUDIT_STATE_OK`] or [`AUDIT_STATE_FAILED`]. `None` when this pass ran
    /// no self-audit (the audit is debounced to its own interval, so most
    /// passes carry no fresh verdict) *and* none has ever run.
    ///
    /// This field is what makes `behavior/backup-destinations.md` § Custodian contract question 4
    /// true as built: without it the check-in has no way to say the local store
    /// rotted, and a rotting custodian keeps reporting healthy `cap_state` +
    /// advancing `high_water` indefinitely.
    ///
    /// ⚠ **A failed audit still checks in.** Refusing to report would make the
    /// row go *silent*, and silence is read by the 30-day intermittency rule as
    /// a sleeping device — so the one failure mode the audit exists to catch
    /// would surface as the wrong alarm, thirty days late.
    #[serde(default)]
    pub audit_state: Option<String>,
    /// Unix seconds of the last **passing** self-audit, or `None` if none ever
    /// passed.
    ///
    /// Advances only on a pass — never on a failure, and never merely because a
    /// pass was attempted. The inference this refuses is the exact twin of the
    /// one `cap_state` refuses: a store that failed its audit an instant ago
    /// would otherwise carry a timestamp saying it was just verified.
    #[serde(default)]
    pub last_audit_passed_at: Option<u64>,
    /// **Which device is speaking** — its stable sync `device_id`, the same one
    /// the registry row names in `custodian_device_id`.
    ///
    /// This exists because the refusal contract on [`CustodianCheckinReply::ok`]
    /// is otherwise unimplementable: the WS-RPC router hands a handler only the
    /// authenticated **actor**, and every one of the owner's devices authenticates
    /// as that same actor, so without this field the nest cannot tell which of
    /// them is checking in. The sender already knows the value — it is what it
    /// matched its own assignment on
    /// (`fauna_core::data::custodian_assignment_for`).
    ///
    /// Self-asserted, and that is the right strength for what it defends: every
    /// candidate device here is the owner's own and already fully authenticated,
    /// so the failure this catches is a *stale or buggy assignment* — device B
    /// checking in against the row registered to device A, silently overwriting
    /// A's progress — not an intruder.
    ///
    /// ⚠ **The carrier stays `Option` for wire shape, but the nest REFUSES
    /// absence** (`docs/goal/behavior/backup-destinations.md` § Custodian contract → *Naming
    /// yourself*). Accepting absence is the general additive-everywhere default
    /// (`docs/goal/architecture/version-compatibility.md`), and it made the guard
    /// opt-in for the caller: an unnamed check-in walked past it and overwrote
    /// the enrolled device's row. The exemption is structural, not a judgement
    /// call — a client-device destination row cannot exist without a non-blank
    /// `custodian_device_id`, and no nest has ever served this kind with a
    /// request type lacking this field (handler and field landed together),
    /// so refusing absence cannot break a client that ever had a
    /// working check-in. Senders must always populate it; the type keeps the
    /// `Option` only because the field is younger than the struct.
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodianCheckinReply {
    /// `true` once the check-in is recorded. A check-in naming an unregistered
    /// destination, or one registered to a different device, is refused rather
    /// than silently accepted — it would otherwise let any of the owner's
    /// devices report progress on another's behalf.
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.backup.writer_grant.{register,revoke,list} ──────────────────────────
//
// The **destination**-side half of the plane (slice 3). Where the `nest_key`
// kinds above are spoken to the owner's *source* nest, these three are spoken by
// the owner's client to each *destination*, over its own authed connection —
// never over the federation channel. That separation is load-bearing: it is what
// keeps revocation operable with the source nest fully hostile
// (`federation.md` § Nest-writer backup plane).

/// `fauna.backup.writer_grant.register` request — seat a source nest as the one
/// writer of the authenticated owner's segment-backup custody on this nest
/// (`segment-backup-protocol.md` § Cross-location backup protocol → *The writer
/// seat*). Idempotent: re-registering the seat's holder refreshes the grant
/// (and re-grants a revoked one), so a client retries enroll freely. Naming
/// another writer takes the seat only while the owner's custody here holds no
/// live path; otherwise it is refused `fauna.backup.writer_seat_held`
/// ([`WRITER_SEAT_HELD_HOLDER`] in the error's details names the holder).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantRegisterRequest {
    /// Hex-encoded 32-byte id of the **source** nest being authorized (the
    /// `nest_id` its `fauna.nest.info` reports, and the `origin_nest_id` the
    /// federation handshake will verify).
    pub writer_nest_id: String,
    /// Hex-encoded 32-byte id of the nest whose seat `writer_nest_id` takes
    /// over — the handover a rotated source box's owner carries. Naming the
    /// seat's holder moves the seat, with the grant in force, and renames the
    /// holder's covered-folder mirror sets under the writer; naming anyone
    /// else is refused `fauna.backup.writer_seat_held` and changes nothing.
    /// Absent = a plain registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeds: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The key, in a `fauna.backup.writer_seat_held` error's `details` map, of the
/// hex-encoded nest id holding the seat. Absent when the refused request named
/// a predecessor and the owner has no seat here at all.
pub const WRITER_SEAT_HELD_HOLDER: &str = "holder_nest_id";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantRegisterReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.writer_grant.revoke` request — the freeze-the-backup
/// affordance. Refuses the source nest's next `write_token.mint`; an
/// already-minted token stays valid for its one remaining TTL. The seat stays
/// held by the revoked writer: a revoke ends its authority and admits no other
/// box.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantRevokeRequest {
    /// Hex-encoded 32-byte id of the source nest to de-authorize.
    pub writer_nest_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantRevokeReply {
    /// `true` if the named writer's grant was in force and is now revoked;
    /// `false` if it held no grant in force (idempotent revoke).
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.writer_grant.list` request — the authenticated owner's own
/// writer seat on this nest (at most one item). No fields beyond the tail; the
/// reply is owner-scoped by construction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantListReply {
    pub grants: Vec<WriterGrantItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The owner's writer seat, for the client's trust-facet row
/// (`docs/goal/ui/nests.md` — per-grant, revocable, audited from the client).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriterGrantItem {
    /// Hex-encoded 32-byte id of the source nest holding the seat.
    pub writer_nest_id: String,
    /// Unix seconds the grant was (most recently) registered.
    pub granted_at: i64,
    /// `true` when the holder's grant is revoked: it holds the seat and may
    /// not write. Absent = in force.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.backup.destination.{register,remove,list} ───────────────────────────
//
// The **source**-side destination registry (slice 3). Spoken by the owner's
// client to its own *source* nest, over its own authed connection, to tell the
// nest **where** to back its segments up — the piece that lets the in-process
// coordinator run, because the destination list in the `fauna.state.backup` rows
// is client-sealed account state the nest cannot read (`backup-restore.md`
// § Background Tasks; `message-segment-store.md` § Cross-location backup protocol).
// Owner is always the authenticated caller; a client acts only on its own
// destinations.

/// `fauna.backup.destination.register` request — register (or refresh) a backup
/// destination for the authenticated owner on its source nest. Idempotent on
/// `destination_id`: re-registering the same id (a URL/nest-id edit, an enroll
/// retry) replaces in place rather than duplicating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DestinationRegisterRequest {
    /// Client-assigned stable id (matches `BackupDestination::destination_id`).
    pub destination_id: String,
    /// Origin URL of the destination nest — the coordinator dials it over the
    /// federation channel. Empty on a non-nest kind, which is never dialled.
    pub destination_nest_url: String,
    /// Hex-encoded 32-byte id of the destination nest (its `fauna.nest.info`
    /// pubkey), pinned as the federation handshake's expected peer. Empty on a
    /// non-nest kind.
    pub destination_nest_id: String,
    /// Which kind of custodian this is — the wire half of
    /// `fauna_core::data::BackupDestination::kind`
    /// (`docs/goal/behavior/backup-destinations.md` § State & data shape → Kind discriminator).
    /// Absent means `"nest"`, the peer-nest kind.
    ///
    /// The nest must branch on this before dialling: a `"client-device"` row
    /// has no address to federate to and is driven by the device pulling.
    #[serde(default = "fauna_core::data::default_destination_kind")]
    pub kind: String,
    /// `"client-device"` rows only: the custodian device's stable sync
    /// `device_id`. The nest keys the custodian's status projection on it
    /// (`fauna.backup.custodian.checkin`).
    #[serde(default)]
    pub custodian_device_id: Option<String>,
    /// `"client-device"` rows only: the user-set capacity cap in bytes — the
    /// kind's only knob (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
    ///
    /// **On the wire because the custodian's own host must be able to read it.**
    /// The authoritative copy is the at-rest `BackupDestination` row, which only
    /// a seed-holding client can open; but on desktop the pull is hosted by the
    /// sync agent, which is bearer-only by construction
    /// (`../architecture/apps/sync-agent.md` § Credential model) and therefore
    /// discovers its own assignment by reading this registry back — the
    /// *policy through the nest, never over local IPC* rule that same doc's
    /// § Control plane split states for every destination row. Absent = uncapped.
    #[serde(default)]
    pub capacity_cap_bytes: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for DestinationRegisterRequest {
    /// Hand-written so `kind` defaults to `"nest"` rather than `""`, matching
    /// `BackupDestination::default()`. A derived `Default` would put an empty
    /// kind on the wire from any struct-update fixture — the same trap the
    /// at-rest row documents.
    fn default() -> Self {
        Self {
            destination_id: String::new(),
            destination_nest_url: String::new(),
            destination_nest_id: String::new(),
            kind: fauna_core::data::default_destination_kind(),
            custodian_device_id: None,
            capacity_cap_bytes: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationRegisterReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.destination.remove` request — deregister a destination (the
/// coordinator then removal-reconciles it away and drops its remote custody).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationRemoveRequest {
    pub destination_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationRemoveReply {
    /// `true` if a row existed and was removed; `false` if none was registered
    /// (idempotent remove).
    pub removed: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.destination.list` request — the authenticated owner's own
/// registered destinations. No fields beyond the tail; the reply is owner-scoped
/// by construction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DestinationListReply {
    pub destinations: Vec<DestinationItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One registered backup destination row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DestinationItem {
    pub destination_id: String,
    pub destination_nest_url: String,
    /// Hex-encoded 32-byte destination nest id.
    pub destination_nest_id: String,
    /// Unix seconds the destination was (most recently) registered.
    pub added_at: i64,
    /// The registered kind — the read-back of
    /// [`DestinationRegisterRequest::kind`]. Absent means `"nest"`.
    #[serde(default = "fauna_core::data::default_destination_kind")]
    pub kind: String,
    /// `"client-device"` rows only: the custodian device's stable sync id.
    #[serde(default)]
    pub custodian_device_id: Option<String>,
    /// `"client-device"` rows only: the read-back of
    /// [`DestinationRegisterRequest::capacity_cap_bytes`] — see there for why a
    /// user-set knob rides the registry at all. Absent = uncapped.
    #[serde(default)]
    pub capacity_cap_bytes: Option<u64>,
    /// The ordinary folders attached to this destination
    /// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
    /// Absent from a reply that lists none — an empty vec, which is also what
    /// it means: the reserved account rails are implicit and never listed here.
    #[serde(default)]
    pub covered_folders: Vec<CoveredFolder>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for DestinationItem {
    /// Hand-written for the same reason as [`DestinationRegisterRequest`]:
    /// an absent kind means `"nest"`, so a default-constructed row must say so.
    fn default() -> Self {
        Self {
            destination_id: String::new(),
            destination_nest_url: String::new(),
            destination_nest_id: String::new(),
            added_at: 0,
            kind: fauna_core::data::default_destination_kind(),
            custodian_device_id: None,
            capacity_cap_bytes: None,
            covered_folders: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl DestinationItem {
    /// This registry row as the shared custodian-matching input
    /// (`fauna_core::data::custodian_assignment_for`).
    ///
    /// The registry row and the at-rest `BackupDestination` are two carriers of
    /// one concept read by two different hosts — the seed-holding app reads the
    /// at-rest row, the bearer-only sync agent reads this one — so the rule that
    /// decides *which row is this device's own assignment* lives in one place
    /// and both feed it, rather than each host re-deriving it.
    pub fn custodian_row(&self) -> fauna_core::data::CustodianRowRef<'_> {
        fauna_core::data::CustodianRowRef {
            destination_id: &self.destination_id,
            kind: &self.kind,
            custodian_device_id: self.custodian_device_id.as_deref(),
            capacity_cap_bytes: self.capacity_cap_bytes,
        }
    }
}

// ── fauna.backup.destination.{attach_folder,detach_folder} ────────────────────
//
// Ordinary-folder coverage — destination places
// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
// Spoken by the owner's client to its own *source* nest: enrollment stays
// per-actor, coverage is per-folder opt-in, and the write verbs belong to the
// backup plane — deliberately NOT a widened `fauna.folders.places.set`, whose
// request is device-shaped (the folders plane owns device seats, the backup
// plane owns custody coverage). Owner is always the authenticated caller.

/// `fauna.backup.destination.attach_folder` request — give one of the owner's
/// ordinary folders a destination place on an already-registered destination.
/// Idempotent on `(destination_id, folder_id)`: re-attaching is a no-op refresh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttachFolderRequest {
    /// The registered destination (matches `DestinationItem::destination_id`).
    pub destination_id: String,
    /// The folder's row id on the owner's own nest (`FolderSummary::id` — the
    /// `FolderRef::Local` id). Only the owner's own, non-reserved folders
    /// attach; the nest refuses anything else.
    pub folder_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttachFolderReply {
    /// `true` if a coverage row was newly created; `false` if the folder was
    /// already attached (idempotent re-attach).
    pub attached: bool,
    /// The canonical destination-side reserved set name for this coverage —
    /// `__folder/<source-nest-id-hex>/<folder-id>`. The nest derives it (only
    /// it authoritatively knows its own id), and the client records it verbatim
    /// as the config row's `folder_name`, so the naming rule lives in one place.
    pub folder_set: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.destination.detach_folder` request — remove a folder's
/// destination place. The coordinator's next pass tears the mirrored corpus
/// down per-path (tombstones under the grace window T), exactly as destination
/// removal does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DetachFolderRequest {
    pub destination_id: String,
    pub folder_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DetachFolderReply {
    /// `true` if a coverage row existed and was removed; `false` if none was
    /// attached (idempotent detach).
    pub detached: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One covered ordinary folder on a [`DestinationItem`] — the read-back of an
/// attach. Carries the folder's id (what a folders-page render joins on), the
/// derived set name (what a custodian pull namespaces its local store by) and,
/// since 2026-09-29, the folder's display name (what a re-seed restores the
/// folder under), so no consumer re-derives the naming rule and none has to
/// ask the source for a label after the source is gone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoveredFolder {
    /// The source-nest folder row id (`AttachFolderRequest::folder_id`).
    pub folder_id: i64,
    /// The destination-side reserved set name
    /// (`AttachFolderReply::folder_set`).
    pub folder_set: String,
    /// The folder's display name on the source nest — the label a covered-folder
    /// materialize restores it under (`segment-backup-protocol.md` § Client-device
    /// custodian (pull) → *Restore* → *Where a restored folder's name comes
    /// from*). This listing is the owner's own, read by the owner's own device,
    /// so the label may ride it where destination custody deliberately never
    /// carries one. Additive (2026-09-29): absent from a reply whose set carries
    /// no label, and then the custodian simply holds no name for the set — the re-seed
    /// reports that set unnamed rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The folder's `name_hash` on the source — the set's address once its
    /// plaintext name leaves the row (`path-sealing.md` § the set-name plane).
    /// Additive (2026-10-02): absent on a row that never carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// The folder's `name_sealed` on the source, verbatim — the label sealed
    /// under the set's root, salted by the name (`label_custody::seal_set_name`),
    /// so it opens on any nest the same owner restores to. A custodian records
    /// the pair beside [`Self::name`] and a restore names its target by the
    /// hash. Additive (2026-10-02), absent beside an absent `name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.backup.generation.{list,restore} ────────────────────────────────────
//
// The **destination**-side custody grace window, spoken by the owner's client
// over its own authed connection — never by the source nest. That separation is
// the whole point: the source nest is the custody *writer*, so it is exactly the
// party a rogue-source recovery must not go through
// (`message-segment-store.md` § Cross-location backup protocol).

/// `fauna.backup.generation.list` request — the authenticated owner's retained
/// superseded generations across every custody set the destination holds for
/// them. The reply is owner-scoped by construction.
///
/// **Paged (additive, 2026-07-29).** A retained-generation storm is exactly
/// when this read matters (a rogue source superseding everything), so the reply
/// is cut at the ratified frame budget (`transport.md` § Max frame corollary)
/// and walked by cursor. The cursor is **server-minted** — the deterministic
/// order's tiebreaker is a rowid no client can see — so the walk follows
/// [`GenerationListReply::next_cursor`] to absence; an absent `next_cursor`
/// (a reply that fits one page) ends the walk after that page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationListRequest {
    /// Opaque resume token from the previous page's `next_cursor`. Absent =
    /// first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Max records for this page. Non-positive/absent = no row bound before
    /// the byte budget — so a cursor-less first-page caller sees as much as
    /// the frame can carry.
    #[serde(default)]
    pub limit: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationListReply {
    pub generations: Vec<GenerationItem>,
    /// The grace window `T` in seconds, so a client renders each generation's
    /// restore deadline without hard-coding a constant the nest owns.
    pub grace_secs: i64,
    /// Present iff more rows remain past this page — pass it back as
    /// [`GenerationListRequest::cursor`]. Absent on the final page (including a reply
    /// that fits one page).
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One retained superseded custody generation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationItem {
    /// The custody set's reserved name (`__mail`, `__post`, `__conv/<hex>`).
    pub folder_name: String,
    /// Plaintext path, when the superseded custody row carried one (`path_hash`
    /// is one-way, so a sealed or scrubbed row has none).
    #[serde(default)]
    pub path: Option<String>,
    /// Hex-encoded 32-byte path hash — the restore address, always present even
    /// when `path` is not.
    pub path_hash: String,
    /// Hex-encoded 32-byte manifest hash of this generation.
    pub manifest_hash: String,
    pub size_bytes: i64,
    /// Unix seconds at which this generation stopped being live — its `T` clock
    /// start; it is reclaimed at `superseded_at + grace_secs`.
    pub superseded_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.generation.restore` request — promote one retained generation
/// back to live for its path. Addressed by `(folder_name, path_hash,
/// manifest_hash)` exactly as `list` reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationRestoreRequest {
    pub folder_name: String,
    /// Hex-encoded 32-byte path hash.
    pub path_hash: String,
    /// Hex-encoded 32-byte manifest hash of the generation to promote.
    pub manifest_hash: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationRestoreReply {
    /// `true` if the generation was promoted; `false` if no such retained
    /// generation exists for this owner (unknown, or already reclaimed past `T`).
    pub restored: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.backup.custody.list ────────────────────────────────────────────────
//
// The **destination**-side read of what it is *currently* holding for this
// owner — the observable the client-side audit loop
// (`../behavior/backup-restore.md` § Background Tasks) checks freshness and
// inclusion against.
//
// It is deliberately a *destination* kind spoken over the owner's own authed
// connection, for the same reason `generation.list` is: the source nest is the
// custody **writer**, so everything it says about the state of a backup is
// self-reported. `fauna.backup.status` (source-side) reports the source's own
// `segment_backup_state` rows; this kind reports what the party actually
// holding the bytes says it has. An audit that trusted the former would be
// asking the suspect to vouch for itself.
//
// The sibling `generation.list` reports only the **superseded** generations
// retained inside the grace window `T`; this reports the **live** latest-per-path
// custody. The two together are the owner's complete view of a destination.

/// `fauna.backup.custody.list` request — the authenticated owner's live custody
/// rows at this destination. The reply is owner-scoped by construction, and
/// **paged exactly like [`GenerationListRequest`]** (additive, 2026-07-29):
/// this is the audit loop's read, so the same storm that floods the generation
/// list must not blind the mechanism that warns about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyListRequest {
    /// Opaque resume token from the previous page's `next_cursor`. Absent =
    /// first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Max records for this page; non-positive/absent = no row bound before
    /// the byte budget (see [`GenerationListRequest::limit`]).
    #[serde(default)]
    pub limit: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyListReply {
    pub items: Vec<CustodyItem>,
    /// Present iff more rows remain past this page — pass it back as
    /// [`CustodyListRequest::cursor`]. Absent on the final page (including a reply
    /// that fits one page).
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One live custody row — the destination's own statement of what it holds at
/// this path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyItem {
    /// The custody set's reserved name (`__mail`, `__post`, `__conv/<hex>`).
    pub folder_name: String,
    /// Plaintext path, when the custody row carries one (`path_hash` is one-way,
    /// so a sealed or scrubbed row has none) — for segment
    /// backup this is `"{scope_hex}/seg-{id:08}.dat"` or the manifest mirror.
    #[serde(default)]
    pub path: Option<String>,
    /// Hex-encoded 32-byte path hash — always present, even when `path` is not.
    pub path_hash: String,
    /// Hex-encoded 32-byte manifest hash of the live generation at this path.
    pub manifest_hash: String,
    /// The bytes this row holds — and charges. On a reserved destination set
    /// this is the **destination-derived** stored size (the manifest blob plus
    /// its distinct chunks, as this nest measured them at upload), never the
    /// writer's declaration — the destination's own statement of what it
    /// holds, like `updated_at` below. (Rows written before the 2026-07-29
    /// derivation carry the writer-declared size until re-recorded.)
    pub size_bytes: i64,
    /// Unix seconds at which the destination last accepted custody for this
    /// path. This is the **destination's** clock stamped on receipt, not a
    /// timestamp the writer supplied — which is what makes the audit's
    /// freshness check source-untrusted.
    pub updated_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// -- fauna.backup.custody.materialize -----------------------------------------
//
// Phase 3 of the re-seed ceremony (`../behavior/backup-destinations.md`
// § Third destination kind -> *Re-seed*): flip ONE custody set from destination
// posture to live source posture, under the owner's ordinary enrollment-time
// `NestBackupKey` grant.
//
// It is a **destination**-side kind spoken over the owner's own authed
// connection, like its `custody.list` / `generation.*` siblings: the party that
// holds the bytes is the party that can reconstitute them. The nest derives the
// owning actor from the connection, so the set a caller can name is always their
// own -- the request carries no actor and no key.
//
// **Nothing here deletes.** A target that already holds live records is refused,
// typed (`fauna.backup.target_not_empty`), with no force arm anywhere in the
// wire shape: merging a backed-up corpus into a lived-in account is a named
// non-goal, and *No user-data loss* holds structurally rather than by care.

/// `fauna.backup.custody.materialize` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyMaterializeRequest {
    /// The custody set to flip, by the reserved name its rows carry -- `__mail`
    /// today; `__conv/<hex>` and the `__folder/<nest>/<id>` mirror axis when
    /// their arms land. The nest re-derives what the name means rather than
    /// trusting its shape (`segment_backup::parse_reserved_backup_set_name`), so
    /// an unrecognised name is a refusal, never an unscoped read.
    pub set_name: String,
    /// The covered-folder arm's display name -- the one thing custody
    /// deliberately never carried, because a mirror holds the folder's sealed
    /// paths and not its label. Absent for a segment-axis set, which needs no
    /// name at all. A `__folder/...` set, whose own name carries only the
    /// source nest id and the source's folder rowid (neither meaningful on the
    /// target), needs this or [`Self::folder_name_hash`]; carrying neither is a
    /// typed refusal rather than a guess.
    #[serde(default)]
    pub folder_display_name: Option<String>,
    /// The covered-folder arm's target by address: the restored folder's
    /// `name_hash` (`CoveredFolder::name_hash`, as the custodian recorded it).
    /// When present the nest resolves the target set by it alone -- a sealed
    /// set's row rests no plaintext name -- and refuses a
    /// [`Self::folder_display_name`] that rides beside it without hashing to
    /// it. Absent → the target resolves by the display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name_hash: Option<ByteBuf>,
    /// The covered-folder arm's signer: the key every carried
    /// [`Self::signatures`] entry verifies under -- the owner's identity key
    /// (a direct signature) or a `SyncWrite` principal key the owner registered
    /// on this nest. Absent on a segment-axis set, which mints no signed row
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<ByteBuf>,
    /// One page of the covered-folder re-home's owner signatures, keyed by the
    /// re-homed row's `path_hash`, at most [`MATERIALIZE_REHOME_PAGE`] per
    /// request. The nest re-homes exactly the rows carried here whose
    /// signature verifies, one transaction per page; a folder-set request
    /// carrying none is refused `signature_required`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<RehomeSignature>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The most [`CustodyMaterializeRequest::signatures`] one request carries --
/// the folder arm's page. One transaction per page bounds the nest's write
/// lock however large the restored folder is; the driver loops until the
/// reply's `remaining` is zero.
pub const MATERIALIZE_REHOME_PAGE: usize = 256;

/// One re-homed row's owner signature over its
/// `fauna_protocol::sync_writer_sig::SignedChange::for_rehome` statement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RehomeSignature {
    /// The row's (source) path hash -- 32 bytes, the custody row it re-homes.
    pub path_hash: ByteBuf,
    /// The 64-byte ed25519 signature.
    pub signature: ByteBuf,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline rule 4); empty on every write today.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.custody.materialize` reply -- what the flip actually did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyMaterializeReply {
    /// The segment ids now live in the scope's segment area, ascending.
    pub segments: Vec<u32>,
    /// Mirror rows re-asserted from the reconstituted sidecars' own footers.
    pub records: u64,
    /// Always `true` on success, and the reply's whole editorial content: the
    /// custody set is now **redundant** -- its bytes are live in the scope, so
    /// keeping it is paying twice. Reclaiming it is the owner's existing
    /// set-delete authority, a separate deliberate gesture, and never something
    /// this verb does on their behalf.
    pub custody_redundant: bool,
    /// How many of those records the same call **filed**: placement rows
    /// replayed from the kind's placement journal, which rides the set beside
    /// the content (`behavior/backup-destinations.md` § Third destination kind
    /// → *Where restored mail lands*).
    ///
    /// `None` and `Some(0)` are different facts, which is why this is not a
    /// bare count. `None`: the kind has **no placement layer** (posts, and
    /// the covered-folder axis). `Some(0)`: the copy held a journal and the
    /// journal files nothing, which is simply an account with no mail in any
    /// mailbox. A journaled kind's copy without its journal is refused as a
    /// delivery still under way, never restored unfiled.
    #[serde(default)]
    pub placements: Option<u64>,
    /// The covered-folder arm's paging: how many of the set's custody rows are
    /// still not live in the target after this page. The driver sends the next
    /// page while it is non-zero. `None` on the segment axis, which flips the
    /// whole set in one call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// -- fauna.backup.custody.recover ---------------------------------------------
//
// The lived-in recovery (`segment-backup-protocol.md` § Client-device custodian
// (pull) -> *Restore* -> *Recovery into the lived-in nest that regressed*):
// materialize's sibling for the one target materialize refuses -- an owner's
// source nest that came back from an older copy of its data directory. Spoken
// over the owner's own authed connection to that source, after the delivery
// leg has landed the destination's live and retained generations in the
// source's own reserved set. Owner-scoped exactly like materialize: the nest
// derives the actor from the connection, so the request names a set, never an
// owner, and carries no key.
//
// **Nothing here deletes, and nothing is refused for what the target holds.**
// The lost records come back as records -- appended under the target's own
// counter, filed where the newest delivered journal put them under fresh UIDs
// -- and every record the target holds, live or tombstoned, is its history and
// is left alone. So there is no force arm, and a repeat recovers nothing.

/// `fauna.backup.custody.recover` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyRecoverRequest {
    /// The reserved segment-axis set the delivery leg landed into -- `__mail`.
    /// Re-derived, never trusted by shape (`parse_reserved_backup_set_name`).
    pub set_name: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.backup.custody.recover` reply -- counts, and the floor applied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyRecoverReply {
    /// Records the target did not hold, now appended and live.
    pub recovered: u64,
    /// Records the delivered segments hold that the target already held, live
    /// or tombstoned -- its own history, untouched.
    pub already_held: u64,
    /// Recovered records filed where the newest delivered journal naming them
    /// put them (mailbox and flags; the UID is the target's own, fresh).
    pub filed: u64,
    /// Recovered mail records no delivered journal names, landed in the inbox
    /// unseen.
    pub inboxed: u64,
    /// Records given no place and so not appended (a calendar or contacts
    /// record no journal names).
    pub unplaceable: u64,
    /// The counter floor applied to the target's segment counter -- the
    /// greatest ledger generation over the delivered mirrors. `None` when no
    /// mirror named anything.
    #[serde(default)]
    pub floor: Option<u32>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn custody_list_reply_round_trips_with_items() {
        let reply = CustodyListReply {
            items: vec![CustodyItem {
                folder_name: "__mail".into(),
                path: Some("4242/seg-00000001.dat".into()),
                path_hash: "aa".repeat(32),
                manifest_hash: "bb".repeat(32),
                size_bytes: 4096,
                updated_at: 1_700_000_000,
                extra: Default::default(),
            }],
            next_cursor: Some("1700000000:7".into()),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: CustodyListReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    /// A path-less historical row (the additive `path` column's NULL case) must
    /// still ride the wire — the audit addresses by `path_hash`, so a `None`
    /// path degrades the label, never the check.
    #[test]
    fn custody_item_round_trips_without_a_plaintext_path() {
        let item = CustodyItem {
            folder_name: "__mail".into(),
            path: None,
            path_hash: "cc".repeat(32),
            manifest_hash: "dd".repeat(32),
            size_bytes: 1,
            updated_at: 42,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&item).unwrap();
        let back: CustodyItem = decode(&bytes).unwrap();
        assert_eq!(back, item);
        assert!(back.path.is_none());
    }

    #[test]
    fn grant_request_round_trips_the_key() {
        let req = NestKeyGrantRequest {
            nest_backup_key: ByteBuf::from(vec![0xABu8; 32]),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: NestKeyGrantRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.nest_backup_key.len(), 32);
    }

    #[test]
    fn status_reply_round_trips_with_destination_rows() {
        let reply = BackupStatusReply {
            enrolled: true,
            destinations: vec![BackupDestinationStatusItem {
                destination_id: "dest-1".into(),
                last_upload_time: Some(1_800_000_000),
                backlog_count: 3,
                ..Default::default()
            }],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: BackupStatusReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn writer_grant_list_reply_round_trips() {
        let reply = WriterGrantListReply {
            grants: vec![WriterGrantItem {
                writer_nest_id: "aa".repeat(32),
                granted_at: 1_800_000_000,
                ..Default::default()
            }],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: WriterGrantListReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn destination_register_request_round_trips() {
        let req = DestinationRegisterRequest {
            destination_id: "dest-1".into(),
            destination_nest_url: "https://d1.example".into(),
            destination_nest_id: "51".repeat(32),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: DestinationRegisterRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn a_client_device_registration_round_trips() {
        let req = DestinationRegisterRequest {
            destination_id: "dest-ipad".into(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some("dev-abc".into()),
            capacity_cap_bytes: Some(64 << 30),
            // A custodian has no address: both nest fields stay empty, and the
            // nest must never dial them.
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: DestinationRegisterRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
        assert!(back.destination_nest_url.is_empty());
    }

    #[test]
    fn a_registry_row_projects_into_the_shared_custodian_rule() {
        // The registry row is the *only* copy the bearer-only sync agent can
        // read (`apps/sync-agent.md` § Control plane split), so it must feed the
        // same matcher the seed-holding app feeds from the at-rest row —
        // including the cap, which is the kind's only knob.
        let item = DestinationItem {
            destination_id: "dest-laptop".into(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some("dev-abc".into()),
            capacity_cap_bytes: Some(64 << 30),
            ..Default::default()
        };
        let bytes = encode_canonical(&item).unwrap();
        let back: DestinationItem = decode(&bytes).unwrap();
        assert_eq!(back, item);

        let found = fauna_core::data::custodian_assignment_for(
            std::iter::once(back.custodian_row()),
            "dev-abc",
        )
        .expect("this device's own row");
        assert_eq!(found.destination_id, "dest-laptop");
        assert_eq!(found.capacity_cap_bytes, Some(64 << 30));

        // And a peer-nest row in the same reply assigns nothing, whoever asks.
        let nest_row = DestinationItem {
            destination_id: "dest-1".into(),
            destination_nest_url: "https://d1.example".into(),
            ..Default::default()
        };
        assert!(
            fauna_core::data::custodian_assignment_for(
                std::iter::once(nest_row.custodian_row()),
                "dev-abc",
            )
            .is_none()
        );
    }

    #[test]
    fn a_registry_row_without_a_cap_reads_as_uncapped() {
        // Additive-everywhere: an omitted cap key is the live uncapped setting, and
        // the custodian must read "no cap set" rather than fail to decode. (An
        // uncapped custodian fills the disk it was given, which is the
        // documented meaning of `None` — not a silent zero cap, which would
        // report cap-reached forever having stored nothing.)
        #[derive(Serialize)]
        struct CaplessItem {
            destination_id: String,
            destination_nest_url: String,
            destination_nest_id: String,
            mode: String,
            added_at: i64,
            kind: String,
            custodian_device_id: Option<String>,
        }
        let capless = CaplessItem {
            destination_id: "dest-laptop".into(),
            destination_nest_url: String::new(),
            destination_nest_id: String::new(),
            mode: "backup".into(),
            added_at: 1_800_000_000,
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some("dev-abc".into()),
        };
        let bytes = encode_canonical(&capless).unwrap();
        let back: DestinationItem = decode(&bytes).unwrap();
        assert_eq!(back.capacity_cap_bytes, None);
        let found = fauna_core::data::custodian_assignment_for(
            std::iter::once(back.custodian_row()),
            "dev-abc",
        )
        .expect("still assigns work");
        assert_eq!(found.capacity_cap_bytes, None);
    }

    #[test]
    fn a_registration_without_kind_means_nest() {
        // The wire half of the at-rest property `fauna-core` pins: a
        // registration that omits `kind` must read as the peer-nest kind
        // rather than as an empty/unknown one.
        #[derive(Serialize)]
        struct KindlessRegister {
            destination_id: String,
            destination_nest_url: String,
            destination_nest_id: String,
            mode: String,
        }
        let kindless = KindlessRegister {
            destination_id: "dest-1".into(),
            destination_nest_url: "https://d1.example".into(),
            destination_nest_id: "51".repeat(32),
            mode: "backup".into(),
        };
        let bytes = encode_canonical(&kindless).unwrap();
        let back: DestinationRegisterRequest = decode(&bytes).unwrap();
        assert_eq!(back.kind, fauna_core::data::DESTINATION_KIND_NEST);
        assert_eq!(back.custodian_device_id, None);
        assert_eq!(back.destination_nest_url, "https://d1.example");
    }

    #[test]
    fn default_register_request_is_a_nest_registration() {
        // Guards the derived-Default trap: a struct-update fixture must not put
        // an empty kind on the wire.
        assert_eq!(
            DestinationRegisterRequest::default().kind,
            fauna_core::data::DESTINATION_KIND_NEST
        );
        assert_eq!(
            DestinationItem::default().kind,
            fauna_core::data::DESTINATION_KIND_NEST
        );
    }

    #[test]
    fn custodian_checkin_round_trips_and_carries_cap_state() {
        let req = CustodianCheckinRequest {
            destination_id: "dest-ipad".into(),
            high_water: 4_096,
            held_bytes: 12_345_678,
            cap_state: CAP_STATE_REACHED.into(),
            audit_state: Some(AUDIT_STATE_FAILED.into()),
            last_audit_passed_at: Some(1_700_000_000),
            device_id: Some("dev-ipad".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: CustodianCheckinRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        // The carrier stays `Option` for wire shape, so an absent `device_id`
        // still decodes; the nest then REFUSES the check-in rather than
        // accepting it (see the carrier's doc above).
        let unnamed = CustodianCheckinRequest {
            device_id: None,
            ..req.clone()
        };
        let bytes = encode_canonical(&unnamed).unwrap();
        let back: CustodianCheckinRequest = decode(&bytes).unwrap();
        assert_eq!(back.device_id, None);
        assert_eq!(back.high_water, 4_096);

        let reply = CustodianCheckinReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<CustodianCheckinReply>(&bytes).unwrap(), reply);
    }

    /// A check-in that omits the audit carrier must
    /// decode as **not-yet-audited**, never as a failure.
    ///
    /// A custodian that has not audited yet sends exactly these four fields.
    /// Reading their silence as
    /// [`AUDIT_STATE_FAILED`] would light up a data-loss alarm on every one of
    /// them the day the nest half lands — the loudest possible false positive,
    /// fleet-wide, on the feature built for nest loss.
    #[test]
    fn a_checkin_without_the_audit_carrier_decodes_as_not_yet_audited() {
        #[derive(Serialize)]
        struct PreAuditCheckin {
            destination_id: String,
            high_water: u64,
            held_bytes: u64,
            cap_state: String,
        }

        let bytes = encode_canonical(&PreAuditCheckin {
            destination_id: "dest-ipad".into(),
            high_water: 9,
            held_bytes: 4_096,
            cap_state: CAP_STATE_OK.into(),
        })
        .unwrap();

        let back: CustodianCheckinRequest = decode(&bytes).unwrap();
        assert_eq!(back.cap_state, CAP_STATE_OK);
        assert_eq!(
            back.audit_state, None,
            "a custodian's silence is 'has not audited', not 'failed'"
        );
        assert_eq!(back.last_audit_passed_at, None);
    }

    #[test]
    fn a_custodian_status_row_distinguishes_cap_reached_from_lag() {
        // Both rows are behind. Only one has stopped pulling, and a client that
        // read `backlog_count` alone could not tell them apart — which is why
        // `cap_state` is a distinct field rather than an inferred threshold.
        let lagging = BackupDestinationStatusItem {
            destination_id: "dest-ipad".into(),
            backlog_count: 42,
            held_bytes: Some(1_000),
            cap_state: Some(CAP_STATE_OK.into()),
            ..Default::default()
        };
        let capped = BackupDestinationStatusItem {
            cap_state: Some(CAP_STATE_REACHED.into()),
            ..lagging.clone()
        };
        assert_ne!(lagging.cap_state, capped.cap_state);

        let bytes = encode_canonical(&capped).unwrap();
        let back: BackupDestinationStatusItem = decode(&bytes).unwrap();
        assert_eq!(back, capped);
    }

    #[test]
    fn a_nest_status_row_omits_the_custodian_only_fields() {
        // A nest destination has no cap and no held-bytes figure; the render
        // must be able to tell "not applicable" from "zero".
        let nest_row = BackupDestinationStatusItem {
            destination_id: "dest-1".into(),
            last_upload_time: Some(1_800_000_000),
            backlog_count: 0,
            ..Default::default()
        };
        assert_eq!(nest_row.held_bytes, None);
        assert_eq!(nest_row.cap_state, None);
    }

    #[test]
    fn destination_list_reply_round_trips() {
        let reply = DestinationListReply {
            destinations: vec![DestinationItem {
                destination_id: "dest-1".into(),
                destination_nest_url: "https://d1.example".into(),
                destination_nest_id: "51".repeat(32),
                added_at: 1_800_000_000,
                ..Default::default()
            }],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: DestinationListReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn status_reply_empty_destinations_and_none_time_round_trip() {
        // The slice-2 shape (no coordinator yet): enrolled flag, no destinations.
        let reply = BackupStatusReply {
            enrolled: false,
            destinations: vec![],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: BackupStatusReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn generation_list_reply_round_trips_including_a_path_less_row() {
        // `path` is `Option` because `path_hash` is one-way: custody rows
        // recorded before the additive `backup_custody.path` column carry no
        // plaintext path, and such a generation must still be listable and
        // restorable (it is addressed by `path_hash`, which is always present).
        let reply = GenerationListReply {
            generations: vec![
                GenerationItem {
                    folder_name: "__mail".into(),
                    path: Some("seg-00000001.dat".into()),
                    path_hash: "a1".repeat(32),
                    manifest_hash: "b2".repeat(32),
                    size_bytes: 4096,
                    superseded_at: 1_800_000_000,
                    extra: Default::default(),
                },
                GenerationItem {
                    folder_name: format!("__conv/{}", "cc".repeat(32)),
                    path: None,
                    path_hash: "c3".repeat(32),
                    manifest_hash: "d4".repeat(32),
                    size_bytes: 17,
                    superseded_at: 1_800_000_100,
                    extra: Default::default(),
                },
            ],
            grace_secs: 30 * 24 * 60 * 60,
            next_cursor: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: GenerationListReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn generation_restore_round_trips() {
        let req = GenerationRestoreRequest {
            folder_name: "__mail".into(),
            path_hash: "a1".repeat(32),
            manifest_hash: "b2".repeat(32),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: GenerationRestoreRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = GenerationRestoreReply {
            restored: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: GenerationRestoreReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn custody_recover_round_trips() {
        let req = CustodyRecoverRequest {
            set_name: "__mail".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: CustodyRecoverRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = CustodyRecoverReply {
            recovered: 3,
            already_held: 5,
            filed: 2,
            inboxed: 1,
            unplaceable: 0,
            floor: Some(9),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: CustodyRecoverReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn custody_materialize_round_trips() {
        let req = CustodyMaterializeRequest {
            set_name: "__mail".into(),
            folder_display_name: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: CustodyMaterializeRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = CustodyMaterializeReply {
            segments: vec![1, 2, 7],
            records: 42,
            custody_redundant: true,
            placements: Some(40),
            remaining: Some(3),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: CustodyMaterializeReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    /// The placement count is additive in both directions
    /// (`version-compatibility.md`): a reply minted by a nest that predates it
    /// decodes here as "no journal was restored", and a reply that carries it
    /// decodes on a reader that predates it, which keeps the key it does not
    /// know rather than refusing the reply.
    #[test]
    fn custody_materialize_placements_is_additive() {
        /// The reply as it was before the field existed.
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Before {
            segments: Vec<u32>,
            records: u64,
            custody_redundant: bool,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }

        let old_nest = encode_canonical(&Before {
            segments: vec![1],
            records: 3,
            custody_redundant: true,
            extra: Default::default(),
        })
        .unwrap();
        let read_here: CustodyMaterializeReply = decode(&old_nest).unwrap();
        assert_eq!(read_here.records, 3);
        assert_eq!(read_here.placements, None);

        let new_nest = encode_canonical(&CustodyMaterializeReply {
            segments: vec![1],
            records: 3,
            custody_redundant: true,
            placements: Some(3),
            ..Default::default()
        })
        .unwrap();
        let read_by_old: Before = decode(&new_nest).unwrap();
        assert_eq!(read_by_old.records, 3);
        assert!(
            read_by_old.extra.contains_key("placements"),
            "an older reader keeps the key it does not know"
        );
    }

    /// The folder arm's display name is additive: a request minted before it
    /// existed decodes on a nest that has it, and vice versa. (The compat rule
    /// every wire type here lives under -- `version-compatibility.md`.)
    #[test]
    fn custody_materialize_display_name_is_additive() {
        let with_name = CustodyMaterializeRequest {
            set_name: "__folder/aa/7".into(),
            folder_display_name: Some("Photos".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&with_name).unwrap();
        let back: CustodyMaterializeRequest = decode(&bytes).unwrap();
        assert_eq!(back, with_name);
    }

    /// The folder arm's signature page is additive both ways
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)): a request with no
    /// signatures encodes byte-identically to one minted before the fields
    /// existed, and a signed page round-trips and leaves an older reader its
    /// unknown keys rather than a refusal.
    #[test]
    fn custody_materialize_signature_page_is_additive() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Before {
            set_name: String,
            #[serde(default)]
            folder_display_name: Option<String>,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }
        let unsigned = CustodyMaterializeRequest {
            set_name: "__mail".into(),
            ..Default::default()
        };
        assert_eq!(
            encode_canonical(&unsigned).unwrap(),
            encode_canonical(&Before {
                set_name: "__mail".into(),
                folder_display_name: None,
                extra: Default::default(),
            })
            .unwrap(),
            "a segment-axis request is unchanged on the wire"
        );

        let page = CustodyMaterializeRequest {
            set_name: "__folder/aa/7".into(),
            folder_display_name: Some("Photos".into()),
            folder_name_hash: Some(ByteBuf::from(vec![9u8; 32])),
            signer_key: Some(ByteBuf::from(vec![7u8; 32])),
            signatures: vec![RehomeSignature {
                path_hash: ByteBuf::from(vec![1u8; 32]),
                signature: ByteBuf::from(vec![2u8; 64]),
                ..Default::default()
            }],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&page).unwrap();
        let back: CustodyMaterializeRequest = decode(&bytes).unwrap();
        assert_eq!(back, page);
        let old: Before = decode(&bytes).unwrap();
        assert!(old.extra.contains_key("signatures"));
        assert!(old.extra.contains_key("signer_key"));
        assert!(old.extra.contains_key("folder_name_hash"));
    }
}
