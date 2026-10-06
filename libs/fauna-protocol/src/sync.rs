//! Device-sync control-plane WS-RPC payload types — `fauna.sync.{register,
//! changes.{list,record},status,files,backup_status,devices.{list,delete}}`.
//! A behavior-preserving transport migration of the bearer-authed device-sync
//! routes (`sync_routes`) — Track B13 of the WS-RPC-everywhere migration
//! (tracked internally). These kinds ride the **bearer**
//! connection (`GET /api/v1/ws/{actor_id}`); the connection authenticates the
//! actor, so every kind the HTTP twin scoped on `bearer.0.0` scopes on the
//! connection `actor_id` — and the twins' explicit `actor_id` request field is
//! dropped (it is implicit in the connection).
//!
//! The sibling `fauna.sync.conflicts.{list,report,resolve}` kinds live in
//! `folders.rs` (they are a folder sub-feature, migrated in B14). The bulk
//! byte routes (`/chunks/*`, `/manifests/*`) stay HTTP residue — see
//! `docs/goal/architecture/api-layers.md` § File Sync. The server-side
//! single-file reassembly route (`GET /api/v1/sync/file/{*path}`) was deleted
//! 2026-07-14 — never a production caller, and the unconditional owner-only
//! chunk seal makes nest-side plaintext reassembly impossible by design.
//!
//! Wire-shape decisions:
//! - **device ids and path / manifest hashes ride as hex `String`** — matching
//!   the twins' `hex::encode` / `parse_hash`, and the B14 `fauna.sync.conflicts.*`
//!   device-id-hex precedent in `folders.rs`, so the whole `fauna.sync.*`
//!   namespace is uniform and client migration is a transport swap.
//! - sequence numbers / sizes / timestamps as `i64`; capabilities as the
//!   comma-separated `String` the `sync_devices` column stores (e.g.
//!   `"read,write"`).
//! - No floats anywhere. Kind registry: `kind.rs::register_sync_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

fn default_capabilities() -> String {
    "read,write".to_string()
}

// ── fauna.sync.register (≡ POST /api/v1/sync/register) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncRegisterRequest {
    /// Hex-encoded 32-byte device id (random per-device, not tied to the actor
    /// keypair).
    pub device_id: String,
    /// Human-readable device name shown in device-management UI.
    pub label: String,
    /// [`Self::label`], **sealed** — a `fauna_core::path_crypto::SealedLabel`
    /// over the user-chosen device name, stored verbatim in the nest's
    /// `sync_devices.label_sealed` column (`file-sync.md` § Sealed names &
    /// paths, the *paths-are-content* ruling). Minted only through
    /// `fauna_core::label_custody::seal_device_label`, never ad hoc.
    ///
    /// Sealed under the **registering owner's** root (salt = this request's
    /// `device_id`, random nonce) — a device belongs to the actor, not to any
    /// folder, so this is deliberately *not* a set's content key. `None` on a
    /// machine-authored label (the funnel refuses those) and from a keyless
    /// writer; such a row rests **nameless** until the device's next keyed
    /// register (`file-sync.md` § Sealed names & paths). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_sealed: Option<ByteBuf>,
    /// Comma-separated capabilities (`"read"`, `"write"`, or both); defaults to
    /// `"read,write"` when absent (the twin's `default_capabilities`).
    #[serde(default = "default_capabilities")]
    pub capabilities: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Catalog-aligned defaults so fixtures can use `..Default::default()` rather
/// than hand-listing every field — two branches independently growing this
/// struct then merge cleanly instead of colliding on the grown axis
/// (the ratified fixture-shape convention). Note `capabilities` defaults to the
/// wire default, **not** the empty string a derived `Default` would give.
impl Default for SyncRegisterRequest {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            label: String::new(),
            label_sealed: None,
            capabilities: default_capabilities(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncRegisterReply {
    /// Echoes the registered (hex) device id (the twin returned `{ok: true}`;
    /// the echo lets the client confirm the id the nest stored).
    pub device_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.device_grant.register (renewal grant; additive 2026-07-19) ────

/// Store a root-key-signed, `RenewBearer`-scoped `DeviceAuthorization` on the
/// actor's `sync_devices` row — the registration half of the sync agent's
/// app-dead bearer renewal (`docs/goal/architecture/apps/sync-agent.md`
/// § Credential model; the mint half is `fauna.auth.device_handshake`).
/// WS-RPC-native (no HTTP twin ever existed). Deleting the device
/// (`fauna.sync.devices.delete`) deletes the grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceGrantRegisterRequest {
    /// Hex-encoded 32-byte sync device id the grant attaches to — must name an
    /// already-registered `sync_devices` row of the connection actor
    /// (`fauna.sync.register` first).
    pub device_id: String,
    /// Signed `DeviceAuthorization` in the embed-as-bytes wire shape: signed by
    /// the actor's root key, `device_key` = the renewal device public key,
    /// `capabilities` containing `RenewBearer`. The nest verifies before
    /// storing and re-verifies at every `fauna.auth.device_handshake` mint.
    pub authorization: fauna_core::encoding::EmbedAsBytes,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceGrantRegisterReply {
    /// `true` — the grant verified and is stored (an error reply carries every
    /// failure; the field exists so the reply is a struct, per convention).
    pub registered: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.device_grant.revoke (grant retirement; additive 2026-08-15) ───

/// Retire ONE renewal grant, named by its device public key — never the whole
/// device row (`fauna.sync.devices.delete` stays the devices-UI gesture).
/// Its live consumers are the signed-out reconcile's nest-side sign-out revoke
/// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
/// block, decision 4: it clears the named row's grant columns and keeps the
/// row) and a runtime's revoke of a removed member's grant at every nest it
/// completes (`account-data-taxonomy.md` § Fleet-scope reclamation, clause (4)
/// → *The nest half follows merged state*). Added 2026-08-15 for the agent's
/// self-retirement of its legacy grant, which went with that grant.
///
/// **Two authorization arms, either sufficient.** The connection is always an
/// authenticated session of the owning account (the app/user arm). The
/// optional proof-of-possession triple below is the **self arm**: a signature
/// by the very key being retired, which is why a bearer-only process may
/// revoke its own credential without holding any authority over anyone else's.
/// Present-but-invalid is a refusal, never a silent fall-through to the
/// session arm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceGrantRevokeRequest {
    /// Hex-encoded 32-byte **renewal device public key** whose grant retires.
    /// The row it sits on is found by this key, so the caller needs no
    /// device_id (and cannot reach a row by naming one).
    pub device_key: String,
    /// Self-arm: milliseconds since epoch, within the `device_handshake` drift
    /// window. `None` ⇒ the app/user arm alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    /// Self-arm: hex nonce uniquifying the deterministic Ed25519 signature for
    /// the replay guard, exactly as the handshake's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Self-arm: hex Ed25519 signature by `device_key` over
    /// [`crate::auth::device_grant_revoke_signed_message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceGrantRevokeReply {
    /// Whether a stored grant was actually cleared. **`false` is success, not
    /// an error** — the key is tombstoned either way, so the caller's terminal
    /// state ("this key can never mint again") holds identically. The agent's
    /// retirement loop treats both as done (the ruling's evidence bound: a
    /// not-found answer counts, because the user may have deleted the row
    /// first).
    pub revoked: bool,
    /// How many live sessions the key had minted and lost with it. Reported
    /// for the caller's log; never load-bearing.
    pub sessions_revoked: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.changes.list (≡ GET /api/v1/sync/changes) ─────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncChangesListRequest {
    /// Folder name: returns that set's changes (ownership enforced against the
    /// connection actor). Optional only because [`Self::name_hash`] selects a
    /// set on its own; a request naming neither — and no `item_class` or
    /// cross-nest route — is refused, there being no actor-wide feed.
    #[serde(default)]
    pub folder: Option<String>,
    /// Hex device id to exclude from the results (a device polling its own
    /// set skips its own changes). Only honored in the `folder` branch.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Cross-nest relay (additive, Phase 2 — `federation.md` § Cross-nest
    /// shared folders + channel append): when set (with `channel_id`), the
    /// caller's own nest relays the read to the set's home nest via
    /// `fauna.federation.folder.changes.fetch` instead of reading locally.
    /// Permitted as an additive field by the ratified wire rule because the
    /// local path fails visibly (`not_found` — the caller's nest holds no
    /// row for a foreign set), never silently misdirects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_url: Option<String>,
    /// The foreign set's derived 32-byte `ChannelId` (hex) — the federated
    /// kinds are channel-keyed (a foreign set's `name` only resolves on its
    /// home nest). Required with `nest_url`; ignored without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Return changes with `seq` strictly greater than this (catch-up cursor).
    #[serde(default)]
    pub since: i64,
    /// Hash-first addressing (S5b, `file-sync.md` § Sealed names & paths):
    /// when present, resolves via `name_hash` before falling back to
    /// [`Self::folder`] — see
    /// `fauna_protocol::folders::FolderUpdateRequest::name_hash` for the
    /// full rationale. Same-nest reads only (a foreign set with `nest_url`
    /// set addresses by `channel_id`, so this is ignored alongside it). A
    /// malformed (non-32-byte) hash is refused, never silently ignored.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// **Item-class routing** (W2.3 of account-data-plane.md § Workstreams;
    /// `account-sync-plane.md` § Feeds and cursors → *Feed row + wire
    /// evolution*): which
    /// class of feed row the caller is asking for
    /// ([`crate::account_state::ItemClass::as_wire`]). Absent — the file-row
    /// request every client sends — serves file rows and **never** state
    /// entries: *"a file-sync client never sees state-entry rows unless it
    /// asks"*. Naming
    /// [`crate::account_state::ItemClass::StateEntry`] is what supersedes the
    /// sync plane's reserved-set refusal for the account-state scope, and is
    /// also what makes that supersession opt-in by request shape rather than a
    /// change to any shipped reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_class: Option<String>,
    /// **The scope** whose feed to serve (W2.3) — *"the unit of subscription and
    /// of frontier tracking"* (`account-sync-plane.md` § Feeds and cursors →
    /// *Scope partition — three scope families, one feed contract*), read only
    /// alongside [`Self::item_class`] and defaulting to
    /// [`crate::account_state::ACCOUNT_STATE_SCOPE`]. Distinct from
    /// [`Self::folder`], which names a *folder* scope by its nest-side set
    /// name; and distinct from `item_class`, because one scope carries several
    /// item classes (a content scope holds `record-cid` rows and tombstones) —
    /// scope says which feed, item class says which rows of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// **The frontier vector** (W2.3, `account-sync-plane.md` § Feeds and
    /// cursors → *The frontier vector — the cursor primitive*), beside
    /// [`Self::since`] rather than replacing it: hex `writer_id` → that
    /// writer's high-water `writer_seq`, for the **device** writers of a
    /// multi-master scope. The nest-writer slot *is* [`Self::since`], which is
    /// what the charter means by "an omitted frontier is `{nest: since}`" — so
    /// a request sending only `since` is already sending a complete,
    /// correct frontier for the one writer it knows about.
    ///
    /// A writer absent from the map is at high-water 0 (send me everything you
    /// have from them) — unless [`Self::held_through_seq`] is present, which
    /// serves every unnamed writer from above it instead. Frontier semantics
    /// are the shipped anchor's accounting law generalized per-writer: a
    /// high-water asserts every row ≤ it is applied or deliberately skipped,
    /// only an accounted walk advances it, and it never regresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<BTreeMap<String, i64>>,
    /// **The serve-order watermark** (`account-sync-plane.md` § Feeds and
    /// cursors → *Compaction is a serve-order watermark*), beside
    /// [`Self::frontier`] rather than replacing it: *every row you hold with
    /// `seq` at or below this, from any device writer I did not name in
    /// `frontier`, I hold.* A named writer keeps its per-writer gate
    /// unchanged; an unnamed one is served from above the watermark instead of
    /// from 0 — which is what lets a requester stop naming a writer whose rows
    /// the watermark already covers, so the request stops growing with history.
    ///
    /// A coordinate in the scope's one sequencing nest's log, exactly as
    /// [`Self::since`] is, and only ever sent from a
    /// [`SyncChangesListReply::complete_through_seq`] that nest echoed. Absent
    /// — every store-served leg, which has no
    /// cross-writer serve order to take one from — keeps the two-gate
    /// semantics byte-for-byte. Additive 2026-09-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_through_seq: Option<i64>,
    /// **Custody addressing** (W8.6 — `account-data-plane.md` § Replica
    /// posture → *The custody grant + ceremony*): 64-char hex of the OWNER
    /// whose account-state feed a CUSTODY-session caller is pulling. Absent
    /// — every non-custody caller — keeps today's caller-own semantics
    /// byte-for-byte; present, the state-entry arm authorizes by the
    /// caller's live custody row for exactly this owner (per-request
    /// re-check) and serves within the row's own scope set. Additive
    /// 2026-08-16; a non-custody caller naming it is refused, never
    /// silently self-served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of_owner: Option<String>,
    /// **The walker's identity for the feed's retention gate**
    /// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (1)): 64-char hex of the requesting replica's
    /// writer id. Read only beside [`Self::held_through_seq`] on the
    /// state-entry arm: the nest records the pair as this walker's mark —
    /// "I hold every row of this scope at or below that seq" — and
    /// `fauna.account.state.retire` refuses to compact a row any marked
    /// walker with a live grant has not walked past. Unauthenticated, like
    /// `AccountStatePutRequest::writer_id`: a session of the account that
    /// lies about another walker's mark can only make a stale sibling's
    /// window last longer, which a live member could do by healing it
    /// directly. Absent — an unbound replica, which has no writer to name — leaves no mark and
    /// blocks nothing. Additive 2026-09-16.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walker_id: Option<String>,
    /// **A sealing-generation filter** (the same ruling, clause (3e)): serve
    /// only state-entry rows whose form-v2 cleartext header names this
    /// 32-byte generation id — the reclamation pass's "is any live row still
    /// sealed under G" question, asked of the nest rather than inferred from
    /// a replica's own relay plane. Absent keeps the serve byte-for-byte.
    /// Additive 2026-09-16.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed_under: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncChange {
    /// Monotonic per-actor / per-folder sequence number.
    pub seq: i64,
    /// Hex BLAKE3 of the file path.
    pub path_hash: String,
    /// Hex BLAKE3 manifest hash; `None` for deletes.
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    /// `"create"` | `"modify"` | `"delete"` (lowercase — the sync-engine apply
    /// match is case-sensitive; see `fauna-sync-engine::engine`).
    pub change_type: String,
    pub created_at: i64,
    /// Plaintext path (stored alongside `path_hash` for newer changes).
    pub path: Option<String>,
    /// Hex device id that recorded the change.
    pub device_id: Option<String>,
    /// The M2 content-key **generation** the chunks of this version were sealed
    /// under (`FolderContentKeys::current_version` at upload), for a cross-user
    /// shared folder. `None` for owner-only sets and deletes.
    /// The reader tries every `keys_for(version)` candidate and fails closed if it lacks that
    /// generation — `mls-group-key-material.md` § M2 content-key mechanism.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// Hex thumbnail-blob hash the uploader recorded for this file (the
    /// `UploadSidecar.thumbnail_hash` pointer fetched via the nest `?thumb=1`
    /// routing). `None` for deletes, non-media files, and changes recorded before
    /// a producer supplies one. The nest stores it opaque and echoes it back so
    /// `fauna.media.list` can surface it as `MediaItem.thumbnail_hash`
    /// (`docs/goal/ui/media.md` § State & data shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_hash: Option<String>,
    /// Hex actor id of the authenticated connection actor that recorded this
    /// change — stamped **by the nest**, never client-asserted (multi-writer
    /// Phase 1 attribution, `file-sync.md` § Multi-writer shared sets; the
    /// cryptographic upgrade is the deferred per-manifest MLS binding, KMH
    /// § M2). Additive; absent on the public-audience projection (which strips it).
    /// On a row with no recorded author the set owner is the recorder, so the
    /// projection stamps those too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_actor_id: Option<String>,
    /// The recorded `path`, sealed under the root that already seals this set's
    /// chunks — a `fauna_core::path_crypto::SealedLabel` envelope, canonical
    /// dag-cbor (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
    /// Opened with `path_crypto::open` under the generation the envelope names;
    /// the nest stores it opaque and can never read it.
    ///
    /// `None` from a writer holding no seal root (an unbound, keyless engine) —
    /// which the nest accepts only on a plane that rests plaintext paths (a
    /// `public`-audience folder, whose `path` then rides beside it) and refuses
    /// everywhere else (`path_seal_required`) — and on the public projection,
    /// which withholds the envelope from a reader outside the label audience.
    /// So a row reaches no reader with neither `path` nor `path_sealed`; an
    /// applier meeting one anyway refuses it (`ChangePathRefusal::NoSeal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// The causal watermark (ruled 2026-08-02; binding prose
    /// `conflicts.md` § Concurrent resolution & ancestor freshness): the
    /// set's change-log seq through which the
    /// WRITER had incorporated every row when this content was produced — "my
    /// resolution of everything ≤ this seq". A LOWER BOUND by contract
    /// (stamped from the writer's own catch-up anchor; over-claiming recreates
    /// the lost-edit defect this field exists to close). `None` = unknown
    /// causality (daemon, WebDAV and peer-sync writers send none) —
    /// readers then fall back to the `local == base` heuristic.
    /// Additive; the nest stores and echoes it opaquely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_through: Option<i64>,
    /// True when this row **carries no novel content beyond
    /// [`Self::derived_through`]** (meaning widened by the 2026-08-03 gap
    /// ruling): a pure RESOLUTION (auto-resolve merge result /
    /// resolution-winner propagation) or a PROVEN REISSUE (a re-upload of
    /// bytes whose change record provably landed — the re-seal migration).
    /// Absent/false for a fresh user edit AND for the lost-ack retry, whose
    /// record may never have landed. A stale resolution (`derived_through`
    /// below the reader's path EDIT-frontier) misses novel content the
    /// reader reflects and is skipped; a COVERING one (edit-frontier ≤ w) is
    /// adopted by nest-log order; a stale fresh edit carries novel content
    /// and merges. Same ruling + additivity as [`Self::derived_through`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolution: Option<bool>,
    /// True on a RETENTION row (loser-row ruling, 2026-08-05 — `conflicts.md`
    /// § Concurrent resolution & ancestor freshness): the resolved report's
    /// transactional loser-retention vehicle, minted **by the nest** (never
    /// client-recorded) so the losing candidate stays listable and GC-pinned.
    /// Receivers that understand it account its seq into the path frontier
    /// and do nothing else — never fetch, apply, merge, or adopt it, and
    /// never advance the edit-frontier (`fauna-sync-engine::causal`,
    /// `IncomingVerdict::RetentionRow`). Old receivers ignore it and degrade
    /// to today's behaviour (they merge it — the same-anchor duplication /
    /// loser-latest-wins lost-edit defect the marker closes). Additive; the
    /// nest stores and echoes it like the other causal-stamp fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_retention: Option<bool>,
    /// **The explicit item-class discriminator** (W2.3,
    /// `account-sync-plane.md` § Feeds and cursors → *Feed row + wire
    /// evolution*;
    /// [`crate::account_state::ItemClass::as_wire`]). `None` on every ordinary
    /// file row — the reader's fallback is the infer-from-shape routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_class: Option<String>,
    /// Hex 32-byte **authoring writer** id, for a row the nest relays from a
    /// device writer. `None` means the nest itself is the writer — the shipped
    /// case, whose log coordinate is [`Self::seq`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_writer: Option<String>,
    /// The authoring writer's own log sequence for this row — the coordinate the
    /// reader accounts into that writer's frontier slot. `None` alongside
    /// [`Self::origin_writer`] being `None`: the nest-writer slot accounts
    /// [`Self::seq`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_seq: Option<i64>,
    /// The sealed class-2 entry, served **inline** on the feed row
    /// (*"inline for state entries, by-CID
    /// for blocks"*). The frozen T14 envelope, echoed byte-for-byte; the nest
    /// holds no key that opens it. `None` on every non-`state-entry` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<ByteBuf>,
    /// The writer's Ed25519 signature over this record's `SignedChange`
    /// statement (`crate::sync_writer_sig`; `mls-group-key-material.md` § M2 →
    /// *Writer-signed change records* (2)). 64 bytes. Required from birth on
    /// every row outside the class exemptions; the nest stores it verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ByteBuf>,
    /// The key the signature verifies under — the store principal's writer key
    /// (delegated, chained through a `SyncWrite` `DeviceAuthorization`) or the
    /// actor id itself (direct). 32 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncChangesListReply {
    pub changes: Vec<SyncChange>,
    /// **Cross-nest relay only** — the caller's current access grant on the set,
    /// stamped by the set's HOME nest (which resolved membership for the read
    /// gate anyway) and threaded back through this nest's relay. Mirrors
    /// [`crate::folders::ContentKeyGetReply::caller_access`]: same refresh
    /// contract, same `None` semantics (asserts nothing — never a revocation),
    /// same **advisory-for-UI-only, never an authz input** rule.
    /// `docs/goal/architecture/federation.md` § Cross-nest → *Recipient-side
    /// access discovery*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// **Cross-nest relay only** — the folder's residency as the set's HOME nest
    /// stamps it, beside `caller_access`; the same stamp
    /// [`crate::folders::ContentKeyGetReply::residency`] carries. `None` = not
    /// stated, never *full*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// **The completeness echo** (`account-sync-plane.md` § Feeds and cursors
    /// → *Compaction is a serve-order watermark*): *this page is complete
    /// through this `seq` — every live row at or below it that your request
    /// did not gate is in this page or an earlier one.* The last served row's
    /// `seq` on a frame-truncated page; the scope's log tip on a page that left
    /// nothing behind, an empty page included — which is how a converged walk
    /// banks the whole log.
    ///
    /// Also the honour signal: an arm that does not honour
    /// [`SyncChangesListRequest::held_through_seq`] echoes nothing, and a
    /// requester that never sees this never projects its frontier. Stamped
    /// only by the nest's class-2 arm; `None` on every other arm and on every
    /// store-served leg's reply. Additive 2026-09-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_through_seq: Option<i64>,
    /// **The retention gate's watermark** (`account-data-taxonomy.md` § The
    /// generation machinery → *Fleet-scope reclamation*, clause (1) → *the
    /// gate's watermark*): the LOWEST mark among this scope's counted walkers
    /// — every walker marked on the scope ([`SyncChangesListRequest::walker_id`])
    /// whose key holds a live, non-tombstoned grant on the account. A
    /// `fauna.account.state.retire` of a live row with `seq` at or below it
    /// passes the gate right now; one above it is refused `not_yet_stable`
    /// until the lagging walker walks past the row or loses its grant. The
    /// reclamation pass reads it beside each relay row's own `seq` and
    /// withholds the retires the gate would refuse, so a fleet with a dead
    /// walker costs no request per row per pass. `None` when no counted mark
    /// exists (nothing is withheld — the gate refuses nothing either), on a
    /// custody-session pull of another owner's feed, and on every other arm. Stamped after this request's own mark is recorded,
    /// so a converged walk's last page never reads its own lag as the
    /// fleet's. Additive 2026-09-22.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirable_through_seq: Option<i64>,
    /// **The serving nest's replica id** (`account-sync-plane.md` § The bind
    /// leg, ruling 2): 16 random bytes the nest minted once with its database
    /// — a box rebuilt over an empty database under the same identity has a
    /// new one, a rotated box keeps its own. What a device remembers about
    /// "the nest" (the banked [`Self::complete_through_seq`] watermark first)
    /// is keyed by it and void on any other. Stamped only by the nest's
    /// class-2 arm; `None` on every other arm, and on every store-served leg's
    /// reply — which a device reads as "unknown
    /// replica", never as the one it banked. Additive 2026-09-30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_id: Option<ByteBuf>,
    /// **The signer-cert side table** (writer-signed change records (2)): one
    /// embed-as-bytes `DeviceAuthorization` per distinct delegated signer among
    /// [`Self::changes`] — the cert travels by reference within a home nest,
    /// and a reader feeds this into its `sync_writer_sig::SignerCertCache`. A
    /// direct signer needs none. Empty is omitted on the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_certs: Vec<fauna_core::encoding::EmbedAsBytes>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.changes.record (≡ POST /api/v1/sync/changes) ──────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncChangeRecordRequest {
    /// Folder name (must be owned by the connection actor).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hex device id recording the change (must have the `write` capability).
    pub device_id: String,
    /// Plaintext file path; the nest hashes it to `path_hash`.
    pub path: String,
    /// Hex manifest hash; `None` for deletes.
    #[serde(default)]
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    /// `"create"` | `"modify"` | `"delete"` (lowercase — the sync-engine apply
    /// match is case-sensitive; see `fauna-sync-engine::engine`).
    pub change_type: String,
    /// The M2 content-key **generation** these chunks were sealed under (see
    /// [`SyncChange::content_key_version`]). `None` for owner-only sets and deletes;
    /// the nest stores it opaque and echoes it back on list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// Hex thumbnail-blob hash the uploader recorded for this file (see
    /// [`SyncChange::thumbnail_hash`]). `None` until a producer supplies one; the
    /// nest stores it opaque and surfaces it via `fauna.media.list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_hash: Option<String>,
    /// Cross-nest relay (additive, Phase 3 — `federation.md` § Cross-nest shared
    /// folders + channel append): when set (with `channel_id`), a **writer**
    /// member's own nest relays the record to the set's home nest via
    /// `fauna.federation.folder.changes.record` instead of recording locally.
    /// Permitted as an additive field by the ratified wire rule because the
    /// local path fails visibly (`not_found` — the recorder's nest holds no
    /// row for a foreign set), never silently misdirects a write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_url: Option<String>,
    /// The foreign set's derived 32-byte `ChannelId` (hex) — the federated write
    /// kind is channel-keyed (a foreign set's `name` only resolves on its home
    /// nest). Required with `nest_url`; ignored without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// `path`, sealed client-side under the set's chunk-seal root — see
    /// [`SyncChange::path_sealed`] for the envelope and the `None` semantics.
    /// The client computes this in the one engine funnel
    /// (`SyncEngine::record_change`); the nest stores the bytes verbatim and
    /// never holds a key that opens them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Hash-first addressing (S5b) — see `SyncChangesListRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// The causal watermark — see [`SyncChange::derived_through`] for the full
    /// contract. Stamped by the recording client from its own catch-up anchor
    /// at content-capture time; the nest stores it verbatim and echoes it on
    /// list + the real-time forward. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_through: Option<i64>,
    /// Resolution marker — see [`SyncChange::is_resolution`]. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolution: Option<bool>,
    /// The writer's Ed25519 signature over this record's `SignedChange`
    /// statement (`crate::sync_writer_sig`; `mls-group-key-material.md` § M2 →
    /// *Writer-signed change records* (2)). 64 bytes. Required from birth on
    /// every row outside the class exemptions; the nest stores it verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ByteBuf>,
    /// The key the signature verifies under — the store principal's writer key
    /// (delegated, chained through a `SyncWrite` `DeviceAuthorization`) or the
    /// actor id itself (direct). 32 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<ByteBuf>,
    /// The writer's cert, **inline** — carried across a trust boundary (the
    /// cross-nest relay: the home nest holds no grant row for a federated
    /// writer). Absent for a direct signer and within the writer's own nest,
    /// which resolves the cert by reference over the actor's registered grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_cert: Option<fauna_core::encoding::EmbedAsBytes>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncChangeRecordReply {
    /// The monotonic sequence number assigned to this change.
    pub seq: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.changes.supersede ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncChangesSupersedeRequest {
    /// Folder name (must be owned by the connection actor — the owner is the
    /// sole writer of a shared set, and supersede is a write).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hex device id issuing the supersede (must have the `write` capability,
    /// the same gate as `changes.record`).
    pub device_id: String,
    /// Plaintext file path; the nest hashes it to `path_hash`.
    pub path: String,
    /// Hex manifest hash of the path's current head — the copy the caller has
    /// **verified retrievable + decryptable end-to-end** (fetched over the real
    /// route, decrypted, whole-file hash checked). The nest marks the path's
    /// older manifest rows superseded **only if** this equals the live head, so
    /// the mark is bound to exactly the verified copy and the head itself is
    /// never markable (`mls-group-key-material.md` § M2 *Pre-bind re-seal
    /// migration* bullet B; `webdav-server.md` § Architectural rules "deletes
    /// nothing until the re-sealed copy is verified").
    pub manifest_hash: String,
    /// Hash-first addressing (S5b) — see `SyncChangesListRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncChangesSupersedeReply {
    /// Number of change rows newly marked superseded (0 on an idempotent
    /// re-run).
    pub superseded: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.backup_status (≡ GET /api/v1/sync/backup-status) ──────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncBackupStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackupStatusEntry {
    pub name: String,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — its
    /// address and the convergent salt [`Self::name_sealed`] opens under, so the
    /// row stays renderable once the plaintext [`Self::name`] scrubs
    /// (`path-sealing.md` § the set-name plane). `None` for a reserved `__` set,
    /// which has no hash. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// [`Self::name`], **sealed** — the row's `folders.name_sealed`, forwarded
    /// verbatim. Rendered through `fauna_core::label_custody::render_set_name`
    /// with custody resolved by [`Self::name_hash`]. `None` on a reserved `__`
    /// or public-audience set and on a row no keyed writer stamped. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    /// Timestamp of the most recent change in the set; `None` if no changes.
    pub last_change_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncBackupStatusReply {
    pub folders: Vec<BackupStatusEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.status (≡ GET /api/v1/sync/status) ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncStatusRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hash-first addressing (S5b) — see `SyncChangesListRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncStatusReply {
    pub folder: String,
    pub source_online: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.files (≡ GET /api/v1/sync/files) ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncFilesRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hash-first addressing (S5b) — see `SyncChangesListRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncFile {
    pub path: String,
    /// Hex BLAKE3 manifest hash (reassemble via the `/manifests` byte routes).
    pub manifest_hash: String,
    pub size_bytes: i64,
    pub updated_at: i64,
    /// The file's `path`, sealed — see [`SyncChange::path_sealed`]. Carried on
    /// the listing so a sealed-first renderer never needs the plaintext column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// The file's `path_hash` (`fauna_core::sync::path_hash`) — the convergent
    /// salt [`Self::path_sealed`] opens under. The pair ships together or not
    /// at all: a seal whose salt is missing is unrenderable the moment the
    /// plaintext `path` scrubs (the third instance of this class, after
    /// `MediaItem::path_hash` and `WebdavFile::path_hash`). Every caller who
    /// reaches `fauna.sync.files` is already the label audience — `readable_folder`
    /// admits only the owner or a group roster member, never an admin-discovery-only
    /// reader — so unlike `MediaItem` this field needs no separate per-reader gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncFilesReply {
    pub files: Vec<SyncFile>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.devices.list (≡ GET /api/v1/sync/devices) ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDevicesListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeviceFolderRole {
    pub name: String,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — its
    /// address and the convergent salt [`Self::name_sealed`] opens under, so the
    /// row stays renderable once the plaintext [`Self::name`] scrubs
    /// (`path-sealing.md` § the set-name plane). `None` for a reserved `__` set,
    /// which has no hash. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// [`Self::name`], **sealed** — the row's `folders.name_sealed`, forwarded
    /// verbatim. Rendered through `fauna_core::label_custody::render_set_name`
    /// with custody resolved by [`Self::name_hash`]. `None` on a reserved `__`
    /// or public-audience set and on a row no keyed writer stamped. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    /// This device's place in the folder (folders re-model § Places).
    pub flags: crate::folders::PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDevice {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    /// [`Self::label`], **sealed** — the nest's `sync_devices.label_sealed`
    /// column, forwarded verbatim (the nest holds no key). Rendered through the
    /// one seam `fauna_core::label_custody::render_device_label`, under
    /// **owner-only** custody: a device label has no folder to resolve custody
    /// for.
    ///
    /// The salt is [`Self::device_id`] itself, already on this row — which is
    /// why the device plane, unlike the path and set-name planes, needs no hash
    /// companion to stay renderable once the plaintext scrubs. `None` on the
    /// machine-authored labels `file-sync.md` § Sealed names & paths declares
    /// non-sealing, and on rows a keyless writer last registered.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_sealed: Option<ByteBuf>,
    /// Comma-separated capabilities (e.g. `"read,write"`).
    pub capabilities: String,
    pub registered_at: i64,
    pub last_seen_at: i64,
    pub online: bool,
    /// Hex-encoded 32-byte device PRINCIPAL the enrollment ceremony granted
    /// on this row (`sync_devices.auth_device_key` — the T10 writer key's
    /// public, the identity the R14 (account-data-plane.md § The ratified decisions) generation plane's wraps target; a public
    /// key, never key material). `None` where no principal is enrolled (before enrollment, or after the
    /// sign-out revoke clears the grant columns) — consumers deriving key-reach posture treat that as
    /// "unknown", never "keyless" (`ui/devices.md` § Custody facet piece 1's
    /// fail-safe: no derived fact, no badge). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    pub folders: Vec<DeviceFolderRole>,
    /// This device is the ward's guardian-enrolled one (`family-safety.md`
    /// § Full visibility). Additive (`#[serde(default)]` — an absent key
    /// decodes as `false`, which is exactly "no device is marked"). The
    /// ward's own list renders it: the pattern is transparent by construction,
    /// so the child always sees which device their guardian enrolled. Always
    /// `false` for an unsupervised account — the mark only means something
    /// while a link exists to enforce it.
    #[serde(default)]
    pub guardian_marked: bool,
    /// The device's own last report of whether it runs its peer listeners
    /// (`docs/goal/behavior/p2p.md` § Per-device participation): `None` =
    /// never reported (a device whose
    /// first pass has not run) — a sibling's card paints it as unknown, never
    /// as off. The device itself never reads this back: its own state is
    /// device-local by ruling. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_participation: Option<bool>,
    /// A pending brake: another of the account's devices asked this one to
    /// turn its peer listeners off, and the device has not folded it yet
    /// (it does so at its next full pass, reporting `off`). Additive — a
    /// status without the field decodes `false`, which is exactly "nothing pending".
    #[serde(default)]
    pub p2p_off_requested: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDevicesListReply {
    pub devices: Vec<SyncDevice>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.devices.delete (≡ DELETE /api/v1/sync/devices/{id}) ───────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDeviceDeleteRequest {
    /// Hex device id to unregister.
    pub device_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDeviceDeleteReply {
    pub deleted: bool,
    /// Number of folder memberships the device was removed from.
    pub folders_removed_from: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.devices.p2p_participation.set ─────────────────────────────────

/// The one kind behind the devices page's `device-p2p-participation-toggle`
/// (`docs/goal/behavior/p2p.md` § Per-device participation). Two arms, the
/// `fauna.sync.device_grant.revoke` shape: the **owner arm** (no proof of
/// possession — any session of the account) may only ask a device to turn
/// its peer listeners OFF, which raises the row's pending brake; asking for
/// `true` is refused. The **self arm** (the proof-of-possession triple,
/// signed by the row's own principal over
/// [`crate::auth::device_p2p_participation_signed_message`]) is the device
/// reporting its own state, and only an `off` report clears the brake.
pub const KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET: &str =
    "fauna.sync.devices.p2p_participation.set";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDeviceP2pParticipationSetRequest {
    /// Hex-encoded 32-byte device id of the row.
    pub device_id: String,
    /// Owner arm: must be `false` (a request to turn off). Self arm: the
    /// device's own state.
    pub participating: bool,
    /// Self arm: milliseconds since epoch, within the `device_handshake`
    /// drift window. `None` ⇒ the owner arm alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    /// Self arm: hex nonce uniquifying the deterministic signature for the
    /// replay guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Self arm: hex Ed25519 signature by the row's principal over
    /// [`crate::auth::device_p2p_participation_signed_message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncDeviceP2pParticipationSetReply {
    /// The row's reported state after the call ([`SyncDevice::p2p_participation`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participating: Option<bool>,
    /// Whether a brake is pending after the call ([`SyncDevice::p2p_off_requested`]).
    #[serde(default)]
    pub off_requested: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.serve.announce (relay serving, ruled 2026-10-01) ──────────────

/// Announce the folders this connection's process serves relay reads for
/// (`docs/goal/behavior/file-sync.md` § Relay serving, step (1)). Sent on the
/// per-actor WS-RPC connection by a process hosting resident engines; the nest
/// keeps the admitted set **on that connection** — replaced whole by the next
/// announce, gone when the connection closes or is revoked — and asks it for a
/// chunk it does not hold with the [`crate::push_events::SyncChunkWantedPayload`]
/// push. An empty `folders` withdraws every earlier announce.
pub const KIND_SYNC_SERVE_ANNOUNCE: &str = "fauna.sync.serve.announce";

/// The most folders one announce may name. A connection's announced set is
/// bounded in count (`file-sync.md` § Relay serving, step (1)); an announce
/// naming more — its `folders` and `foreign` entries together — is refused
/// whole and replaces nothing.
pub const SERVE_ANNOUNCE_MAX_FOLDERS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncServeAnnounceRequest {
    /// Hex-encoded 32-byte sync device id — must name one of the connection
    /// actor's registered devices (`fauna.sync.register`).
    pub device_id: String,
    /// The folders this process runs an engine for and holds bodies of, each a
    /// `FolderRef` wire string (`local:<folders.id>`,
    /// `fauna_core::folder_keys::FolderRef::to_wire`). A ref the nest cannot
    /// admit — unparseable, foreign, absent, or a folder the actor neither
    /// owns nor is a roster member of — is left out of the reply's
    /// [`SyncServeAnnounceReply::admitted`], never an error.
    pub folders: Vec<String>,
    /// The folders homed on **another** nest this process serves — a
    /// cross-nest writer's seat (`file-sync.md` § Relay serving → *A member on
    /// another nest*, step (2)). Each names the folder by its foreign ref and
    /// its home nest; this nest forwards the announce there, and an entry the
    /// home nest refuses — or a home nest that predates the kind — is left
    /// out of [`SyncServeAnnounceReply::admitted`], never an error. Additive:
    /// a nest that predates the field ignores it, and admits none of it.
    /// Counted with `folders` against [`SERVE_ANNOUNCE_MAX_FOLDERS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<SyncServeForeignEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One foreign folder of a [`SyncServeAnnounceRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncServeForeignEntry {
    /// The folder's `FolderRef` wire string — `foreign:<64-hex channel id>`.
    pub folder: String,
    /// The folder's home nest — the base URL its custody record holds
    /// (`ForeignFolder::home_nest_url`).
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncServeAnnounceReply {
    /// The refs of [`SyncServeAnnounceRequest::folders`] the nest admitted, in
    /// request order, each once, then the admitted refs of
    /// [`SyncServeAnnounceRequest::foreign`] the same way — what the
    /// connection now serves.
    pub admitted: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;

    #[test]
    fn serve_announce_round_trips() {
        assert_round_trips(&SyncServeAnnounceRequest {
            device_id: "ab".repeat(32),
            folders: vec!["local:7".into(), "local:9".into()],
            foreign: vec![SyncServeForeignEntry {
                folder: format!("foreign:{}", "cd".repeat(32)),
                nest_url: "https://home.example".into(),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        // An announce with no foreign entry keeps the pre-field shape.
        assert_round_trips(&SyncServeAnnounceRequest {
            device_id: "ab".repeat(32),
            folders: vec!["local:7".into()],
            ..Default::default()
        });
        assert_round_trips(&SyncServeAnnounceReply {
            admitted: vec!["local:7".into()],
            extra: BTreeMap::new(),
        });
    }
}
