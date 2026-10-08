//! Folder management WS-RPC payload types — `fauna.folders.*` (CRUD +
//! members + devices + schedule + lease + cross-user `share`) and
//! `fauna.sync.conflicts.*` (list/report/resolve). Mostly a
//! behavior-preserving transport migration of the
//! bearer-authed user folder routes (`user_folder_routes` and
//! `lease_routes`) — Track B14 of
//! the WS-RPC-everywhere migration (tracked internally). These kinds ride the bearer
//! connection (`GET /api/v1/ws/{actor_id}`); the connection authenticates the
//! actor, so every kind that the HTTP twin scoped on `bearer.0.0` scopes on the
//! connection `actor_id`.
//!
//! Wire-shape decisions (see the TODO § Wire-type design decisions):
//!
//! - **`retention_policy: Option<String>`** rides as the opaque JSON string the
//!   nest stores in the `folders.retention_policy` TEXT column. The nest
//!   treats it opaquely (only `backup/scheduler.rs` parses it); the typed
//!   `RetentionPolicy {max_snapshots,max_age_days}` lives in
//!   `libs/fauna-folders-machine` (a higher layer this crate can't depend on),
//!   so the client serializes before send / parses on receipt — the
//!   calendars-ICS-as-`String` precedent.
//! - **`include_paths`/`exclude_paths: Option<Vec<String>>`** ride as typed
//!   string arrays (the HTTP layer stored them as JSON string arrays).
//! - **update** uses plain `Option<T>` per field (`Some` = set, `None` = don't
//!   change). The twin's `Option<Option<&str>>` "clear" arm is unreachable via
//!   HTTP (serde maps both absent and `null` to `None`).
//! - **device ids ride as hex `String`** (matching the twins' `hex::encode` /
//!   `parse_hash`); folder / conflict ids as `i64`.
//!
//! No floats anywhere — every field is `String` / `i64` / `bool` / arrays
//! thereof. Kind registry: `kind.rs::register_folders_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

// ── WS-RPC kind constants ────────────────────────────────────────────────────
//
// One source of truth for the nest router (`register_folders_handlers`), the
// registration table (`kind.rs::register_folders_kinds`), and every client
// dispatch site — see the per-kind `//! ── fauna.folders.* ──` sections below
// for behavior. `fauna.sync.conflicts.*` (the module's other kind family) is
// out of scope here — a separate namespace, not folder management.

pub const KIND_FOLDERS_CREATE: &str = "fauna.folders.create";
pub const KIND_FOLDERS_LIST: &str = "fauna.folders.list";
pub const KIND_FOLDERS_UPDATE: &str = "fauna.folders.update";
pub const KIND_FOLDERS_DELETE: &str = "fauna.folders.delete";
pub const KIND_FOLDERS_DEVICES: &str = "fauna.folders.devices";
pub const KIND_FOLDERS_MEMBERS_LIST: &str = "fauna.folders.members.list";
pub const KIND_FOLDERS_MEMBERS_LIST_ACTORS: &str = "fauna.folders.members.list_actors";
pub const KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE: &str =
    "fauna.folders.members.list_actors_remote";
pub const KIND_FOLDERS_MEMBERS_SET_ACCESS: &str = "fauna.folders.members.set_access";
pub const KIND_FOLDERS_MEMBERS_REMOVE: &str = "fauna.folders.members.remove";
pub const KIND_FOLDERS_MEMBERS_EVICT: &str = "fauna.folders.members.evict";
pub const KIND_FOLDERS_PLACES_SET: &str = "fauna.folders.places.set";
pub const KIND_FOLDERS_LEAVE: &str = "fauna.folders.leave";
pub const KIND_FOLDERS_SHARE: &str = "fauna.folders.share";
pub const KIND_FOLDERS_CONTENT_KEY_PUT: &str = "fauna.folders.content_key.put";
pub const KIND_FOLDERS_CONTENT_KEY_GET: &str = "fauna.folders.content_key.get";
pub const KIND_FOLDERS_LEASE_ACQUIRE: &str = "fauna.folders.lease.acquire";
pub const KIND_FOLDERS_LEASE_RELEASE: &str = "fauna.folders.lease.release";
pub const KIND_FOLDERS_PUBLIC_FETCH: &str = "fauna.folders.public.fetch";
pub const KIND_FOLDERS_SET_WEB_PAYWALL: &str = "fauna.folders.set_web_paywall";
pub const KIND_FOLDERS_WRITE_TOKEN_GET: &str = "fauna.folders.write_token.get";
pub const KIND_FOLDERS_READ_TOKEN_GET: &str = "fauna.folders.read_token.get";
pub const KIND_FOLDERS_SERVED_ROWS_ADOPT: &str = "fauna.folders.served_rows.adopt";
/// A third-party principal's write-only ingress into one folder — served on a
/// principal session only (`file-sync.md` § Third-party deposit ingress).
pub const KIND_FOLDERS_DEPOSIT: &str = "fauna.folders.deposit";
/// The owner's read of one folder's parked deposits — adoption's input
/// (`file-sync.md` § Third-party deposit ingress).
pub const KIND_FOLDERS_DEPOSITS_LIST: &str = "fauna.folders.deposits.list";
/// The owner's retire of one parked deposit once its adopted change row is
/// durable (`file-sync.md` § Third-party deposit ingress).
pub const KIND_FOLDERS_DEPOSITS_RETIRE: &str = "fauna.folders.deposits.retire";

// ── shared sub-structs ───────────────────────────────────────────────────────

/// **The nest place's policy** for one folder — the folders re-model's phase 2
/// § Places applied to the one place every folder always has (`folders.md`
/// § Target re-model; behavior owner `backup-restore.md` § 8 *The nest place's
/// snapshot policy*).
///
/// The nest place is **always present and always live** — it holds the head, and
/// that is not a knob: the design's metadata-only residency keeps the place live
/// and varies a separate *content* property instead (phase 5). So there is no
/// `live` field here, and adding one would mint a column that can only ever say
/// `true`.
///
/// **Sent whole, applied whole.** A `fauna.folders.update` carrying this struct
/// replaces the folder's entire nest-place policy, so each `None` inside means
/// *unset — use the nest-wide default*, never "leave whatever was there". That is
/// what lets the editor express all three states of each knob (on / off / default)
/// without an `Option<Option<_>>`, which this wire cannot round-trip (dag-cbor).
/// The set-vs-unchanged axis lives on the **outer** `Option` at the request field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NestPlacePolicy {
    /// Whether the nest place keeps snapshots of this folder. `None` = unset ⇒
    /// the nest-wide behavior (snapshot), which is where every folder rests until
    /// its owner chooses.
    ///
    /// ⚠ `Some(true)` is a *preference*, not a guarantee — the nest's structural
    /// refusals still apply on top (a reserved `__` destination set is never
    /// snapshotted, because a snapshot pin would defeat the custodian's
    /// latest-per-path reclamation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<bool>,
    /// Seconds of quiet before the nest place cuts a snapshot. `None` = unset ⇒
    /// the nest-wide scheduler cadence. In `0..=`[`Self::MAX_QUIET_SECS`]; the
    /// nest refuses a value outside it rather than normalizing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_secs: Option<i64>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl NestPlacePolicy {
    /// The longest quiet period an owner may choose: seven days. A quiet period
    /// is how long a folder must sit unchanged before the nest cuts its
    /// snapshot, so a week already means "only after the folder has gone
    /// still"; anything longer stops being a cadence and only lets one owner's
    /// value reach the nest-wide scheduler's arithmetic. The nest refuses a
    /// larger value (`fauna.folders.bad_request`) exactly as it refuses a
    /// negative one, and the shared editor reads one as unset
    /// (`docs/goal/behavior/backup-restore.md` § 8b).
    pub const MAX_QUIET_SECS: i64 = 7 * 24 * 60 * 60;

    /// True when no knob is set — the resting state of every folder whose owner
    /// has never touched the policy, and the case the projections omit entirely
    /// so an unset folder's wire bytes are unchanged from before this field
    /// existed.
    pub fn is_unset(&self) -> bool {
        self.snapshots.is_none() && self.quiet_secs.is_none() && self.extra.is_empty()
    }
}

/// **Version-retention bounds** for one folder — the per-set SIBLING policy of
/// `retention_policy`, bounding *version history* on the metered sync plane
/// (`file-versions.md` § Retention, ratified 2026-08-17; the § 8b fourth
/// per-place knob). Never re-mapped onto the armed snapshot column — § 8
/// forbids the silent re-map.
///
/// This struct is both the wire shape and the canonical JSON resting in the
/// `folders.version_retention` TEXT column (the same JSON discipline as its
/// snapshot sibling). Semantics mirror § 8's armed bounds engine exactly: the
/// two bounds **intersect** (a version survives only if it is among the newest
/// `max_versions_per_path` listable versions of its path AND younger than
/// `max_age_days`); a `0` in a bound means *that bound is unset*; a policy
/// binding nothing is not a policy (the nest rests it as `NULL` — § 8b's
/// three-state rule: `NULL` is the honest resting value).
///
/// **Sent whole, applied whole** on `fauna.folders.update` (the
/// [`NestPlacePolicy`] rule): `Some(policy)` replaces the folder's whole
/// version-retention policy; clearing means sending the binds-nothing policy,
/// never omitting the field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VersionRetention {
    /// Keep at most this many listable versions per path. `0` = unset.
    #[serde(default)]
    pub max_versions_per_path: u32,
    /// Keep only versions younger than this many days. `0` = unset.
    #[serde(default)]
    pub max_age_days: u32,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl VersionRetention {
    /// True when neither bound binds — the binds-nothing policy, which the nest
    /// rests as `NULL` (keep everything) and the projections omit entirely.
    pub fn is_unset(&self) -> bool {
        self.max_versions_per_path == 0 && self.max_age_days == 0
    }
}

/// The three [`FolderSummary::audience`] wire tokens (`folders.md` § Target
/// re-model, phase 4 owns what each one means). They are named here, beside the
/// field, so the readers below and every consumer spell them once — a
/// `String`-typed wire field is forward-compat machinery, not a licence for each
/// reader to re-type the token it compares against.
pub const AUDIENCE_PRIVATE: &str = "private";
/// See [`AUDIENCE_PRIVATE`].
pub const AUDIENCE_SHARED: &str = "shared";
/// See [`AUDIENCE_PRIVATE`]. The one audience whose content may rest
/// **unsealed** — but the nest's claim of it is never the verdict: a seat asks
/// [`FolderSummary::judge_declassification`], which verifies the owner's
/// attestation (and keeps the WebDAV fail-safe), never this constant alone.
pub const AUDIENCE_PUBLIC: &str = "public";

/// The opt-in [`FolderSummary::residency`] token (`file-sync.md` § Content
/// residency). Absent/empty/unrecognised ⇒ **full** residency — classify with
/// [`FolderSummary::is_metadata_only`], which owns that fail-closed direction.
pub const RESIDENCY_METADATA_ONLY: &str = "metadata_only";
/// The default-residency token. A *projection* spells it as an empty/absent
/// [`FolderSummary::residency`]; it is sent explicitly only on the flip BACK
/// (`FolderUpdateRequest.residency = Some("full")`), which is why the token
/// exists at all rather than being modelled as the absence it usually is.
pub const RESIDENCY_FULL: &str = "full";

/// One folder in a `fauna.folders.list` reply, mirroring the twin's
/// per-row JSON (`list_folders`, with the cached stat columns).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderSummary {
    pub id: i64,
    pub name: String,
    /// Opaque JSON-string retention policy (see module docs); `None` when unset.
    ///
    /// Conceptually the third member of [`Self::nest_place`]'s policy — retention
    /// is what the nest place *keeps* — kept in its own field because it is a
    /// live column with a sealed display twin, and relocating it would be a
    /// contract move with nothing user-visible to show for it.
    pub retention_policy: Option<String>,
    /// The nest place's own policy (folders re-model phase 2 § Places).
    /// Wire-additive and omitted entirely when nothing is set, so a folder whose
    /// owner never touched it carries no `nest_place` key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_place: Option<NestPlacePolicy>,
    /// The per-set version-retention bounds (`file-versions.md` § Retention) —
    /// the fourth per-place knob. Rides both projection arms on the same
    /// audience grading as `retention_policy`. Wire-additive; omitted when
    /// unset (`NULL` column = keep everything).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_retention: Option<VersionRetention>,
    pub cached_snapshot_count: i64,
    pub cached_total_bytes: i64,
    pub cached_last_snapshot_at: Option<i64>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
    /// Hex-encoded raw MLS **group id** binding this set to a cross-user shared
    /// group (shared folders, Slice 1–3). `Some` ⇒ the set is bound; the owner's
    /// sync engine derives `ChannelId::from_group_id(mls_group_id)` to load the M2
    /// per-set content keys from the account-plane folder-key custody (`fauna.state.folder-keys`) and seal/open chunks under the
    /// rotated content key (5d(c)); `None` ⇒ owner-only. Mirrors the nest's
    /// `folders.mls_group_id` BLOB and `SyncConfig.mls_group_id`. Wire-additive:
    /// absent on old messages / unbound sets. Consumed by
    /// `fauna_client_folders::resolve_engine_key_binding`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mls_group_id: Option<String>,
    /// The caller's role for this row in the unified `fauna.folders.list`
    /// reply (B3 member-list-visibility): `Some("owner")` — a set the caller
    /// owns (the owner-scoped rows, the default surface); `Some("member")` — a
    /// set shared *with* the caller that they are a roster member of, present
    /// only when the request set `include_shared_with_me`. Absent on the
    /// default surface (owner-scoped ⇒ treat as `"owner"`).
    ///
    /// ⚠ **SAFETY (the load-bearing invariant of shared folders).** A
    /// `"member"` row is only *rostered* nest-side; the nest **cannot** observe
    /// an MLS group *join* (that is client-side crypto — see `FolderActorMember`
    /// docs). The client MUST render a member row **only if it has actually
    /// joined the group** — i.e. `MlsEngine::has_group(ChannelId::from_group_id(
    /// mls_group_id))` is true — else a stranger's *rostered-but-unaccepted*
    /// knock would appear unbidden in the list (`folders.md` § Sharing: "a
    /// stranger cannot force a set into your list"). A knock must surface **only**
    /// as a `folder-pending-share`, never here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The **caller's** access to a `role == "member"` row: `Some("writer")` when
    /// the caller holds an explicit `writer` grant on the set (multi-writer
    /// Phase 1, `ui/folders.md` § Sharing), else `None` ⇒ **reader** (the
    /// fail-safe default — an absent `folder_member_access` row, or a
    /// `role == "owner"` row). Never overloads `role`
    /// (owner/member) — the reader/writer axis is orthogonal. A **writer** member
    /// may bind local folders and sync read-write; a **reader** may not (a bound
    /// folder whose edits cannot upload would breach `file-sync.md`'s iron rule).
    /// Consumed by the engine's reader-unbindable guard
    /// (`fauna_sync_engine::engine_lifecycle::decide_engine_content_binding`) and
    /// the client folder-binding UI gate. Wire-additive: absent ⇒ reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// Handle of the set's owner — the "Shared by ‹handle›" recipient badge —
    /// populated **only** for a `role == "member"` row (a set shared *with* the
    /// caller); `None` for the caller's own sets or when the owner's handle is
    /// unresolved. B3 member-list-visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<String>,
    /// Hex-encoded owner **ActorId** — the display *fallback* for the recipient
    /// "Shared by ‹…›" badge when [`Self::owner_handle`] is absent (a cross-nest
    /// or handle-unset owner). Populated **only** for a `role == "member"` row
    /// (`None` for the caller's own sets, which show "Shared · N", not a
    /// "Shared by" badge). The client-side transcribe folds handle + this into a
    /// single precomputed `owner_display` (`fauna_core::format::account_display_label`)
    /// so the six apps cannot re-drift on the fallback truncation — mirrors the
    /// track-D `shared_by_display` shape for the pending-share row. Wire-additive:
    /// absent on old messages ⇒ `None` (owner-only degrades to a blank badge, the
    /// pre-enrichment behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_actor_id: Option<String>,
    /// Whether the user has flagged this set to be served over WebDAV — the
    /// per-set exposure gate (`webdav-server.md` § Independent enablement).
    /// `false` = not served (default); `true` = the MDA exposes it read/write to
    /// generic DAV clients. Mirrors the nest's `folders.webdav_enabled` column;
    /// the Settings → Folders `folder-webdav-toggle` reads/writes it. A reserved
    /// `__` set is never served. Wire-additive: absent on old messages ⇒
    /// `false`.
    #[serde(default)]
    pub webdav_enabled: bool,
    /// The folder's **audience** (`folders.md` § Target re-model, phase 4):
    /// `"private"` — owner-key sealed, the unbound default; `"shared"` — bound
    /// to an MLS group, M2 content-key sealed; `"public"` — world-readable by
    /// design, chunks/names/paths rest **unsealed** (`principles.md` § The user
    /// always controls their data, the one deliberate exception). Derived
    /// nest-side: the at-rest `folders.audience` column stores only the owner's
    /// explicit **declassification** (`'public'`), because bound-ness already
    /// rests authoritatively in `mls_group_id` — `public` if declassified, else
    /// `shared` if bound, else `private`. Rides **both** projection arms (it is
    /// the set's identity, like [`Self::mls_group_id`], not an owner-side
    /// control). The nest always projects a token; a reader meeting an empty
    /// one treats it as `"private"` (the most restrictive audience), never
    /// deriving it from `mls_group_id`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub audience: String,
    /// The owner's signature that [`Self::audience`] is `public` — the only
    /// thing that lets a seat write this folder unsealed. The nest stores and
    /// serves it and verifies nothing (it is the adversary the attestation
    /// defends against); read it **only** through
    /// [`Self::judge_declassification`]. Kept by the nest after a flip-back,
    /// inert, so the next mint counts above it. Rides **both** projection arms,
    /// like the audience it vouches for. Wire-additive: absent from a folder no
    /// attesting client has flipped ⇒ sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_attestation: Option<AudienceAttestation>,
    /// The set's **content-key floor**: the owner-stamped current generation
    /// the nest keeps beside the set's envelope (`mls-group-key-material.md`
    /// § M2 content-key mechanism → *Multi-writer*). Rides **both** arms — a
    /// member already holds every generation the envelope carries, so this
    /// discloses nothing new — because every holder that seals must hold
    /// itself to it, and the capability host, whom the nest's owner exemption
    /// does not cover, reads it here at its re-resolve edges
    /// (`on-demand-files.md` § Shared sets on a capability host, decision 2).
    /// `None` = unbound or no floor established. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_floor: Option<u64>,
    /// The folder's **content residency** (phase 5 — `file-sync.md` § Content
    /// residency): `"metadata_only"` = chunk bytes never rest on the nest (the
    /// owner's explicit consent-gated choice; seats skip byte upload, the nest
    /// accept-and-discards, bytes move seat↔seat via transient relay); empty /
    /// absent = **full**, today's behavior. Rides **both** projection arms —
    /// member seats upload bytes too, so members must see it. **Fail-closed to
    /// full**: nothing unparseable may ever stop bytes resting; only an
    /// explicit, parsed `"metadata_only"` may. Wire-additive: absent on old
    /// wire ⇒ full.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub residency: String,
    /// Whether this folder is under **exclusive editing** — one device at a
    /// time may write (`file-sync.md` § Exclusive editing). Rides **both**
    /// projection arms: a member seat writes too, so a member must know to take
    /// the lease, exactly as for [`Self::residency`].
    ///
    /// **Fails OPEN to un-governed.** Absent or unparseable ⇒ `false` ⇒ today's
    /// behavior. Note this is the opposite direction from [`Self::residency`]'s
    /// fail-closed-to-full, and deliberately so: failing the wrong way there
    /// would rest bytes the owner asked us not to rest, while failing the wrong
    /// way here would **freeze a user's own folder against their own writes**
    /// over a field that did not parse. A lease coordinates cooperating devices
    /// of one account and is not a security boundary, so the tie breaks toward
    /// the behavior that keeps the user working. Wire-additive: absent on old
    /// wire ⇒ `false`.
    #[serde(default)]
    pub exclusive_editing: bool,
    /// The folder's **live lease state** — who may write it right now, when
    /// [`Self::exclusive_editing`] is on (`file-sync.md` § Exclusive editing).
    /// `None` = not held.
    ///
    /// **This is how a seat learns a folder is locked, and the ONLY sanctioned
    /// way.** `fauna.folders.lease.acquire` must never be used as a probe: it
    /// *takes* a free lease as a side effect of asking, and it is gated on the
    /// writable-folder resolver, so a **reader** member — the seat that most
    /// needs to know a folder is locked — cannot ask at all. This field rides
    /// the projection every client already polls, so both problems vanish.
    ///
    /// It is also where the **holder's identity** lives, for both arms. The
    /// acquire refusal is a typed, payload-less `conflict` error whose detail
    /// string is free-form prose for a log; no client may parse it for the
    /// holder. A client refused the lease re-reads this projection instead, so
    /// the holder's device id has exactly one owner on the wire.
    /// Wire-additive: absent ⇒ not held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<FolderLeaseState>,
    /// Whether this folder is served as the user's **website** — the per-folder
    /// toggle that replaced `mode == "web"` (folders re-model phase 4;
    /// `web-content-hosting.md` § Content model). `true` ⇒ the nest fans this
    /// folder's recorded changes out to `web_files` *in addition to* the head
    /// row (never instead — `web_files` is not a GC reachability source).
    /// Serving still fails closed unless the content is actually openable at
    /// serve time: `public`-audience plaintext, or paywalled under
    /// [`Self::web_paywall_tier`]'s grant. Owner-side control (member arm
    /// projects `false`, like [`Self::webdav_enabled`]). Wire-additive: absent
    /// on old messages ⇒ `false`.
    #[serde(default)]
    pub website_enabled: bool,
    /// Per-set conflict policy (`"auto"` | `"latest_wins_always"`,
    /// `fauna_core::format::ConflictPolicy` wire strings — file-sync.md
    /// § Conflicts, ratified 2026-07-10). Governs how the syncing device
    /// auto-resolves a detected conflict. Mirrors the nest's
    /// `folders.conflict_policy` column. Wire-additive: absent on old
    /// messages ⇒ treat as `"auto"` (the default; unknown values also degrade
    /// to auto — both arms retain the losing version, so degrading is safe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_policy: Option<String>,
    /// Web-paywall tier (monetization.md § Pillar 2, the folder half):
    /// `None` = not paywalled; `Some(tier)` = this website-enabled folder is paywalled
    /// to the owner's named subscription tier. Mirrors the nest's
    /// `folders.web_paywall_tier` column; written via
    /// `fauna.folders.set_web_paywall`. Wire-additive: absent on old
    /// messages ⇒ not paywalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_paywall_tier: Option<String>,
    /// The set's [`Self::name`], **sealed** — a `fauna_core::path_crypto::SealedLabel`
    /// over the user-chosen name, mirroring the nest's `folders.name_sealed`
    /// column (`file-sync.md` § Sealed names & paths, the *paths-are-content*
    /// ruling). Rendered through the one seam
    /// `fauna_core::label_custody::render_set_name`, never opened ad hoc.
    ///
    /// Sealed under the root that already seals the set's chunks, so exactly the
    /// audience that can read the set's bytes can read its name. `None` on a
    /// reserved `__` set (a routing constant, never sealed) and on a row no
    /// keyed writer has stamped yet. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — the
    /// addressing/uniqueness key (`UNIQUE(name_hash, actor_id)`) and the
    /// convergent **salt** [`Self::name_sealed`] opens under.
    ///
    /// **Load-bearing for the render, not just for addressing:** `name_sealed`
    /// is convergent, so its nonce derives from this salt. A reader whose
    /// plaintext [`Self::name`] has been scrubbed at the flip has nothing to
    /// derive it from, so a plane carrying the seal without this hash renders
    /// every row as `Omit`. That hole was shipped and caught twice already — on
    /// `fauna.media.list` (S2b) and `WebdavFile` (S4) — hence the field.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// [`Self::include_paths`], **sealed** — a `fauna_core::path_crypto::SealedLabel`
    /// over the selective-sync list, mirroring the nest's
    /// `folders.include_paths_sealed` column (`file-sync.md` § Sealed names &
    /// paths; `encryption-at-rest.md` § Carve-outs calls this field *"the
    /// sharpest: the owner's absolute local filesystem layout"*). Rendered
    /// through `fauna_core::label_custody::render_include_paths`, never opened
    /// ad hoc.
    ///
    /// ⚠ **Owner-only, unlike [`Self::name_sealed`].** The nest already withholds
    /// the plaintext [`Self::include_paths`] from a `role == "member"` row, and
    /// the seal is withheld with it — a member neither syncs the owner's folders
    /// nor manages their config. It is sealed under the **owner's**
    /// `BackupKey::convergent_chunk_root()`, never a bound set's M2 generation,
    /// so a leaked blob is unopenable by the roster (the funnel takes a
    /// `BackupKey` so this cannot be got wrong at a call site).
    ///
    /// **The salt is [`Self::id`]**, a non-`Option` field on this very row, so —
    /// unlike the path and set-name planes — this pair needs no hash companion
    /// to stay openable once the plaintext scrubs. `None` on a row no keyed
    /// writer has stamped and on a member row. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_paths_sealed: Option<ByteBuf>,
    /// [`Self::exclude_paths`], **sealed** — the [`Self::include_paths_sealed`]
    /// twin under its own field domain tag, with the same owner-only audience,
    /// owner root and [`Self::id`] salt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_paths_sealed: Option<ByteBuf>,
    /// [`Self::retention_policy`], **sealed** (S6-e) — mirroring the nest's
    /// `folders.retention_policy_sealed` column. Rendered through
    /// `fauna_core::label_custody::render_retention_policy`, never opened ad hoc.
    ///
    /// ⚠ **Label-audience, like [`Self::name_sealed`] and *unlike* the two path
    /// lists three fields up.** This row ships on both projection arms because
    /// the nest ships the plaintext [`Self::retention_policy`] to a roster member
    /// unmodified today — so withholding the seal, or sealing it under the owner
    /// root, would blank a member's retention display at the flip. The two
    /// audiences sit adjacent on this struct on purpose: the precedent to copy is
    /// decided by who reads the field, never by which neighbour is nearest.
    ///
    /// **The salt is [`Self::name_hash`]**, not [`Self::id`], because retention
    /// is settable at create while the id is minted at INSERT — so this pair
    /// *does* need the hash companion, and it rides beside it on both arms.
    /// `None` on a row no keyed writer has stamped. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy_sealed: Option<ByteBuf>,
    /// The set's **client-minted 32-byte set nonce** — the binding every
    /// writer-signed change record's statement carries
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records* (2);
    /// `crate::sync_writer_sig`). The nest stores it opaque beside the row as a
    /// convenience it never vouches for: a reader takes the set's nonce from
    /// custody (owner) or the content-key envelope (member), never from this
    /// echo. `None` on a row created without one. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_nonce: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The owner's signed statement that a folder is `public`
/// (`encryption-at-rest.md` § Readable classes → *The declassification is
/// owner-ATTESTED* owns the mechanism). The folder **name** is signed but not
/// carried: a verifier supplies the name it is acting under, so a row cannot
/// lend one folder's attestation to another by lying about its own name.
///
/// `deny_unknown_fields` (transport.md § Schema and forward-compat
/// discipline, rule 4, category 4): every field here is exactly one of the
/// signed statement's components — a flatten catch-all would let an
/// unauthenticated field ride alongside a valid signature. Evolution goes
/// through a new domain-separation tag ([`crate::sig_domain::FOLDER_AUDIENCE_ATTESTATION_V1`]
/// gains a `_V2` sibling), not an added field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AudienceAttestation {
    /// The signer's actor id — 32 raw bytes, which *is* its Ed25519 public key.
    /// Descriptive: a verifier signs-checks against the owner identity **it**
    /// trusts and never reads the owner from this field.
    pub owner: ByteBuf,
    /// The `folders.id` this attests. Descriptive, like [`Self::owner`]: the
    /// verifier checks the signature over the id of the row in its hand.
    pub folder_id: i64,
    /// Monotonic per folder: `max(now_ms, previous + 1)` at mint. A seat refuses
    /// a counter below its [`AttestationMemory::floor`].
    pub counter: u64,
    /// Ed25519 signature over [`audience_attestation_signed_message`].
    pub sig: ByteBuf,
}

/// The exact bytes an [`AudienceAttestation`] signs — the single construction
/// point for [`AudienceAttestation::mint`] and its verifier.
pub fn audience_attestation_signed_message(
    owner: &[u8; 32],
    folder_id: i64,
    name: &str,
    counter: u64,
) -> Vec<u8> {
    crate::sig_domain::domain_separated_length_prefixed(
        crate::sig_domain::FOLDER_AUDIENCE_ATTESTATION_V1,
        &[
            owner,
            &folder_id.to_be_bytes(),
            &counter.to_be_bytes(),
            name.as_bytes(),
        ],
    )
}

impl AudienceAttestation {
    /// Sign "folder `folder_id`, named `name`, is public". **Call this only
    /// from the owner's confirmed declassify gesture** — a caller that mints
    /// for whatever audience its nest reports has rebuilt the defect this type
    /// closes. `previous` is the attestation the nest last served for the
    /// folder, if any; the new counter always exceeds it, so a seat that burned
    /// the old one accepts the new one whatever this device's clock says.
    #[must_use]
    pub fn mint(
        keypair: &fauna_core::identity::ActorKeypair,
        folder_id: i64,
        name: &str,
        now_ms: u64,
        previous: Option<&AudienceAttestation>,
    ) -> Self {
        use ed25519_dalek::Signer as _;
        let owner = keypair.actor_id().0;
        let counter = previous.map_or(now_ms, |p| now_ms.max(p.counter.saturating_add(1)));
        let message = audience_attestation_signed_message(&owner, folder_id, name, counter);
        let sig = keypair.signing_key().sign(&message);
        Self {
            owner: ByteBuf::from(owner.to_vec()),
            folder_id,
            counter,
            sig: ByteBuf::from(sig.to_bytes().to_vec()),
        }
    }

    /// Is this a genuine statement by `trusted_owner` about exactly this folder
    /// id and name? Says nothing about staleness — that is
    /// [`FolderSummary::judge_declassification`]'s half.
    ///
    /// **The message is rebuilt from the VERIFIER's inputs, never from this
    /// struct's own `owner` / `folder_id`** — only `counter` and `sig` are read
    /// off the wire. So a wrong signer or a wrong folder fails as a bad
    /// signature, and there is deliberately no separate "does `self.owner`
    /// match" comparison: it could never decide an outcome (a mutation round
    /// showed dropping it changes nothing), and a check that cannot fail reads
    /// as a guard while being none. The two carried fields are descriptive.
    fn is_genuine(
        &self,
        trusted_owner: &fauna_core::identity::ActorId,
        folder_id: i64,
        name: &str,
    ) -> bool {
        let message =
            audience_attestation_signed_message(&trusted_owner.0, folder_id, name, self.counter);
        fauna_core::identity::verify_detached(&trusted_owner.0, &message, &self.sig)
    }
}

/// What one seat remembers about one folder's attestations, so a nest cannot
/// replay a `public` attestation the owner has since withdrawn. Device-local —
/// the engine seats persist it as a `SyncDb` meta row via [`Self::to_meta`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttestationMemory {
    /// The lowest counter this seat will still arm under.
    pub floor: u64,
    /// The counter the seat is armed under right now, if it is.
    pub armed: Option<u64>,
}

impl AttestationMemory {
    /// The seat read the folder list successfully and the folder did **not**
    /// verify public (or was absent from it): whatever counter it was armed
    /// under is burned. Not for an unreadable list — that changes nothing.
    #[must_use]
    pub fn observe_sealed(self) -> Self {
        Self {
            floor: self
                .armed
                .map_or(self.floor, |c| self.floor.max(c.saturating_add(1))),
            armed: None,
        }
    }

    fn observe_armed(self, counter: u64) -> Self {
        Self {
            floor: self.floor.max(counter),
            armed: Some(counter),
        }
    }

    /// `"<floor>:<armed or ->"`.
    #[must_use]
    pub fn to_meta(self) -> String {
        match self.armed {
            Some(c) => format!("{}:{c}", self.floor),
            None => format!("{}:-", self.floor),
        }
    }

    /// An absent row is a seat with no history. A row that does not parse
    /// **fails closed**: a floor no counter reaches, so the folder seals rather
    /// than a corrupt row reading as "no history, trust anything genuine".
    #[must_use]
    pub fn from_meta(row: Option<&str>) -> Self {
        let Some(row) = row else {
            return Self::default();
        };
        let parsed = row.split_once(':').and_then(|(floor, armed)| {
            let floor = floor.parse().ok()?;
            let armed = match armed {
                "-" => None,
                c => Some(c.parse().ok()?),
            };
            Some(Self { floor, armed })
        });
        parsed.unwrap_or(Self {
            floor: u64::MAX,
            armed: None,
        })
    }
}

impl FolderSummary {
    /// Whether this list row is the set named `name` — by its `name_hash`, or
    /// by the plaintext a hash-less row (a reserved `__` set)
    /// still carries. The row twin of [`SetAddressed::addresses`], and the one
    /// match a caller holding a name makes over an **unrendered** list
    /// (`fauna_client_folders::FoldersClient::list_wire`): since schema 114 a sealed set's row rests no plaintext
    /// name, so `row.name == name` never finds it (`path-sealing.md` § the
    /// set-name plane).
    pub fn is_named(&self, name: &str) -> bool {
        if !self.name.is_empty() && self.name == name {
            return true;
        }
        let want = fauna_core::path_crypto::set_name_hash(name);
        self.name_hash.as_deref().map(|h| &h[..]) == Some(&want[..])
    }

    /// May this seat write the folder's content **UNSEALED**? The one verifier
    /// every seat asks (`encryption-at-rest.md` § Readable classes → *The
    /// declassification is owner-ATTESTED*).
    ///
    /// - `acting_name` — the folder name the **seat** is working under (its own
    ///   binding, the name it looked the row up by), never taken from the row
    ///   on the row's say-so.
    /// - `trusted_owner` — the owner identity from a source the nest cannot
    ///   forge: the seat's own actor id for a folder its account owns, the
    ///   folder channel's MLS-authenticated owner for a member's seat. `None`
    ///   (a member seat with no recorded owner) seals.
    /// - `memory` — what this seat remembers for this folder; persist the
    ///   returned value.
    ///
    /// Returns the verdict and the memory to persist. Every failure seals **and
    /// burns** the armed counter — call this only on a row from a list the seat
    /// actually read; for a folder absent from such a list use
    /// [`AttestationMemory::observe_sealed`], and for an unreadable list change
    /// nothing.
    #[must_use]
    pub fn judge_declassification(
        &self,
        acting_name: &str,
        trusted_owner: Option<&fauna_core::identity::ActorId>,
        memory: AttestationMemory,
    ) -> (bool, AttestationMemory) {
        let armed_counter = self
            .claims_unsealed()
            .then_some(())
            .and(self.audience_attestation.as_ref())
            .zip(trusted_owner)
            .filter(|(att, owner)| att.is_genuine(owner, self.id, acting_name))
            .map(|(att, _)| att.counter)
            .filter(|counter| *counter >= memory.floor);
        match armed_counter {
            Some(counter) => (true, memory.observe_armed(counter)),
            None => (false, memory.observe_sealed()),
        }
    }

    /// [`Self::judge_declassification`] for a folder a seat looked up in a list
    /// it **read successfully** — `found` is that list's row for the folder,
    /// or `None` when the list carried none. The row judges as usual; an
    /// absent row is a sealed verdict that **burns** the armed counter
    /// ([`AttestationMemory::observe_sealed`]): a deleted or un-shared set is
    /// not world-readable, and whatever this seat was armed under is withdrawn.
    /// Never call this for a list the seat could not read — that changes
    /// nothing, and the caller keeps its posture (the same keep-the-last
    /// discipline the sync mode has).
    #[must_use]
    pub fn judge_listed_declassification(
        found: Option<&Self>,
        acting_name: &str,
        trusted_owner: Option<&fauna_core::identity::ActorId>,
        memory: AttestationMemory,
    ) -> (bool, AttestationMemory) {
        match found {
            Some(row) => row.judge_declassification(acting_name, trusted_owner, memory),
            None => (false, memory.observe_sealed()),
        }
    }

    /// Does the nest claim this folder public while its attestation does **not**
    /// verify under `own` — the owner's re-confirm surface's one question
    /// (`folder-audience-unattested`, `ui/folders.md` § Audience and website
    /// serving)?
    ///
    /// For the OWNER's own row only: `own` is the seat's own actor id, the trust
    /// anchor [`Self::judge_declassification`] names for a folder its account
    /// owns, and the acting name is the row's own (the owner looks its folder up
    /// by the name it holds). True for a folder inherited through a
    /// succession (the attestation names the predecessor) — which a seat
    /// keeps sealed until the owner re-confirms, which re-mints
    /// (`encryption-at-rest.md` § Readable classes → *The declassification is
    /// owner-ATTESTED*).
    ///
    /// Built on the claim, not on `audience == public`: the nest-refused
    /// (public + WebDAV-served) fail-safe state claims nothing unsealed, so it
    /// owes no re-confirm. The replay memory is a fresh one — replay is a nest
    /// attacking a seat, not a folder its owner needs to re-sign.
    #[must_use]
    pub fn is_public_unverified_for(&self, own: &fauna_core::identity::ActorId) -> bool {
        self.claims_unsealed()
            && !self
                .judge_declassification(&self.name, Some(own), AttestationMemory::default())
                .0
    }

    /// Does the nest claim this folder public on a seat that holds **no trusted
    /// owner** for it — `trusted_owner` being what the seat's anchor resolved
    /// for this row, exactly as [`Self::judge_declassification`] takes it?
    ///
    /// A **diagnostic** question, never a verdict and never an input to one:
    /// the seat is sealed either way, and this only names *why* — a member seat
    /// on a host with no MLS state (`encryption-at-rest.md` § Implementation
    /// status today) — so the seat can say so in its log. A seat that HAS a
    /// trusted owner and still judges sealed (forged, replayed, unattested) is
    /// a different story and answers `false` here.
    #[must_use]
    pub fn is_public_claim_unanchored(
        &self,
        trusted_owner: Option<&fauna_core::identity::ActorId>,
    ) -> bool {
        self.claims_unsealed() && trusted_owner.is_none()
    }

    /// The nest's bare **claim** — never a verdict, which is why it is private:
    /// the one caller that may read it is [`Self::judge_declassification`]
    /// (and its re-confirm twin [`Self::is_public_unverified_for`]), and every
    /// seat reads the verdict, never the claim. Its predecessor, a public
    /// `rests_unsealed` that four readers armed the plaintext arm off, was
    /// retired when they adopted the verifier.
    ///
    /// **`true` requires an explicit `public` audience AND no WebDAV serving.**
    /// The second conjunct is a fail-safe, not a second question: the nest
    /// refuses that combination from *both* sides — serve-ON is rejected on an
    /// effective-public folder and declassify is rejected on an
    /// effective-served one (`folders.md` § Target re-model, phase 4,
    /// *Cross-toggle refusals*) — so it cannot legitimately arrive. If a corrupt
    /// projection carries both anyway, **serving wins and the content stays
    /// sealed**, which is the fail-closed direction for an impossible state.
    /// Everything else — every other audience, and the empty string an
    /// audience-less nest sends — claims nothing.
    ///
    /// **Not the same question as "is this folder public-by-design plaintext?"**
    /// That classifier deliberately carries no WebDAV fail-safe, because its callers fail safe
    /// in the opposite direction (they *skip* work). Its copies live nest-side
    /// and in the conversations index walk; do not merge the two.
    fn claims_unsealed(&self) -> bool {
        self.audience == AUDIENCE_PUBLIC && !self.webdav_enabled
    }

    /// Has the owner opted this folder out of resting chunk bytes on the nest?
    ///
    /// **Fail-closed to FULL residency**, the direction [`Self::residency`]
    /// states: only an explicit, parsed [`RESIDENCY_METADATA_ONLY`] may stop
    /// bytes resting. An absent row, an empty string,
    /// and any token this build does not recognise all upload as always — the
    /// safe direction, since the failure mode of guessing wrong is a folder
    /// whose bytes exist on no nest at all.
    #[must_use]
    pub fn is_metadata_only(&self) -> bool {
        self.residency == RESIDENCY_METADATA_ONLY
    }

    /// Is this a set shared *with* the caller that the caller may only read —
    /// a `role == "member"` row without an explicit `writer` grant? An absent
    /// [`Self::access`] is a reader (the fail-safe); an owner row never is.
    /// The one predicate behind the engine's reader guard and the on-demand
    /// presence plan's read-only flag, so the two cannot disagree on who a
    /// reader is.
    pub fn is_reader_member(&self) -> bool {
        self.role.as_deref() == Some("member") && self.access.as_deref() != Some("writer")
    }
}

/// `fauna.folders.set_web_paywall` — set or clear a website-enabled folder's paywall
/// tier (monetization.md § Pillar 2, the folder half). `tier: Some(name)`
/// paywalls the set to the caller's named subscription tier; `None` clears.
/// Caller-scoped (the set and the tier are both the caller's). A dedicated
/// kind rather than a `fauna.folders.update` field because clear-vs-leave-
/// unchanged would need a nested `Option`, which dag-cbor cannot round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderSetWebPaywallRequest {
    /// The website-enabled folder's owner-local name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `Some(tier)` = paywall to this tier; `None` = clear the paywall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Hash-first addressing (S5b, `file-sync.md` § Sealed names & paths):
    /// when present, resolves via `name_hash` before falling back to
    /// [`Self::name`] — the address that survives once the nest's plaintext
    /// `name` column scrubs post-flip (the caller already knows its own name
    /// and derives the hash itself; no seal-opening is involved, unlike the
    /// admin plane, `AdminFolderGetRequest::name_hash`). A malformed
    /// (non-32-byte) hash is refused, never silently ignored. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.folders.set_web_paywall`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderSetWebPaywallReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.folders.served_rows.adopt` — the owner signs, in place, a page of
/// the set's WebDAV pseudo-device rows before the flip OFF
/// (`writer-signed-change-records.md` ruling (7)(b)). Each signature covers
/// `fauna_protocol::sync_writer_sig::SignedChange::for_row_as` over the stored
/// row with the owner as actor and the row's own pseudo `device_id`; the nest
/// verifies each as at record, refuses the page whole on one that fails or on
/// a row [`crate::sync_writer_sig::served_row_adoptable`] rejects, and fills
/// the row's `signature` / `signer_key` without minting a row or moving a seq.
/// Owner-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ServedRowsAdoptRequest {
    /// The served set's owner-local name.
    pub name: String,
    /// Hash-first addressing, as on [`FolderUpdateRequest::name_hash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// The key every carried signature verifies under — the owner's identity
    /// key (direct) or a `SyncWrite` principal key the owner registered here.
    pub signer_key: ByteBuf,
    /// One page, at most [`SERVED_ROWS_ADOPT_PAGE`], keyed by `seq` — every
    /// row of the set under the pseudo device, superseded versions included,
    /// so `path_hash` is not unique across them.
    pub signatures: Vec<ServedRowSignature>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The most [`ServedRowsAdoptRequest::signatures`] one request carries: one
/// transaction per page bounds the nest's write lock however many rows the
/// served era wrote; the composition loops until `remaining` is zero.
pub const SERVED_ROWS_ADOPT_PAGE: usize = 256;

/// One served row's owner signature, by the row's `seq`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServedRowSignature {
    pub seq: i64,
    /// The 64-byte ed25519 signature.
    pub signature: ByteBuf,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline rule 4); empty on every write today.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.folders.served_rows.adopt`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServedRowsAdoptReply {
    /// Rows this page signed (an already-signed row counts nothing).
    pub adopted: u64,
    /// Adoptable pseudo-device rows of the set still unsigned after this page
    /// — the count the flip OFF refuses `served_rows_unadopted` with.
    pub remaining: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One device syncing a folder (`fauna.folders.devices`), mirroring the
/// twin's `DeviceSummary` JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderDevice {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    pub last_change_at: i64,
    pub change_count: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What one **device place** does with a folder's changes — the flag triple
/// that replaced the Source/Sync/Backup/Mirror role enum (folders re-model
/// § Places, ratified 2026-08-13; concept owner
/// `docs/goal/behavior/folders.md` § Target re-model).
///
/// A place is its three flags and nothing else: the legacy role strings, their
/// column and every mapping between the two retired with the role contraction
/// (2026-09-29), so there is no second spelling of a place to derive from or
/// drift against. All eight points are legal; the default point is
/// [`Self::default_place`].
///
/// `applies_deletes` is the delete guard (`SyncMode::applies_remote_deletes`)
/// and `accepts` gates a seat's delivery rails whole (`file-sync.md` § 4);
/// `originates` is carried for the upload side.
///
/// Not `Copy`: the rule-4 `extra` catch-all below owns a `BTreeMap`, and the
/// catch-all is not optional here — `PlaceFlags` is a plain client↔nest payload
/// struct with no signing and no capability negotiation, so neither of rule 4's
/// two opt-outs reaches it, and its own sibling nested structs in this file
/// ([`NestPlacePolicy`], [`VersionRetention`]) have carried it all along.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlaceFlags {
    /// Files added or edited here upload to the rest of the folder.
    pub originates: bool,
    /// Remote changes land here.
    pub accepts: bool,
    /// A peer's delete deletes here too. `false` is the archive seat, and the
    /// one direction that is not recoverable if guessed wrong (`principles.md`
    /// § No user-data loss).
    pub applies_deletes: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (`transport.md` § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl PlaceFlags {
    /// A place from its three flags, with an empty `extra`.
    pub fn new(originates: bool, accepts: bool, applies_deletes: bool) -> Self {
        Self {
            originates,
            accepts,
            applies_deletes,
            ..Default::default()
        }
    }

    /// The **default point** — all three flags. What every enrollment has been
    /// since before the flags existed, what the wizard's device checkbox writes
    /// for a user who touches nothing, and what a place-less device enrols at
    /// when it gains a local presence (`file-sync.md` § 4, *A local presence
    /// writes the place it needs*).
    pub fn default_place() -> Self {
        Self::new(true, true, true)
    }

    /// The **archive point** — originates and accepts, but a peer's delete
    /// never erases a file here (`ui/folders.md` § Photo backup).
    pub fn archive_place() -> Self {
        Self::new(true, true, false)
    }

    /// The three flags as a comparable point, **ignoring the rule-4 `extra`
    /// catch-all**.
    ///
    /// Which flag point a seat is, is the three bools and nothing else. Derived
    /// `PartialEq` also compares `extra`, so a whole-struct `==` would answer
    /// "a different seat" for one a newer nest had merely attached an unknown
    /// key to. Compare points, not structs, wherever the question is *which
    /// seat is this*.
    pub fn point(&self) -> (bool, bool, bool) {
        (self.originates, self.accepts, self.applies_deletes)
    }
}

/// One member of a folder (`fauna.folders.members.list`) — a **device place**
/// in the re-model's vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderMember {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    /// This place's behavior flags (folders re-model § Places) — the whole of
    /// what the seat does. Required: the legacy `role` string this field once
    /// sat beside retired with the role contraction, and a reply still carrying
    /// a `role` key rides [`Self::extra`], ignored.
    pub flags: PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One candidate version in a sync conflict — a manifest the user may choose
/// to keep. Self-contained (carries display metadata) so the `conflicts.list`
/// reply needs no join against `file_versions`. See
/// `docs/goal/behavior/file-sync.md` § Conflicts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConflictCandidate {
    /// Hex BLAKE3 manifest hash — the version kept if this candidate is chosen
    /// (passed back as `ConflictResolveRequest.winning_manifest_hash`).
    pub manifest_hash: String,
    /// Hex-encoded device id that produced this version.
    pub device_id: String,
    pub size_bytes: i64,
    /// When this version was recorded (unix seconds).
    pub created_at: i64,
    /// The M2 content-key generation this candidate's manifest was sealed
    /// under, for a bound (MLS-keyed) set — carried so the review-list
    /// "use the other version" re-point (`SyncClient::restore_version`) can
    /// echo it verbatim without a versions-projection join. `None` for
    /// unsealed sets and on pre-auto-resolve wire. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// [`SyncConflict::conflict_type`] of a change a device could never apply and
/// skipped: an unresolved, candidate-free row whose `device_id` names the
/// device that skipped (`conflicts.md` § Skipped catch-up changes reach the
/// review list).
pub const CONFLICT_TYPE_CATCHUP_FAILED: &str = "catchup_failed";

/// One sync conflict (`fauna.sync.conflicts.list`) — unresolved (the blocking
/// chooser flow) or auto-resolved (the review list, ratified 2026-07-10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncConflict {
    pub id: i64,
    /// Folder name (resolved from `folder_id` by the handler).
    pub folder: String,
    /// Hex-encoded device id that produced the conflicting change.
    pub device_id: String,
    pub path: String,
    pub conflict_type: String,
    pub details: Option<String>,
    pub created_at: i64,
    /// The diverging versions the user may choose between. Empty for a
    /// candidate-free (mark-only) conflict — the engine's degraded report when
    /// its local candidate upload failed — which resolves by `id` alone.
    /// Defaulted so a reply may omit it.
    #[serde(default)]
    pub candidates: Vec<ConflictCandidate>,
    /// When this conflict was resolved (unix seconds) — set for auto-resolved
    /// rows (the review-list surface) and chooser-resolved rows returned with
    /// `include_resolved`. `None` = still unresolved. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    /// How the conflict resolved: `"merged"` (clean three-way text merge; the
    /// winner is the merged result) | `"latest_wins"` (timestamp pick). `None`
    /// on unresolved rows and chooser-resolved rows. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Hex BLAKE3 manifest hash of the winning version (the head the devices
    /// converged on). On a `"merged"` resolution this is the merged result —
    /// NOT one of `candidates`. `None` on unresolved / candidate-free (mark-only) rows.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_manifest_hash: Option<String>,
    /// BLAKE3 of the normalized conflicting path — the key winner-propagation
    /// reads since path-sealing S1, and the salt `path_sealed` opens under.
    /// **Required** and sent to every reader (the pair below is the
    /// audience-gated half; this is the row's address): the nest writes it at
    /// every insert, so a row without it is refused at decode — the optional
    /// form served only a nest predating the sealed-label expand, retired
    /// under the compat-remnant sweep
    /// (`docs/goal/architecture/version-compatibility.md` § Dimension 2, the
    /// fourth exception).
    pub path_hash: ByteBuf,
    /// The conflicting `path`, sealed — see
    /// [`crate::sync::SyncChange::path_sealed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// `details`, sealed. Free text describing the conflict is **mutable under
    /// its salt**, so it seals with an explicit random nonce
    /// (`path_crypto::seal_random`), never the convergent mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_sealed: Option<ByteBuf>,
    /// [`Self::folder`], sealed — see
    /// [`crate::folders::FolderSummary::name_sealed`]. Opaque to the nest;
    /// rendered client-side by `fauna_core::label_custody::render_set_name`.
    /// Ships to every reader of this owner/participant-audience surface
    /// unconditionally — unlike `MediaItem`, there is no non-audience admin
    /// arm to project against here (`docs/goal/behavior/file-sync.md`
    /// § Sealed names & paths, S5c-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_sealed: Option<ByteBuf>,
    /// The convergent salt [`Self::folder_sealed`] opens under
    /// (`fauna_core::path_crypto::set_name_hash`) — the pair rule: a seal
    /// without its salt is unrenderable once the plaintext `folder` scrubs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.create (≡ POST /api/v1/file-sets) ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderCreateRequest {
    /// The new set's user-chosen name — carried only where the nest must rest
    /// it: a create with no [`Self::name_sealed`] (a custody-less client) and a
    /// `public` create (the name is the URL segment). A sealed create travels
    /// by [`Self::name_hash`] alone ([`SetAddressed::keeps_set_name`]), so the
    /// nest never learns that name; omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The new set's wire address, `fauna_core::path_crypto::set_name_hash` of
    /// [`Self::name`] — the convergent salt [`Self::name_sealed`] and
    /// [`Self::retention_policy_sealed`] open under, and the key the nest's
    /// `UNIQUE(name_hash, actor_id)` already enforces. Filled by the one shared
    /// create site (`fauna_client_folders::create_set`); the nest refuses a
    /// value that is not the hash of `name`, so the two can never disagree
    /// while both ride the wire, and with no `name` it is the whole address
    /// (the nest then requires `name_sealed`). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    pub retention_policy: Option<String>,
    /// Create-time conflict policy (`"auto"` | `"latest_wins_always"`) — the
    /// wizard stamps the user's global default (`fauna.state.sync-prefs`) here
    /// so a new set starts on the preferred policy atomically. `None` = the
    /// nest column default (`auto`). Wire-additive: absent ⇒ the column default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_policy: Option<String>,
    /// Create-time audience (phase 4): `Some("public")` = born declassified —
    /// a website folder created public rests plaintext from its first chunk,
    /// which is what makes non-paywalled serving work with no re-seal pass.
    /// `Some("private")` / `None` = the default (owner-sealed).
    /// `Some("shared")` is **refused** — bound-ness is entered through the
    /// share flow (`fauna.folders.share`), never spelled at create. Refused on
    /// a reserved `__` set. Wire-additive: absent ⇒ private.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// The new set's sealed name (`fauna_core::label_custody::seal_set_name`
    /// under the owner's `convergent_chunk_root()` — a brand-new set is
    /// owner-only, so there is no content-key generation yet). Filled by the
    /// one shared create site (`fauna_client_folders::create_set`) from the
    /// client's label custody, so a set is sealed from birth; `None` only from
    /// a client wired without custody, whose seal then arrives via
    /// [`FolderUpdateRequest::name_sealed`] (the seal backfill). Rejected on a
    /// reserved `__` set. Wire-additive.
    ///
    /// ⚠ **There is deliberately no path list on this request at all** — neither
    /// plaintext nor sealed — and the asymmetry with [`FolderUpdateRequest`] is
    /// the point rather than an omission to fix. `name_sealed` rides here
    /// because its salt is the name's own `name_hash`, which a client can derive
    /// before the row exists; the path lists seal under the **`folders.id`** the
    /// nest mints at INSERT, so a create could only ever carry them in
    /// plaintext. The plaintext pair left this request for that reason (paths
    /// are content, `encryption-at-rest.md` § Carve-outs; no writer ever filled
    /// them): the lists arrive on the first keyed `fauna.folders.update`, which
    /// is also the only gesture that ever edits them
    /// (`DevicesMachine::set_folder_paths`). A sealed carrier here would be one
    /// no writer can ever fill: a dark rail (caught by a dedicated dev-fleet
    /// audit script).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    /// [`Self::retention_policy`], sealed under the set's label-audience root
    /// (`fauna_core::label_custody::seal_retention_policy`) — S6-e.
    ///
    /// Rides here for the same reason [`Self::name_sealed`] does and **not** for
    /// the reason the path lists deliberately do not: its salt is the name's own
    /// `name_hash`, which a client can derive before the row exists. So this is a
    /// carrier a keyed create *can* fill, unlike an id-salted one.
    ///
    /// Filled beside [`Self::name_sealed`] by the same shared create site when
    /// the request carries a policy (a preset); a custody-less client's seal
    /// arrives on the first [`FolderUpdateRequest::retention_policy_sealed`] —
    /// which is where the one production retention editor writes anyway.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy_sealed: Option<ByteBuf>,
    /// The new set's **client-minted 32-byte set nonce** (writer-signed change
    /// records (2)): minted by the one shared create site and recorded in the
    /// owner's custody in the same act; the nest stores it opaque and echoes it
    /// as [`FolderSummary::set_nonce`]. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_nonce: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderCreateReply {
    pub id: i64,
    pub name: String,
    pub retention_policy: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.list (≡ GET /api/v1/file-sets) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FoldersListRequest {
    /// Opt into the **member-visible projection** (B3): when `Some(true)`, the
    /// reply unions the caller's owned sets with sets they are a *roster member*
    /// of (each a `role == "member"` summary carrying the owner handle). Absent /
    /// `Some(false)` ⇒ the historic **owner-scoped** enumeration — the contract
    /// the sync engines and the owner-side author flow rely on, so those callers are left untouched. Only
    /// the folders management UI (`DevicesMachine`) opts in. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_shared_with_me: Option<bool>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FoldersListReply {
    pub folders: Vec<FolderSummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.update (≡ PUT /api/v1/file-sets/{name}) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderUpdateRequest {
    /// Names the folder to update (the twin took it from the path).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `Some` = set, `None` = leave unchanged. (The twin's "clear" arm is
    /// unreachable via HTTP — see module docs.)
    pub retention_policy: Option<String>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
    /// `Some` = **replace** this folder's whole nest-place policy with the one
    /// given; `None` = leave it unchanged. Each knob left `None` *inside* the
    /// struct clears back to unset (the nest-wide default) — see
    /// [`NestPlacePolicy`] for why the set-vs-unchanged and on/off/default axes
    /// are split across the two levels rather than nested. Wire-additive: absent
    /// on old messages ⇒ unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_place: Option<NestPlacePolicy>,
    /// `Some` = **replace** this folder's whole version-retention policy
    /// (`file-versions.md` § Retention); `None` = leave it unchanged. Clearing
    /// means sending the binds-nothing policy (both bounds `0`), which the nest
    /// rests as `NULL` — the same sent-whole/applied-whole rule as
    /// [`Self::nest_place`]. Wire-additive: absent on old messages ⇒ unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_retention: Option<VersionRetention>,
    /// `Some(true)`/`Some(false)` = flag/unflag this set for WebDAV serving;
    /// `None` = leave unchanged. The Settings → Folders `folder-webdav-toggle`
    /// writes it (`webdav-server.md` § Independent enablement). Serving is
    /// refused on a `public`-audience folder (WebDAV serving is
    /// M2-content-key-sealed; a plaintext folder has no key to convey).
    /// Wire-additive: absent on old messages ⇒ unchanged.
    #[serde(default)]
    pub webdav_enabled: Option<bool>,
    /// `Some(audience)` = request an **audience transition** (phase 4);
    /// `None` = leave unchanged. Valid transitions, validated nest-side:
    /// `Some("public")` — **declassify** (any audience; the client shows the
    /// one-way confirm naming the consequence before sending — `principles.md`
    /// § The user always controls their data); `Some("private")` — flip back,
    /// legal only on an **unbound** folder; `Some("shared")` — flip back,
    /// legal only on a **bound** folder. Anything else, a bound→"private" or
    /// unbound→"shared" request, a reserved `__` set, or declassifying a
    /// WebDAV-served or paywalled folder ⇒ `fauna.folders.invalid_request`.
    /// The nest records the state flip only; re-sealing (flip-back) and
    /// un-sealing (declassify) of the back-catalogue are client-driven
    /// (`file-sync.md` § Content-Addressed Storage). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// `Some` = store this as the folder's audience attestation
    /// ([`FolderSummary::audience_attestation`]); `None` = leave it unchanged.
    /// Sent with `audience: Some("public")` by the attesting `set_audience`
    /// gesture. The nest stores it opaquely — owner-only like every field here,
    /// never verified nest-side. Wire-additive: absent unless the gesture attests, and the folder
    /// stays sealed on attesting seats until it is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_attestation: Option<AudienceAttestation>,
    /// `Some(true)`/`Some(false)` = flip this folder's **website toggle**
    /// (phase 4 — the per-folder control that replaced `mode == "web"`);
    /// `None` = leave unchanged. Refused on a reserved `__` set. Enabling does
    /// **not** require `public` audience — a paywalled website is sealed
    /// (`shared`-audience machinery) — but a sealed, un-paywalled website
    /// serves nothing (fail-closed 404) until the owner declassifies or sets a
    /// tier; the app UI leads the user through that. Wire-additive: absent on
    /// old messages ⇒ unchanged.
    #[serde(default)]
    pub website_enabled: Option<bool>,
    /// `Some("full" | "metadata_only")` = set the folder's **content
    /// residency** (folders re-model phase 5 — `file-sync.md` § Content
    /// residency); `None` = leave unchanged. Deliberately its **own field**,
    /// never folded into the sent-whole [`Self::nest_place`] record, where a
    /// policy edit would silently clear it. `"metadata_only"`
    /// is **consent-gated in the app** (`folder-residency-confirm` — the nest
    /// drops its chunk bytes for the folder on this flip; the app arms the
    /// confirm naming exactly that and calls this only once answered).
    /// Refused on reserved `__` rails and, both directions, against any
    /// serving surface (website ⊕ webdav ⊕ paywall — a site that is up only
    /// while the owner's laptop is on is a broken serving promise). Unknown
    /// values are rejected. Wire-additive: absent on old messages ⇒ unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// `Some(true)`/`Some(false)` = turn this folder's **exclusive editing** on
    /// or off; `None` = leave unchanged (`file-sync.md` § Exclusive editing).
    /// On ⇒ one device at a time may write: a seat acquires
    /// `fauna.folders.lease.acquire` once before an upload pass, renews while it
    /// runs, and releases when it drains. Owner-only, written by the app's
    /// `folder-exclusive-editing-toggle`.
    ///
    /// Deliberately **its own field and not a third
    /// [`Self::conflict_policy`] value**, though both are per-folder sync
    /// controls: the policy decides what happens *after* a divergence, this
    /// tries to stop one *before*. A lease-governed folder still needs a policy
    /// for what a lease cannot cover (an offline edit, an expired lease, a
    /// member who never took one), so both are meaningful at once.
    /// Wire-additive: absent on old messages ⇒ unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_editing: Option<bool>,
    /// `Some("auto" | "latest_wins_always")` = set the per-set conflict policy
    /// (file-sync.md § Conflicts, ratified 2026-07-10); `None` = leave
    /// unchanged. The Settings → Folders policy select writes it. Unknown
    /// values are rejected (`fauna.sync.bad_request`). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_policy: Option<String>,
    /// `Some` = **stamp** this set's sealed name (`folders.name_sealed`);
    /// `None` = leave unchanged. The set's plaintext `name` is *not* changed by
    /// this field — it names the row, and the seal is a sibling of it.
    ///
    /// This is the push half of the keyed-writer stamp: the create gesture holds
    /// no key material on any app today, so a set's name is sealed by whichever
    /// keyed writer next touches it — in practice the sync engine's
    /// bind/serve/first-record hook, which knows the set's current seal root
    /// (`file-sync.md` § Sealed names & paths). The nest rejects a stamp on a
    /// reserved `__` set. Wire-additive: absent ⇒ unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_sealed: Option<ByteBuf>,
    /// Hash-first addressing (S5b, `file-sync.md` § Sealed names & paths):
    /// when present, resolves via `name_hash` before falling back to
    /// [`Self::name`] — the address that survives once the nest's plaintext
    /// `name` column scrubs post-flip (the caller already knows its own name
    /// and derives the hash itself; no seal-opening is involved, unlike the
    /// admin plane, `AdminFolderGetRequest::name_hash`). A malformed
    /// (non-32-byte) hash is refused, never silently ignored. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// [`Self::include_paths`], sealed under the owner's root
    /// (`fauna_core::label_custody::seal_include_paths`) — the write half of
    /// `FolderSummary::include_paths_sealed`.
    ///
    /// ⚠ **The pair moves together, and that is enforced nest-side.** Whenever
    /// [`Self::include_paths`] is `Some`, this field's value is written with it —
    /// including `None`, which **clears** any existing seal. A keyless writer
    /// therefore drops the row to plaintext-only for S8's backfill to re-stamp,
    /// rather than leaving a row whose plaintext says one thing and whose seal
    /// opens to the list it replaced (post-flip the user would be shown the
    /// *stale* layout with nothing failing). This is the `sync_devices.label`
    /// rule from S6-b, applied. A request carrying only the seal (no plaintext)
    /// stamps it in place — the S8 backfill shape.
    ///
    /// Wire-additive: absent ⇒ unchanged when [`Self::include_paths`] is also
    /// absent, cleared when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_paths_sealed: Option<ByteBuf>,
    /// [`Self::exclude_paths`], sealed — the [`Self::include_paths_sealed`] twin,
    /// under the same pair-moves-together rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_paths_sealed: Option<ByteBuf>,
    /// [`Self::retention_policy`], sealed under the set's **label-audience** root
    /// (`fauna_core::label_custody::seal_retention_policy`) — the write half of
    /// `FolderSummary::retention_policy_sealed`, and the one durable writer of
    /// that column (S6-e). Reached today from apple's post-create retention
    /// editor; the shared create wizard is keyless.
    ///
    /// ⚠ **The pair moves together, enforced nest-side**, exactly as for
    /// [`Self::include_paths_sealed`]: whenever [`Self::retention_policy`] is
    /// `Some`, this field's value is written with it — including `None`, which
    /// **clears** any existing seal. A keyless writer therefore drops the row to
    /// plaintext-only for S8's backfill rather than leaving a seal that opens to
    /// the policy it replaced — post-flip that would show the user a *stale*
    /// retention rule with nothing failing. A request carrying only the seal
    /// stamps it in place: the S8 backfill shape. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy_sealed: Option<ByteBuf>,
    /// **Owner-only.** Overwrite the nest's stored copy of the set's nonce
    /// with the owner's live custody pick (`mls-group-key-material.md` § M2 →
    /// *Custody shape of the set nonce*, (f)): the nest checks every signed
    /// statement's binding against its stored copy, so that copy must trail
    /// custody — the owner reconcile pushes it whenever the echo
    /// ([`FolderSummary::set_nonce`]) differs. The nest never selects a nonce;
    /// a lying echo costs one redundant update. `None` = leave unchanged.
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_nonce: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderUpdateReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.public.fetch (net-new — the publicly-synced follow) ───────

/// `fauna.folders.public.fetch` request — the **client twin** of
/// `fauna.federation.folder.public.fetch`, the read a follower's app runs
/// against a `public`-audience folder (`docs/goal/behavior/folders.md`
/// § Publicly-synced follow; `docs/goal/architecture/federation.md` § The public
/// folder read plane owns the kinds and gates).
///
/// The caller's own nest serves this locally when the folder is homed here, and
/// relays it to [`Self::nest_url`] otherwise — exactly as every cross-nest
/// client read relays. **No requesting actor rides the federation hop**: there
/// is no membership to check, so a follower's identity never crosses the wire
/// (the home nest sees the requesting *nest* and source IP, its throttle keys,
/// and nothing about which user follows).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FoldersPublicFetchRequest {
    /// The folder's home nest. Absent (or empty) ⇒ the folder is homed on the
    /// caller's own nest — the same-nest follow, where two users share a nest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_url: Option<String>,
    /// Hex owner actor id, paired with [`Self::folder_name`] — the
    /// **first-contact** address, resolved from a handle by the existing
    /// discovery kinds. Ignored when [`Self::folder_id`] is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_actor_id: Option<String>,
    /// The folder's **plaintext** name. World-readable by the ratified public
    /// exception (names and paths are URLs), so name-addressed resolution of a
    /// public folder is not an oracle — every other existence question stays
    /// folded into one `not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    /// The home nest's stable `folders.id`, pinned by the first successful
    /// read. **Preferred once held**: a later rename never breaks an
    /// established follow. Meaningful only on the home nest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<i64>,
    /// Return changes with `seq` strictly greater than this (catch-up cursor).
    /// The served floor is `max(since, public_floor_seq)` — see
    /// [`FoldersPublicFetchReply::changes`].
    #[serde(default)]
    pub since: i64,
    /// Page size; `<= 0` = full page, clamped by the nest's fetch limit.
    #[serde(default)]
    pub limit: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.folders.public.fetch` reply — the public folder's identity plus one
/// floor-filtered, **stripped** page of its change log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FoldersPublicFetchReply {
    /// The home nest's stable `folders.id` — what the follower pins so later
    /// reads survive a rename.
    pub folder_id: i64,
    /// The folder's current plaintext name.
    pub name: String,
    /// The home nest's deployment `nest_actor_id` (hex 32-byte pubkey) — the
    /// byte-plane SPKI-pin trust root a cross-nest follower dials the open
    /// by-hash bulk plane under (`security.md` § Transport trust). `None` only
    /// if the home nest's signing key is unexpectedly absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    /// The page: rows with `seq` strictly above **both** the caller's `since`
    /// and the folder's `public_floor_seq`, oldest first, frame-budgeted.
    ///
    /// Each row is the ordinary `SyncChange` shape with the identity/key
    /// metadata **absent** — `device_id`, `author_actor_id`, `path_sealed`,
    /// `content_key_version`. A follower needs paths, manifest refs, sizes,
    /// types, timestamps and the causal watermark; never the owner's device
    /// fleet or authorship map. Manifests and chunks ride the existing open
    /// by-hash bulk GETs (plaintext for a public folder), never this channel.
    pub changes: Vec<crate::sync::SyncChange>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.delete (≡ DELETE /api/v1/file-sets/{name}) ────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDeleteRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderDeleteReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.devices (≡ GET /api/v1/file-sets/{name}/devices) ──────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDevicesRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderDevicesReply {
    pub devices: Vec<FolderDevice>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.places.set ────────────────────────────────────────────────
//
// The one add/edit door onto a device place (folders re-model § Places, design
// § Wire surface): it enrols a device that holds no place and rewrites the flags
// of one that does. `members.remove` is the only other roster verb; the
// role-speaking `members.add` retired with the role contraction.

/// Set (or create) a device place's behavior flags.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PlacesSetRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// Hex-encoded 32-byte device id of the place to set. The device must
    /// already be one of the actor's registered sync devices.
    pub device_id: String,
    /// The place's flags — **any** of the eight points.
    pub flags: PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PlacesSetReply {
    pub ok: bool,
    pub folder: String,
    pub device_id: String,
    pub flags: PlaceFlags,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.list (≡ GET /api/v1/file-sets/{name}/members) ─────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MembersListRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MembersListReply {
    pub members: Vec<FolderMember>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.list_actors ───────────────────────────────────────
// The *actor* (user) roster of a shared folder — who the set is shared with,
// backing the owner-side "Shared with" list (`docs/goal/ui/folders.md`
// § Sharing). Distinct from `members.list`, which is the *device* roster.

/// One actor (user) a shared folder is shared with, or its owner
/// (`fauna.folders.members.list_actors`). Projected over the set's derived
/// `ChannelId` `actor_channels` roster (the same roster the read gate
/// `folder_authz` admits members through). `role` is `"owner"` for the set's
/// owner and `"member"` for every other roster actor: the "Shared with" list
/// renders `role == "member"`, and the recipient-side "Shared by ‹handle›" badge
/// reads the `role == "owner"` entry. `handle` is resolved nest-side and is empty
/// when unknown (a remote actor, or a local user with no handle set).
///
/// There is deliberately no `status` field: the nest cannot observe an MLS group
/// *join* (that is client-side crypto), so every roster actor it can see is one
/// the share reached. The UI's "Pending" vs "Active" distinction is a client-side
/// state (optimistic while an invite is in flight) layered over this read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderActorMember {
    /// Hex-encoded 32-byte actor id.
    pub actor_id: String,
    /// Display handle resolved nest-side; empty when unknown (remote / unset).
    pub handle: String,
    /// `"owner"` | `"member"`.
    pub role: String,
    /// `"reader"` | `"writer"` — the member's access grant (multi-writer
    /// Phase 1, `ui/folders.md` § Sharing; **never** overloads `role`).
    /// Additive: absent ⇒ reader. `None` on `role == "owner"`
    /// rows (the owner has no grant — they own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// Writer byte cap; `None`/absent = uncapped (meaningful only when
    /// `access == "writer"`). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_cap: Option<i64>,
    /// Abuse counter of bytes this member's records currently contribute
    /// (file-sync.md § Multi-writer shared sets — not exact attribution).
    /// Additive; absent on a never-granted member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_used: Option<i64>,
    /// `Some(true)` for a **cross-nest** member (a `channel_foreign_members`
    /// row — their home nest relays their reads/sends; Phase 2,
    /// `ui/folders.md` § Sharing → Cross-nest members). Additive: absent on
    /// every local row. Their `handle` is empty (a foreign
    /// member is not a local `users` row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<bool>,
    /// The landed succession statements whose chain **ends at this member** —
    /// each the verbatim canonical bytes of a
    /// `fauna_core::recovery::SignedIdentitySuccession` its submitter signed,
    /// oldest first (the shape of
    /// [`crate::recovery::SuccessionLookupReply::statements`]). On the owner
    /// row and each `writer` row only; empty for an identity that never
    /// succeeded another. Additive.
    ///
    /// What a reader proves a retired identity's rows by
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(b)): the nest carries the statements, it never asserts the
    /// link — a reader admits a predecessor only where every link's `new_sig`
    /// verifies under that link's own successor
    /// ([`crate::sync_row_verify::writer_roster`]), so a statement the nest
    /// invents, drops or reorders proves nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub succession_statements: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ActorMembersListRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActorMembersListReply {
    pub members: Vec<FolderActorMember>,
    /// The caller's live access grant as the set's HOME nest stamps it — set
    /// only on the cross-nest relay ([`ActorMembersListRemoteRequest`]), the
    /// same stamp `ContentKeyGetReply::caller_access` carries. `None` asserts
    /// nothing (advisory for the UI, never a revocation signal). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// The folder's residency as the set's HOME nest stamps it — set only on
    /// the cross-nest relay, the same stamp `ContentKeyGetReply::residency`
    /// carries. `None` = not stated, never *full*. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.list_actors_remote ────────────────────────────────

/// `fauna.folders.members.list_actors_remote` — a **cross-nest member's**
/// actor-roster read: the set's authoritative roster lives on its home nest, so
/// the member's own nest relays the read there via
/// `fauna.federation.folder.actors.fetch` (`federation.md` § Cross-nest…, *The
/// cross-nest writer roster read*). The writer-signed change-record reader's
/// roster source for a foreign set (`mls-group-key-material.md` § M2, ruling
/// (3)).
///
/// A **distinct kind** — deliberately NOT an additive `nest_url` on
/// `members.list_actors` — for the `fauna.conversations.channel.actors_remote`
/// reason: a roster is an **authorization input** to the reader, and an old
/// member-nest ignoring an additive field on a *name*-addressed request would
/// answer the member's own same-named set's roster as a clean success,
/// installing another set's writers on this one. An unknown kind fails typed
/// `unknown_kind`, the read fails, and the reader's fail-closed posture holds.
///
/// Replies with [`ActorMembersListReply`] (ids-only: every `handle` is empty
/// across nests), stamped with the home nest's `caller_access`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ActorMembersListRemoteRequest {
    /// Hex-encoded channel id (32 bytes) of the shared set.
    pub channel_id: String,
    /// The set's home nest base URL. Required and non-empty — a same-nest set
    /// uses `members.list_actors`.
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.set_access (multi-writer Phase 1) ─────────────────
// Grant or change a member's `reader`/`writer` access (+ optional byte cap) on
// a shared set. Owner-scoped AND claimant-gated exactly like `content_key.put`
// (`ui/folders.md` § Sharing owns the access model). Role transitions never
// rotate the content key — a demoted writer stays a reader (they already hold
// the keys); removal is `members.evict`, which rotates.

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemberSetAccessRequest {
    /// The **owner's** shared (group-bound) folder; looked up by
    /// `(name, authenticated caller)` — the caller, not a payload field, is
    /// the owner.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded 32-byte actor id of the member whose access is being set.
    pub actor_id: String,
    /// `"reader"` | `"writer"`.
    pub access: String,
    /// Writer byte cap in bytes; `None` = uncapped (the ratified
    /// blank-cap-means-uncapped — clients warn on an uncapped writer grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_cap: Option<i64>,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemberSetAccessReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.remove ────────────────────────────────────────────
// (≡ DELETE /api/v1/file-sets/{name}/members/{device_id})

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemberRemoveRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberRemoveReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.members.evict (shared folders, Slice 3 — F1/OBS-1) ───────
//
// Evict a **cross-user actor** (a roster member of the shared MLS group) from the
// set's `actor_channels` roster — the metadata-confidentiality half of
// rotate-on-removal (`docs/goal/architecture/mls-group-key-material.md` § M2,
// "MUST also evict the removed member from `actor_channels`"). Distinct from
// `members.remove` above, which removes one of the **owner's own sync devices**
// (`device_id`) from `folder_members`; this removes another **user** (`ActorId`)
// from the shared roster so they can no longer read discovery metadata (snapshot
// lists / names / sizes). Owner-scoped; the nest derives the `ChannelId` from the
// set's stored `mls_group_id` and runs the only scoped `DELETE FROM
// actor_channels` in the codebase. Idempotent: evicting an already-absent member
// returns `ok: true, evicted: false`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MemberEvictRequest {
    /// The shared (group-bound) folder the member is being evicted from.
    /// Owner-scoped: the nest looks it up by `(name, authenticated caller)`; a
    /// non-owner or owner-only (unshared) set is rejected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded 32-byte `ActorId` (Ed25519 pubkey) of the cross-user member to
    /// evict — the actor the owner removed from the MLS group.
    pub member: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberEvictReply {
    pub ok: bool,
    /// Hex-encoded 32-byte `ChannelId` the eviction targeted
    /// (`ChannelId::from_group_id(set.mls_group_id)`).
    pub channel_id: String,
    /// Whether a roster row was actually removed (`false` ⇒ the member was
    /// already absent — an idempotent no-op, e.g. a crash-resumed eviction).
    pub evicted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.leave (shared folders, Slice 3 — recipient self-remove) ──
//
// A **member** voluntarily leaves a set shared *with* them — the recipient
// counterpart to the owner-side `members.evict` above. Distinct in *authorization*
// and *addressing*: `members.evict` is owner-scoped and addressed by the set's
// `name` (which a member does not own / know); `leave` drops the **authenticated
// caller's own** row from the derived channel roster, addressed by the raw
// `mls_group_id` the member holds (projected into their B3 member-visible
// `FolderSummary`). Self-scoped: no `ownerSecret`, no first-binder-claimant check
// — "you can always remove yourself". The client also locally forgets the MLS group
// (`MlsEngine::forget_group`), so the set drops from `has_group`-filtered list
// rendering; off the roster, the leaver's `content_key.get` is denied (they stop
// receiving rotations). A voluntary leave does **not** rotate the owner's content
// key — the leaver keeps the generations they already held ("forward secrecy from
// yourself is not a threat", `mls-group-key-material.md` § M2). Same-nest today;
// cross-nest leave (roster on the sharer's home nest) is a follow-on with B4.

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemberLeaveRequest {
    /// Hex-encoded raw MLS `group_id` of the shared set to leave — the member
    /// holds it in their member-visible `FolderSummary.mls_group_id` (B3); the
    /// nest derives `ChannelId::from_group_id(group_id)` and drops the caller.
    pub group_id: String,
    /// Cross-nest leave (additive, Phase 2 — `ui/folders.md` § Sharing →
    /// Leave): when set, the caller's roster row lives on the set's HOME nest
    /// (`channel_foreign_members`), so the caller's own nest relays
    /// `fauna.federation.channel.leave` there — killing all future federated
    /// fetches; generations already held are not revoked (exact parity with
    /// same-nest voluntary leave). Additive per the ratified wire rule; without
    /// it the local path fails visibly (no local roster row → `left: false`),
    /// never silently misdirects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl MemberLeaveRequest {
    /// Build the `fauna.folders.leave` request — **the one place this shape is
    /// assembled**, so the two dispatchers that issue it cannot drift apart.
    ///
    /// `fauna_client_folders::FoldersClient::leave_with_home` owns the call
    /// for ordinary callers, but `fauna-client-conversations` has to issue the
    /// same request directly: `fauna-client-folders/mls` depends on *that* crate
    /// (for `ConversationsClient`'s keypackage/welcome RPCs), so importing the
    /// client back would be a package cycle. Both build through here instead of
    /// keeping two hand-written struct literals in step by hand.
    ///
    /// Every field is listed explicitly — deliberately **not**
    /// `..Default::default()` — so a field added to this request is a compile
    /// error in this one constructor, where an author must decide what it should
    /// be, rather than silently taking its default on each dispatch path.
    ///
    /// `home_nest_url`: `Some` for a **foreign** (cross-nest) set, so the
    /// caller's own nest relays `fauna.federation.channel.leave` to the set's
    /// home nest; `None` for the plain same-nest self-drop.
    #[must_use]
    pub fn new(group_id_hex: impl Into<String>, home_nest_url: Option<impl Into<String>>) -> Self {
        Self {
            group_id: group_id_hex.into(),
            nest_url: home_nest_url.map(Into::into),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberLeaveReply {
    pub ok: bool,
    /// Hex-encoded 32-byte `ChannelId` the self-eviction targeted
    /// (`ChannelId::from_group_id(group_id)`).
    pub channel_id: String,
    /// Whether a roster row was actually removed (`false` ⇒ the caller was not a
    /// member — an idempotent no-op, e.g. a crash-resumed / double-tapped leave).
    pub left: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One folder's live exclusive-edit lease, as [`FolderSummary::lease`] reports
/// it (`file-sync.md` § Exclusive editing).
///
/// A *report*, never a grant: it says which device holds the folder and until
/// when, so a seat can render the folder read-only and name the holder. Taking
/// a lease is [`LeaseAcquireRequest`]'s job and nothing here substitutes for it
/// — between reading this and acquiring, another seat may win, and the acquire
/// is the atomic arbiter. That race is not a defect: this field is for
/// *rendering*, the acquire is for *writing*.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderLeaseState {
    /// Hex-encoded 32-byte device id of the holder, **empty when the reader is
    /// not the account that holds the lease** (the id is client-asserted and
    /// means nothing to another account). A client renders the
    /// device's **label** (from its own devices projection), never this hex —
    /// a 64-character string is not an answer to "why can't I edit this?"; an
    /// empty or unknown id renders as *another device is editing*.
    pub device_id: String,
    /// Unix epoch seconds at which the lease lapses if the holder does not
    /// renew. A reader must treat a past value as *not held*: the nest sweeps
    /// expired rows lazily, on the next acquire for that folder, so a stale row
    /// can outlive its expiry in the table.
    pub expires_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.lease.acquire (≡ POST /api/v1/file-sets/{name}/lease) ─────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LeaseAcquireRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaseAcquireReply {
    pub acquired: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.lease.release (≡ DELETE /api/v1/file-sets/{name}/lease) ───

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LeaseReleaseRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded 32-byte device id of the releasing holder — REQUIRED: the
    /// nest releases only the lease this device holds FOR THE CALLING ACCOUNT
    /// (the lease is bound to the actor that took it), so a writer member can
    /// never drop another actor's active lease, whatever device id
    /// it names. The former
    /// device-id-less arm (a holder-blind clear for an old client, or an owner
    /// force-releasing a crash-stuck lease) was retired 2026-09-24 under the
    /// compat-remnant sweep (`version-compatibility.md` § Dimension 2, the
    /// fourth exception): no caller sent it, no app offered it, and a
    /// crash-stuck lease lapses on the nest's TTL takeover.
    pub device_id: String,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaseReleaseReply {
    pub released: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.write_token.get (cross-nest writer byte-plane token) ───────

/// `fauna.folders.write_token.get` — a **net-new** per-actor kind (Phase 3):
/// a cross-nest **writer** asks its OWN nest to relay a short-lived byte-plane
/// write token from the set's home nest (`fauna.federation.folder.write_token.mint`),
/// so the writer's client can POST sealed chunks/manifests DIRECT to the home
/// nest over the open by-hash bulk plane. Net-new (not an additive field) because
/// there is no local fallback that would "fail loud" — an old own nest returns
/// typed `unknown_kind`, exactly the S5 loud signal. Channel-keyed (a foreign
/// set's `name` only resolves on its home nest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteTokenGetRequest {
    /// The set's home nest URL (from the `ForeignFolder` record).
    pub nest_url: String,
    /// The foreign set's derived 32-byte `ChannelId` (hex).
    pub channel_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.folders.write_token.get` reply — the opaque write-only bulk token +
/// its absolute expiry (Unix seconds). The client POSTs bytes to the home nest
/// URL it already holds, bearing this token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteTokenGetReply {
    pub token: String,
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.read_token.get (cross-nest member byte-plane read token) ───

/// `fauna.folders.read_token.get` — the read-scoped twin of
/// [`WriteTokenGetRequest`]: a cross-nest **member** (a reader holds no write
/// grant to mint under) asks its OWN nest to relay a short-lived byte-plane
/// read token from the set's home nest
/// (`fauna.federation.folder.read_token.mint`). The home nest takes it at one
/// door — the store-miss relay arm of `GET /api/v1/chunks/{hash}` — and
/// re-checks the membership there at every request. Net-new, like the
/// write twin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadTokenGetRequest {
    /// The set's home nest URL (from the `ForeignFolder` record).
    pub nest_url: String,
    /// The foreign set's derived 32-byte `ChannelId` (hex).
    pub channel_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.folders.read_token.get` reply — the opaque read-only bulk token +
/// its absolute expiry (Unix seconds).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadTokenGetReply {
    pub token: String,
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.conflicts.list (≡ GET /api/v1/sync/conflicts) ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConflictsListRequest {
    /// `Some(true)` = also return **resolved** conflicts (the auto-resolve
    /// review list, newest-first) alongside unresolved ones. Absent /
    /// `Some(false)` ⇒ the historic unresolved-only contract every existing
    /// caller relies on. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_resolved: Option<bool>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConflictsListReply {
    pub conflicts: Vec<SyncConflict>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.conflicts.report (≡ POST /api/v1/sync/conflicts) ───────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConflictReportRequest {
    /// Folder name (owned by the caller).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hex-encoded device id.
    pub device_id: String,
    pub path: String,
    pub conflict_type: String,
    pub details: Option<String>,
    /// The diverging versions the reporter (the sync engine) saw — its
    /// own local manifest plus the incoming manifest that conflicted. Empty
    /// when the engine's local candidate upload failed and it degraded to a
    /// candidate-free (mark-only) report. Defaulted so a request may omit it.
    #[serde(default)]
    pub candidates: Vec<ConflictCandidate>,
    /// Auto-resolve (ratified 2026-07-10): `Some("merged" | "latest_wins")`
    /// reports the conflict **already resolved** by the detecting device — the
    /// row lands with `resolved_at` set and feeds the review list instead of
    /// the blocking chooser. Requires `winning_manifest_hash`. `None` = the
    /// unresolved (mark-only) report that feeds the chooser flow. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Hex BLAKE3 manifest hash of the version the device resolved to. On
    /// `"merged"` this is the merged result, not one of `candidates`. Required
    /// with `resolution`; ignored without it. The nest PROPAGATES a resolved
    /// report transactionally (same precedent as `conflicts.resolve`
    /// choose-winner): it records the reporter's losing candidate as an
    /// ordinary `sync_changes` row (version retention — never when it IS the
    /// winner) and then the winner as the new head row, so every device
    /// converges via normal catch-up and both versions stay listable +
    /// GC-pinned (file-sync.md § Conflicts / § File Versions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_manifest_hash: Option<String>,
    /// Byte size of the winning version — required with `resolution` when the
    /// winner is not one of `candidates` (a merged result); for a candidate
    /// winner the nest takes the candidate's recorded size. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_size_bytes: Option<i64>,
    /// M2 content-key generation the winning version's chunks were sealed
    /// under (bound sets; `None` for unsealed sets / candidate winners, which
    /// carry their own). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_content_key_version: Option<u64>,
    /// Hash-first addressing (S5b): when present, resolves via `name_hash`
    /// before falling back to [`Self::folder`] — see
    /// `FolderUpdateRequest::name_hash` for the full rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// BLAKE3 of the normalized [`Self::path`] — the routing/PK-class key the
    /// stored row is addressed by (path-sealing S6-a). The nest derives it from
    /// [`Self::path`] when absent;
    /// it becomes the only source once the plaintext write flip lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    /// [`Self::path`] sealed under the set's label root — the write half of the
    /// conflict plane (path-sealing S6-a). Mint it with
    /// `fauna_core::label_custody::seal_path`, never by hand: the
    /// entire path plane shares one field tag because the nest copies these
    /// blobs verbatim between tables. Surfaces as
    /// [`SyncConflict::path_sealed`], which shipped ahead of this writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// [`Self::details`] sealed under the same root, salted by the conflict's
    /// own `path_hash` (path-sealing S6-a). Mint it with
    /// `fauna_core::label_custody::seal_conflict_details` — **random nonce**,
    /// because details is mutable prose the salt does not determine. Surfaces
    /// as [`SyncConflict::details_sealed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_sealed: Option<ByteBuf>,
    /// Causal watermark for the propagated WINNER head row (the causal-watermark
    /// ruling, 2026-08-02 — `sync::SyncChange::derived_through` has the
    /// contract): the set seq through which the resolving device had
    /// incorporated every row when it computed the resolution — for an in-order
    /// catch-up/apply pass, the incoming row's own seq. The nest stamps the
    /// winner head row with this **exactly as sent** — never upgraded to the
    /// loser retention row's seq (retired 2026-09-27, `conflicts.md` §
    /// *Retention rows are transparent to the licence*), so it is the value
    /// [`Self::winner_signature`] signs — plus `is_resolution =
    /// !winning_carries_novelty`. Wire-additive; absent from a reporter that
    /// sends none → the winner row rests NULL (unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_derived_through: Option<i64>,
    /// Causal watermark for the reporter's LOSING candidate retention row — the
    /// seq of the ancestor the reporter's local content derived from (its
    /// ledger ancestor), when known. The retention row is a fresh edit
    /// (`is_resolution` false), never a resolution. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub losing_derived_through: Option<i64>,
    /// The winner CARRIES NOVEL CONTENT (the same-anchor ruling, 2026-08-05 —
    /// `conflicts.md` § Concurrent resolution's decision record): the report
    /// consumed unpublished local novelty (pre-merge content with no seq
    /// anywhere), so the winner row is that novelty's only carrier and must
    /// mint EDIT-class — the ratified `is_resolution` semantics ("carries no
    /// novel content") applied honestly. `Some(true)` → the nest stamps the
    /// winner head row `is_resolution = false`; absent/`Some(false)` → the
    /// resolution stamp. Wire-additive: a reporter that sends none gets the
    /// resolution stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winning_carries_novelty: Option<bool>,
    /// The reporter's / chooser's signature over the **winner head row's**
    /// `SignedChange` statement exactly as the nest will mint it (`device_id` =
    /// the winner's) — writer-signed change records (1)(ii); the nest verifies
    /// it as at record and copies it onto the minted row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_signature: Option<ByteBuf>,
    /// The key [`Self::winner_signature`] verifies under (see
    /// `SyncChange::signer_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_signer_key: Option<ByteBuf>,
    /// The reporter's signature over the **retention row** of its own losing
    /// candidate, exactly as the nest will mint it
    /// (`SignedChange::for_retained_loser`, writer-signed change records ruling
    /// (10)(d)) — present whenever the report retains a loser. One signer per
    /// report: it verifies under [`Self::winner_signer_key`]. The nest copies
    /// it onto the retention row, so the losing version is listed in the
    /// version history signed as its reporter. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loser_signature: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConflictReportReply {
    pub id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sync.conflicts.resolve (≡ POST /api/v1/sync/conflicts/{id}/resolve) ─

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConflictResolveRequest {
    pub id: i64,
    /// Hex BLAKE3 manifest hash of the candidate version to keep — must be one
    /// of the conflict's recorded candidates. `None` is the candidate-free
    /// (mark-only) resolve (flips `resolved_at`, propagates nothing) — the only
    /// resolve a conflict reported with no candidates admits.
    #[serde(default)]
    pub winning_manifest_hash: Option<String>,
    /// The reporter's / chooser's signature over the **winner head row's**
    /// `SignedChange` statement exactly as the nest will mint it (`device_id` =
    /// the winner's) — writer-signed change records (1)(ii); the nest verifies
    /// it as at record and copies it onto the minted row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_signature: Option<ByteBuf>,
    /// The key [`Self::winner_signature`] verifies under (see
    /// `SyncChange::signer_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_signer_key: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConflictResolveReply {
    pub resolved: bool,
    /// Echoes the chosen winner when the resolve propagated a version
    /// (`None` for the candidate-free (mark-only) path).
    #[serde(default)]
    pub winning_manifest_hash: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.share ─────────────────────────────────────────────────────
//
// Cross-user sharing (Slice 2 of shared folders; design tracked
// internally; `docs/goal/architecture/
// mls-group-key-material.md` § Audience: an MLS group). A **net-new** kind (not an
// HTTP→WS-RPC migration): binds an existing owner-only folder to a
// **client-created** MLS group (conv-style — the client owns MLS group creation),
// transitioning it to a group-scoped *shared* set. The nest derives the 32-byte
// `ChannelId` from the raw group id (`fauna_mls::types::ChannelId::from_group_id`,
// the same fn `SyncEngine::chunk_root` uses), repoints `folders.actor_id` to it
// (the conv `folders.actor_id = channel_id` trick — `message-segment-store.md:294`
// — so change-log/manifest reads are group-scoped), persists `mls_group_id = raw`
// (the `chunk_crypto` per-chunk root source, P2/P3), and registers the owner on
// the `actor_channels` group roster.
//
// **Not in this kind:** member-add reuses `fauna.conversations.welcome.deliver`
// (its handler already calls `register_actor_channel` on receipt); member-remove
// rides the MLS Remove commit (epoch advance — a removed member can't export the
// new-epoch chunk root). The rotate-on-removal re-key + roster removal are Slice 3.
//
// **FS-BIND-3:** the handler
// (S2-P2) sources the owner from the **authenticated caller** (the connection
// actor), never a payload field, and derives `channel_id` **server-side** — the
// request carries only `name` + the raw `group_id`, never a self-asserted channel
// id (no authz decision off a self-asserted field). The reply **echoes** the
// derived `channel_id` so the client reuses the identical value as the
// `welcome.deliver` `channel_id` and the set's change-log scope.

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderShareRequest {
    /// The owner-only folder to share. The nest looks it up by
    /// `(name, authenticated caller)` — the caller (connection actor), not a
    /// payload field, is the owner (FS-BIND-3).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Hex-encoded **raw** MLS group id the client created for this set.
    /// Variable length (openMLS group ids are not fixed-32); the nest
    /// BLAKE3-derives the 32-byte `ChannelId` from it. Stored verbatim in
    /// `folders.mls_group_id`, the `chunk_crypto` per-chunk root source.
    pub group_id: String,
    /// Hex-encoded 32-byte actor id of the member this share invites — carried
    /// so the share can record the member's access grant (below) in the same
    /// nest call as the bind, before the Welcome is even delivered. Additive;
    /// **never an authz input** (the roster still comes only from
    /// `welcome.deliver`, and a caller could set any grant on their own set via
    /// `members.set_access` regardless — this is a convenience, not a gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_actor_id: Option<String>,
    /// Share-time access grant for the invited member: `"reader"` | `"writer"`
    /// (multi-writer Phase 1). Additive; absent = reader (absent role row = reader,
    /// fail-safe). Recorded with the bind so there is no window where a joined
    /// writer's grant is still unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderShareReply {
    pub ok: bool,
    pub folder: String,
    /// The set's `name_hash` (`fauna_core::path_crypto::set_name_hash`) — the
    /// address that survives once the plaintext [`Self::folder`] scrubs
    /// (`path-sealing.md` § the set-name plane). `None` for a reserved `__` set,
    /// which has no hash. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// Hex-encoded 32-byte `ChannelId` the nest derived from `group_id`
    /// (`ChannelId::from_group_id(group_id)`). The client uses this as the
    /// `channel_id` in subsequent `fauna.conversations.welcome.deliver`
    /// member-adds, and as this set's change-log scope.
    pub channel_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.content_key.{put,get} (shared folders, Slice 3 — M2) ────
//
// The opaque transport for the M2 **content-key envelope**
// (`docs/goal/architecture/mls-group-key-material.md` § M2 content-key
// mechanism). The owner seals the full generation bundle in `fauna-mls`
// (`MlsEngine::seal_content_key_envelope`) and `put`s it nest-side, keyed by the
// derived `ChannelId`, **re-published on every membership change**. Any roster
// member `get`s it and opens it with their group's current epoch secret
// (`open_content_key_envelope`). The nest stores `sealed` opaque — it never holds
// the group secret.
//
// Both kinds **address the set by `name`** (like the rest of Slice 2/3): the
// handler resolves the owner-scoped row for `put` (owner-only — members are
// read-only) and the member-aware readable row for `get` (`folder_authz`),
// derives the `ChannelId` from the set's stored `mls_group_id`, and that is the
// storage key. `sealed` rides as **hex** (the module's binary-as-hex convention —
// `device_id` / `manifest_hash` / `group_id`), keeping every field
// `String`/`i64`/`bool`.

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContentKeyPutRequest {
    /// The **shared** (group-bound) folder whose envelope is being published.
    /// Owner-scoped: the nest looks it up by `(name, authenticated caller)`; a
    /// non-owner or owner-only (unshared) set is rejected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The MLS epoch the owner sealed the envelope under (the current epoch at
    /// publish time; staleness metadata — the read path derives the *current*
    /// epoch key).
    pub epoch: i64,
    /// Hex-encoded opaque sealed envelope bytes
    /// (`SealedContentKeyEnvelope::sealed`).
    pub sealed: String,
    /// The bundle's current content-key generation — the owner-stamped
    /// **version floor** for non-owner records (KMH § M2 version floor,
    /// multi-writer Phase 1). **Monotonic nest-side**: the nest stores
    /// `MAX(stored, this)`, so a lower stamp never lowers an established
    /// floor. Readers are unaffected; a non-owner `changes.record` stamped
    /// below the floor is refused `stale_content_key`.
    pub current_version: u64,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentKeyPutReply {
    pub ok: bool,
    /// Hex-encoded 32-byte `ChannelId` the envelope was stored under
    /// (`ChannelId::from_group_id(set.mls_group_id)`).
    pub channel_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContentKeyGetRequest {
    /// The shared folder whose envelope to fetch. Member-aware: the nest
    /// resolves the readable row (owner *or* roster member); a non-member or
    /// owner-only set folds to `not_found` (ST-RES-1 oracle closed).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Cross-nest relay (additive, Phase 2): when set (with `channel_id`), the
    /// caller's own nest relays via `fauna.federation.folder.content_key.fetch`
    /// to the set's home nest. Additive per the ratified wire rule; without
    /// it the local path fails visibly (`not_found`) for a foreign set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nest_url: Option<String>,
    /// The foreign set's derived 32-byte `ChannelId` (hex); required with
    /// `nest_url`, ignored without it (`name` only resolves on the home nest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Hash-first addressing (S5b) — see `FolderUpdateRequest::name_hash`.
    /// Same-nest reads only (a foreign set with `nest_url` set addresses by
    /// `channel_id`, not `name`/`name_hash`, so this is ignored alongside it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ContentKeyGetReply {
    /// The MLS epoch the stored envelope was sealed under.
    pub epoch: i64,
    /// Hex-encoded opaque sealed envelope bytes, fed verbatim to
    /// `MlsEngine::open_content_key_envelope`.
    pub sealed: String,
    /// **Cross-nest relay only** — the caller's current access grant on the set
    /// (`"reader"`/`"writer"`), stamped by the set's HOME nest and threaded back
    /// through this nest's relay. The refresh half of *Recipient-side access
    /// discovery* (`docs/goal/architecture/federation.md` § Cross-nest): the
    /// client CAS-updates its `ForeignFolder.access` when it differs, so a
    /// promotion or demotion is picked up on the ordinary commit-poll cadence
    /// with no push kind.
    ///
    /// `None` on a same-nest read (the caller's own nest already projects
    /// `access` onto the `FolderSummary`) — which asserts nothing and must
    /// never be read as a revocation.
    /// **Advisory-for-UI only, never an authorization input.**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_access: Option<String>,
    /// **Cross-nest relay only** — the home nest's deployment `nest_actor_id`
    /// (hex 32-byte pubkey), threaded back through the relay so the member
    /// refreshes its stored `ForeignFolder.home_nest_actor_id` — the byte-plane
    /// SPKI-pin trust root (`security.md` § Transport trust, federation-granted
    /// Axis-2 row). `None` same-nest ⇒ keep what is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    /// **Cross-nest relay only** — the set's owner-stamped content-key floor
    /// off the home nest's envelope row, the same value `fauna.folders.list`
    /// projects as [`FolderSummary::content_key_floor`]. A cross-nest member
    /// has no row on its own nest, so this reply is the carrier that lets its
    /// engine hold a write behind the floor locally instead of learning of a
    /// rotation only from the home nest's `stale_content_key` refusal
    /// (`docs/goal/behavior/on-demand-files.md` § Shared sets on a capability
    /// host → *One mechanism*, question 2). The member refreshes its stored
    /// `ForeignFolder.content_key_floor` from it. `None` same-nest, or from a
    /// home nest that holds no floor ⇒ keep what is held
    /// (never a claim the floor was cleared).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_floor: Option<u64>,
    /// **Cross-nest relay only** — the folder's content residency
    /// (`"metadata_only"` / `"full"`) as the set's HOME nest stamps it off its
    /// own row, threaded back through this nest's relay
    /// (`docs/goal/architecture/federation.md` § Cross-nest shared folders +
    /// channel append → *Relay serving across nests*, the `residency` stamp).
    /// The member refreshes its stored `ForeignFolder.residency` from it, and
    /// its engine arms the upload skip and the holder-keeps gate from that
    /// record. `None` same-nest ⇒ *not stated* — never *full*; keep what is
    /// held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    /// **Cross-nest relay only** — the set's owner's handle, as the set's HOME
    /// nest stamps it off its own `users` row, forwarded by this nest only
    /// once it bound [`Self::owner_domain`] to the home nest's verified key
    /// (`docs/goal/architecture/federation.md` § Cross-nest shared folders +
    /// channel append → *The cross-nest owner label*). The member refreshes
    /// its stored `ForeignFolder.owner_handle` from it, so a renamed owner
    /// relabels on the commit-poll cadence. Both-or-neither with
    /// [`Self::owner_domain`]; `None` same-nest or unverified ⇒ keep what is
    /// held. Display-only — never a lookup key or an authorization input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<String>,
    /// **Cross-nest relay only** — the handle domain [`Self::owner_handle`] is
    /// joined with at display (`alice@example.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_domain: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A request addressed to one folder set by its user-chosen name that also
/// carries the set's hash address (`name_hash`, S5b) — the address that
/// survives once the nest stops resting the plaintext name. The client stamps
/// the hash through one funnel ([`addressed`]), which also takes the plaintext
/// name off the request, so no such request leaves an app by name.
pub trait SetAddressed {
    /// The set's name as this request carries it.
    fn set_name(&self) -> &str;
    /// The request's hash address, if stamped.
    fn name_hash(&self) -> Option<&ByteBuf>;
    /// The request's `name_hash` slot.
    fn name_hash_mut(&mut self) -> &mut Option<ByteBuf>;
    /// Take the plaintext name off the request (an empty name is omitted on
    /// the wire).
    fn clear_set_name(&mut self);
    /// Whether this request must still carry the plaintext name beside the
    /// hash. Only two do: a create the nest must rest the name for (an
    /// unsealed or a `public` one) and an update that turns the set `public`,
    /// whose name the nest restores as the URL segment.
    fn keeps_set_name(&self) -> bool {
        false
    }
    /// Whether this request addresses the set named `name` — by its hash, or
    /// by the literal name a reserved `__` set (and an unaddressed request)
    /// still travels under. The one match a fake nest or a test makes.
    fn addresses(&self, name: &str) -> bool {
        if self.set_name() == name {
            return true;
        }
        let want = fauna_core::path_crypto::set_name_hash(name);
        self.name_hash().map(|h| &h[..]) == Some(&want[..])
    }
}

macro_rules! set_addressed {
    ($field:ident: $($t:ty),+ $(,)?) => {
        $(impl SetAddressed for $t {
            fn set_name(&self) -> &str {
                &self.$field
            }
            fn name_hash(&self) -> Option<&ByteBuf> {
                self.name_hash.as_ref()
            }
            fn name_hash_mut(&mut self) -> &mut Option<ByteBuf> {
                &mut self.name_hash
            }
            fn clear_set_name(&mut self) {
                self.$field.clear();
            }
        })+
    };
}

impl SetAddressed for FolderCreateRequest {
    fn set_name(&self) -> &str {
        &self.name
    }
    fn name_hash(&self) -> Option<&ByteBuf> {
        self.name_hash.as_ref()
    }
    fn name_hash_mut(&mut self) -> &mut Option<ByteBuf> {
        &mut self.name_hash
    }
    fn clear_set_name(&mut self) {
        self.name.clear();
    }
    fn keeps_set_name(&self) -> bool {
        self.name_sealed.is_none() || self.audience.as_deref() == Some("public")
    }
}

impl SetAddressed for FolderUpdateRequest {
    fn set_name(&self) -> &str {
        &self.name
    }
    fn name_hash(&self) -> Option<&ByteBuf> {
        self.name_hash.as_ref()
    }
    fn name_hash_mut(&mut self) -> &mut Option<ByteBuf> {
        &mut self.name_hash
    }
    fn clear_set_name(&mut self) {
        self.name.clear();
    }
    fn keeps_set_name(&self) -> bool {
        self.audience.as_deref() == Some("public")
    }
}

set_addressed!(name:
    FolderSetWebPaywallRequest,
    FolderDeleteRequest,
    FolderDevicesRequest,
    PlacesSetRequest,
    MembersListRequest,
    ActorMembersListRequest,
    MemberSetAccessRequest,
    MemberRemoveRequest,
    MemberEvictRequest,
    LeaseAcquireRequest,
    LeaseReleaseRequest,
    FolderShareRequest,
    ContentKeyPutRequest,
    ContentKeyGetRequest,
    ServedRowsAdoptRequest,
);
set_addressed!(folder:
    ConflictReportRequest,
    crate::sync::SyncChangeRecordRequest,
    crate::sync::SyncChangesSupersedeRequest,
    crate::sync::SyncStatusRequest,
    crate::sync::SyncFilesRequest,
    crate::filesync::SnapshotCreateFolderRequest,
    crate::filesync::SnapshotPruneRequest,
    crate::filesync::SnapshotPruneSetPolicyRequest,
    crate::filesync::SnapshotCheckRequest,
    crate::web::WebFilesPruneSealedRequest,
);

/// The optional-set reads: `None` names no set, so the funnel leaves the
/// request unaddressed (an empty name is never hashed).
macro_rules! optional_set_addressed {
    ($($t:ty),+ $(,)?) => {
        $(impl SetAddressed for $t {
            fn set_name(&self) -> &str {
                self.folder.as_deref().unwrap_or_default()
            }
            fn name_hash(&self) -> Option<&ByteBuf> {
                self.name_hash.as_ref()
            }
            fn name_hash_mut(&mut self) -> &mut Option<ByteBuf> {
                &mut self.name_hash
            }
            fn clear_set_name(&mut self) {
                self.folder = None;
            }
        })+
    };
}

optional_set_addressed!(
    crate::sync::SyncChangesListRequest,
    crate::filesync::SnapshotListRequest,
    crate::stats::StatsGetRequest,
    crate::files::FilesVersionsListRequest,
);

/// Address `req` by its set's hash (`name_hash = set_name_hash(name)`) and
/// take the plaintext name off it — the one funnel every by-name set request
/// leaves an app through, so the nest learns a set's name from no request but
/// an unsealed or `public` create or a `public` flip
/// ([`SetAddressed::keeps_set_name`];
/// `path-sealing.md` § the set-name plane). A caller-filled hash is kept; a
/// reserved `__` set is left by name, since the nest routes on those literal
/// names.
pub fn addressed<T: SetAddressed>(mut req: T) -> T {
    let name = req.set_name();
    if name.is_empty() || fauna_core::sync::is_reserved_folder_name(name) {
        return req;
    }
    let hash = fauna_core::path_crypto::set_name_hash(name);
    req.name_hash_mut()
        .get_or_insert_with(|| ByteBuf::from(hash.to_vec()));
    if !req.keeps_set_name() {
        req.clear_set_name();
    }
    req
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// The funnel stamps every by-name set request — the folders plane, the
    /// sync plane and the snapshot plane alike — and leaves the three shapes
    /// that name no hashable set by name: a reserved `__` set, an unnamed
    /// optional-set read, and a hash the caller already filled.
    #[test]
    fn addressed_stamps_the_set_hash_across_planes() {
        let want = Some(ByteBuf::from(
            fauna_core::path_crypto::set_name_hash("photos").to_vec(),
        ));
        let status = addressed(crate::sync::SyncStatusRequest {
            folder: "photos".into(),
            ..Default::default()
        });
        assert_eq!(status.name_hash, want);
        let check = addressed(crate::filesync::SnapshotCheckRequest {
            folder: "photos".into(),
            ..Default::default()
        });
        assert_eq!(check.name_hash, want);
        let list = addressed(crate::sync::SyncChangesListRequest {
            folder: Some("photos".into()),
            ..Default::default()
        });
        assert_eq!(list.name_hash, want);
        let remove = addressed(MemberRemoveRequest {
            name: "photos".into(),
            ..Default::default()
        });
        assert_eq!(remove.name_hash, want);
        let prune = addressed(crate::web::WebFilesPruneSealedRequest {
            folder: "photos".into(),
            ..Default::default()
        });
        assert_eq!(prune.name_hash, want);
        let stats = addressed(crate::stats::StatsGetRequest {
            folder: Some("photos".into()),
            ..Default::default()
        });
        assert_eq!(stats.name_hash, want);
        let versions = addressed(crate::files::FilesVersionsListRequest {
            folder: Some("photos".into()),
            ..Default::default()
        });
        assert_eq!(versions.name_hash, want);

        let unnamed = addressed(crate::filesync::SnapshotListRequest::default());
        assert_eq!(unnamed.name_hash, None);
        let reserved = addressed(crate::sync::SyncFilesRequest {
            folder: "__inbox".into(),
            ..Default::default()
        });
        assert_eq!(reserved.name_hash, None);
        let kept = addressed(crate::sync::SyncFilesRequest {
            folder: "photos".into(),
            name_hash: Some(ByteBuf::from(vec![7u8; 32])),
            ..Default::default()
        });
        assert_eq!(kept.name_hash, Some(ByteBuf::from(vec![7u8; 32])));
        assert_eq!(reserved.folder, "__inbox");
    }

    /// The funnel takes the plaintext name off every addressed request, so it
    /// is absent from the encoded bytes — except a create and a →`public`
    /// update, the two the nest needs the name for.
    #[test]
    fn addressed_takes_the_plaintext_name_off_the_wire() {
        fn carries_photos<T: Serialize>(v: &T) -> bool {
            let bytes = encode_canonical(v).unwrap();
            bytes.windows(6).any(|w| w == b"photos")
        }
        let remove = addressed(MemberRemoveRequest {
            name: "photos".into(),
            ..Default::default()
        });
        assert!(remove.name.is_empty());
        assert!(!carries_photos(&remove));
        let record = addressed(crate::sync::SyncFilesRequest {
            folder: "photos".into(),
            ..Default::default()
        });
        assert!(!carries_photos(&record));
        let versions = addressed(crate::files::FilesVersionsListRequest {
            folder: Some("photos".into()),
            ..Default::default()
        });
        assert_eq!(versions.folder, None);
        assert!(versions.name_hash.is_some());
        let settings = addressed(FolderUpdateRequest {
            name: "photos".into(),
            ..Default::default()
        });
        assert!(!carries_photos(&settings));

        let create = addressed(FolderCreateRequest {
            name: "photos".into(),
            ..Default::default()
        });
        assert_eq!(create.name, "photos", "an unsealed create rests its name");
        let sealed_create = addressed(FolderCreateRequest {
            name: "photos".into(),
            name_sealed: Some(ByteBuf::from(vec![0xa1u8; 24])),
            ..Default::default()
        });
        assert!(
            !carries_photos(&sealed_create),
            "a sealed create by hash alone"
        );
        assert!(sealed_create.name_hash.is_some());
        let public_create = addressed(FolderCreateRequest {
            name: "photos".into(),
            name_sealed: Some(ByteBuf::from(vec![0xa1u8; 24])),
            audience: Some("public".into()),
            ..Default::default()
        });
        assert_eq!(public_create.name, "photos", "a public name is its URL");
        let to_public = addressed(FolderUpdateRequest {
            name: "photos".into(),
            audience: Some("public".into()),
            ..Default::default()
        });
        assert_eq!(to_public.name, "photos");
        assert!(to_public.name_hash.is_some());
    }

    fn round_trip<T>(v: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bytes = encode_canonical(v).unwrap();
        let back: T = decode(&bytes).unwrap();
        assert_eq!(v, &back);
    }

    /// The default point a place-less device enrols at is all three flags;
    /// the archive point differs from it in `applies_deletes` alone.
    #[test]
    fn the_default_and_archive_points() {
        assert_eq!(PlaceFlags::default_place().point(), (true, true, true));
        assert_eq!(PlaceFlags::archive_place().point(), (true, true, false));
    }

    // ── MemberLeaveRequest::new — the one assembly point for `fauna.folders.leave` ──
    //
    // Two dispatchers issue this request (`fauna_client_folders`'s
    // `leave_with_home`, and `fauna-client-conversations`' folder-gate roster
    // drop, which cannot import that client without a package cycle). These pin
    // the shape they now share.

    #[test]
    fn member_leave_new_same_nest_carries_no_nest_url() {
        let req = MemberLeaveRequest::new("abc123", None::<String>);
        assert_eq!(req.group_id, "abc123");
        assert_eq!(req.nest_url, None);
        assert!(req.extra.is_empty(), "the forward-compat bag starts empty");
    }

    #[test]
    fn member_leave_new_foreign_set_carries_the_home_nest() {
        let req = MemberLeaveRequest::new("abc123", Some("https://home.example"));
        assert_eq!(req.group_id, "abc123");
        assert_eq!(req.nest_url.as_deref(), Some("https://home.example"));
    }

    /// The same-nest form must not put a `nest_url` key on the wire at all
    /// (`skip_serializing_if`), so the request carries no `nest_url` — the additive-evolution rule the
    /// field was added under.
    #[test]
    fn member_leave_new_same_nest_omits_nest_url_on_the_wire() {
        let same = MemberLeaveRequest::new("abc123", None::<String>);
        let foreign = MemberLeaveRequest::new("abc123", Some("https://home.example"));
        let same_bytes = encode_canonical(&same).unwrap();
        let foreign_bytes = encode_canonical(&foreign).unwrap();
        assert!(
            same_bytes.len() < foreign_bytes.len(),
            "the same-nest leave must omit nest_url, not send it empty"
        );
        round_trip(&same);
        round_trip(&foreign);
    }

    /// What the constructor exists to protect: it is byte-for-byte the literal
    /// both dispatchers used to hand-write, so adopting it changed no wire bytes.
    #[test]
    fn member_leave_new_matches_the_hand_written_literal_it_replaced() {
        let built = MemberLeaveRequest::new("abc123", Some("https://home.example"));
        let hand_written = MemberLeaveRequest {
            group_id: "abc123".to_string(),
            nest_url: Some("https://home.example".to_string()),
            ..Default::default()
        };
        assert_eq!(built, hand_written);
        assert_eq!(
            encode_canonical(&built).unwrap(),
            encode_canonical(&hand_written).unwrap()
        );
    }

    #[test]
    fn create_round_trips() {
        round_trip(&FolderCreateRequest {
            name: "photos".into(),
            name_hash: Some(ByteBuf::from(vec![7u8; 32])),
            retention_policy: Some(r#"{"max_snapshots":7,"max_age_days":30}"#.into()),
            ..Default::default()
        });
        round_trip(&FolderCreateReply {
            id: 42,
            name: "photos".into(),
            retention_policy: None,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn list_reply_round_trips() {
        round_trip(&FoldersListRequest {
            include_shared_with_me: Some(true),
            extra: BTreeMap::new(),
        });
        round_trip(&FoldersListReply {
            folders: vec![
                // An owned row.
                FolderSummary {
                    id: 1,
                    name: "docs".into(),
                    retention_policy: None,
                    cached_snapshot_count: 3,
                    cached_total_bytes: 1024,
                    cached_last_snapshot_at: Some(1_700_000_000),
                    include_paths: Some(vec!["/docs".into()]),
                    exclude_paths: None,
                    mls_group_id: Some("aa".repeat(16)),
                    role: Some("owner".into()),
                    // Owner rows carry no member-access grant.
                    access: None,
                    owner_handle: None,
                    // Owner's own row: no "shared by" badge, so no owner actor id.
                    owner_actor_id: None,
                    webdav_enabled: false,
                    conflict_policy: Some("latest_wins_always".into()),
                    web_paywall_tier: Some("gold".into()),
                    // The sealed name and its salt ride together — a seal
                    // without the salt is unrenderable post-scrub.
                    name_sealed: Some(ByteBuf::from(vec![0xa1u8; 40])),
                    name_hash: Some(ByteBuf::from(vec![0xb2u8; 32])),
                    // The owner-only selective-sync seals (S6-c) — no salt
                    // companion: they open under this row's own `id`.
                    include_paths_sealed: Some(ByteBuf::from(vec![0xc3u8; 44])),
                    exclude_paths_sealed: Some(ByteBuf::from(vec![0xc4u8; 44])),
                    // Struct-update form for the rest, so a future field growing
                    // on this summary does not break the literal again — as
                    // S6-e's `retention_policy_sealed` did. The `Default` derive
                    // above the struct exists for this.
                    ..Default::default()
                },
                // A member (shared-with-me) row: owner handle set, local paths
                // withheld (B3).
                FolderSummary {
                    id: 7,
                    name: "family-photos".into(),
                    retention_policy: None,
                    cached_snapshot_count: 0,
                    cached_total_bytes: 0,
                    cached_last_snapshot_at: None,
                    include_paths: None,
                    exclude_paths: None,
                    mls_group_id: Some("bb".repeat(16)),
                    role: Some("member".into()),
                    // A writer grant rides the summary so the recipient's client
                    // can offer folder binding (multi-writer Phase 1).
                    access: Some("writer".into()),
                    owner_handle: Some("alice".into()),
                    // Member row: the owner id rides so the recipient badge can
                    // fall back to `short_id` when the handle is unresolved.
                    owner_actor_id: Some("a1".repeat(32)),
                    webdav_enabled: false,
                    // conflict_policy absent on the wire (the column default).
                    conflict_policy: None,
                    web_paywall_tier: None,
                    // Unsealed arm: neither the seal nor its salt on the wire.
                    // Covered here so the additive fields round-trip absent as
                    // well as present (the owner row above carries both).
                    name_sealed: None,
                    name_hash: None,
                    // Withheld from a member on purpose:
                    // this row's plaintext `include_paths`/`exclude_paths` are
                    // `None` above for the same reason, and the seal follows the
                    // field it seals, not the `name_sealed` precedent two lines
                    // up. The nest projection pins this; here it is the wire's
                    // absent arm.
                    include_paths_sealed: None,
                    exclude_paths_sealed: None,
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    /// The `owner_actor_id` enrichment is additive-everywhere within a major
    /// (`version-compatibility.md`): both decode directions survive.
    #[test]
    fn owner_actor_id_is_wire_additive() {
        // A stand-in for a decoder that lacks the field: the same catch-all
        // `extra` every wire struct carries, but no `owner_actor_id` field.
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct LegacyRow {
            id: i64,
            name: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            owner_handle: Option<String>,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }

        // New nest → old client: the new key lands in `extra`, decode does not
        // fail (forward compat).
        let new_row = FolderSummary {
            id: 7,
            name: "family-photos".into(),
            role: Some("member".into()),
            owner_handle: Some("alice".into()),
            owner_actor_id: Some("a1".repeat(32)),
            ..Default::default()
        };
        let bytes = encode_canonical(&new_row).unwrap();
        let legacy: LegacyRow = decode(&bytes).unwrap();
        assert_eq!(legacy.owner_handle.as_deref(), Some("alice"));
        assert!(legacy.extra.contains_key("owner_actor_id"));

        // Old nest → new client: a summary with `owner_actor_id` unset is
        // byte-identical to old wire (`skip_serializing_if` omits the key), and
        // the new decoder defaults the field to `None` (backward compat).
        let old_wire = FolderSummary {
            id: 7,
            name: "family-photos".into(),
            role: Some("member".into()),
            owner_handle: Some("alice".into()),
            owner_actor_id: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&old_wire).unwrap();
        // No `owner_actor_id` key rides when unset — so an old-shape reader sees
        // exactly the pre-enrichment bytes.
        let as_legacy: LegacyRow = decode(&bytes).unwrap();
        assert!(!as_legacy.extra.contains_key("owner_actor_id"));
        let migrated: FolderSummary = decode(&bytes).unwrap();
        assert_eq!(migrated.owner_actor_id, None);
        assert_eq!(migrated.owner_handle.as_deref(), Some("alice"));
    }

    #[test]
    fn update_round_trips() {
        round_trip(&FolderUpdateRequest {
            name: "docs".into(),
            retention_policy: None,
            include_paths: None,
            exclude_paths: Some(vec![".git".into()]),
            webdav_enabled: None,
            conflict_policy: Some("latest_wins_always".into()),
            name_sealed: Some(ByteBuf::from(vec![0xc3u8; 40])),
            ..Default::default()
        });
        round_trip(&FolderUpdateReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn set_web_paywall_round_trips() {
        round_trip(&FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: Some("gold".into()),
            ..Default::default()
        });
        // The clear arm: tier omitted from the wire entirely.
        round_trip(&FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: None,
            ..Default::default()
        });
        round_trip(&FolderSetWebPaywallReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn members_devices_round_trip() {
        round_trip(&FolderDevicesReply {
            devices: vec![FolderDevice {
                device_id: "aa".repeat(32),
                label: "laptop".into(),
                last_change_at: 1_700_000_000,
                change_count: 5,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        });
        round_trip(&MembersListReply {
            members: vec![FolderMember {
                device_id: "cc".repeat(32),
                label: "phone".into(),
                flags: PlaceFlags::new(true, false, false),
                ..Default::default()
            }],
            extra: BTreeMap::new(),
        });
        round_trip(&ActorMembersListReply {
            members: vec![
                FolderActorMember {
                    actor_id: "a1".repeat(32),
                    handle: "alice".into(),
                    role: "owner".into(),
                    ..Default::default()
                },
                FolderActorMember {
                    actor_id: "b2".repeat(32),
                    handle: String::new(),
                    role: "member".into(),
                    ..Default::default()
                },
            ],
            caller_access: Some("writer".into()),
            residency: Some("full".into()),
            extra: BTreeMap::new(),
        });
        round_trip(&ActorMembersListRemoteRequest {
            channel_id: "cd".repeat(32),
            nest_url: "https://home.example".into(),
            ..Default::default()
        });
    }

    #[test]
    fn lease_round_trip() {
        round_trip(&LeaseAcquireReply {
            acquired: true,
            extra: BTreeMap::new(),
        });
        round_trip(&LeaseReleaseReply {
            released: true,
            extra: BTreeMap::new(),
        });
    }

    /// `path_hash` is required: a conflict row that does not carry its
    /// address is refused at decode (the optional form served only a nest
    /// predating the sealed-label expand — the compat-remnant sweep).
    #[test]
    fn a_conflict_without_a_path_hash_is_refused_at_decode() {
        let full = SyncConflict {
            id: 7,
            folder: "docs".into(),
            path_hash: ByteBuf::from(vec![9; 32]),
            ..Default::default()
        };
        let mut map: BTreeMap<String, Value> = decode(&encode_canonical(&full).unwrap()).unwrap();
        assert!(map.remove("path_hash").is_some());
        let bytes = encode_canonical(&map).unwrap();
        assert!(
            decode::<SyncConflict>(&bytes).is_err(),
            "a conflict without `path_hash` must not decode"
        );
    }

    #[test]
    fn conflicts_round_trip() {
        // Unresolved mark-only report (no auto-resolve fields on the wire;
        // skip_serializing_if keeps them absent).
        round_trip(&ConflictReportRequest {
            folder: "docs".into(),
            device_id: "dd".repeat(32),
            path: "/docs/a.txt".into(),
            conflict_type: "concurrent_edit".into(),
            details: Some("two devices".into()),
            candidates: vec![ConflictCandidate {
                manifest_hash: "aa".repeat(32),
                device_id: "dd".repeat(32),
                size_bytes: 4096,
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        // Pre-resolved report — the auto-resolve shape (ratified 2026-07-10):
        // the detecting device reports the conflict already resolved.
        round_trip(&ConflictReportRequest {
            folder: "docs".into(),
            device_id: "dd".repeat(32),
            path: "/docs/a.txt".into(),
            conflict_type: "concurrent_edit".into(),
            details: None,
            candidates: vec![ConflictCandidate {
                manifest_hash: "aa".repeat(32),
                device_id: "dd".repeat(32),
                size_bytes: 4096,
                created_at: 1_700_000_000,
                content_key_version: Some(3),
                ..Default::default()
            }],
            resolution: Some("merged".into()),
            winning_manifest_hash: Some("cc".repeat(32)),
            ..Default::default()
        });
        round_trip(&ConflictsListRequest {
            include_resolved: Some(true),
            ..Default::default()
        });
        round_trip(&ConflictsListReply {
            conflicts: vec![
                // Unresolved row.
                SyncConflict {
                    id: 7,
                    folder: "docs".into(),
                    device_id: "ee".repeat(32),
                    path: "/docs/a.txt".into(),
                    conflict_type: "concurrent_edit".into(),
                    details: None,
                    created_at: 1_700_000_000,
                    candidates: vec![
                        ConflictCandidate {
                            manifest_hash: "aa".repeat(32),
                            device_id: "dd".repeat(32),
                            size_bytes: 4096,
                            created_at: 1_700_000_000,
                            ..Default::default()
                        },
                        ConflictCandidate {
                            manifest_hash: "bb".repeat(32),
                            device_id: "ee".repeat(32),
                            size_bytes: 5120,
                            created_at: 1_700_000_050,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                // Auto-resolved row (the review-list surface).
                SyncConflict {
                    id: 8,
                    folder: "docs".into(),
                    device_id: "ee".repeat(32),
                    path: "/docs/b.txt".into(),
                    conflict_type: "concurrent_edit".into(),
                    details: None,
                    created_at: 1_700_000_100,
                    candidates: vec![ConflictCandidate {
                        manifest_hash: "aa".repeat(32),
                        device_id: "dd".repeat(32),
                        size_bytes: 4096,
                        created_at: 1_700_000_100,
                        content_key_version: Some(2),
                        ..Default::default()
                    }],
                    resolved_at: Some(1_700_000_101),
                    resolution: Some("latest_wins".into()),
                    winning_manifest_hash: Some("bb".repeat(32)),
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        });
        round_trip(&ConflictResolveRequest {
            id: 7,
            winning_manifest_hash: Some("bb".repeat(32)),
            winner_signature: Some(ByteBuf::from(vec![0x5a; 64])),
            winner_signer_key: Some(ByteBuf::from(vec![0xa5; 32])),
            extra: BTreeMap::new(),
        });
        round_trip(&ConflictResolveReply {
            resolved: true,
            winning_manifest_hash: Some("bb".repeat(32)),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn share_round_trips() {
        round_trip(&FolderShareRequest {
            name: "photos".into(),
            // Raw MLS group id — variable length (not fixed-32); 16 bytes here.
            group_id: "ab".repeat(16),
            ..Default::default()
        });
        round_trip(&FolderShareReply {
            ok: true,
            folder: "photos".into(),
            name_hash: Some(ByteBuf::from(vec![9u8; 32])),
            // 32-byte derived ChannelId.
            channel_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn member_leave_round_trips() {
        round_trip(&MemberLeaveRequest {
            // Raw MLS group id — variable length (not fixed-32); 16 bytes here.
            group_id: "ab".repeat(16),
            ..Default::default()
        });
        round_trip(&MemberLeaveReply {
            ok: true,
            // 32-byte derived ChannelId.
            channel_id: "cd".repeat(32),
            left: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn content_key_round_trips() {
        round_trip(&ContentKeyPutRequest {
            name: "photos".into(),
            epoch: 7,
            sealed: "ab".repeat(48),
            current_version: 3,
            ..Default::default()
        });
        round_trip(&ContentKeyPutReply {
            ok: true,
            channel_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        });
        round_trip(&ContentKeyGetRequest {
            name: "photos".into(),
            ..Default::default()
        });
        round_trip(&ContentKeyGetReply {
            epoch: 7,
            sealed: "ab".repeat(48),
            caller_access: Some("writer".into()),
            home_nest_actor_id: Some("cd".repeat(32)),
            content_key_floor: Some(3),
            residency: Some("metadata_only".into()),
            owner_handle: Some("alice".into()),
            owner_domain: Some("example.com".into()),
            extra: BTreeMap::new(),
        });
    }

    /// The cross-nest owner label on the content-key reply is additive within
    /// a major (`version-compatibility.md`): an older relaying nest or client
    /// decodes a stamped reply (the pair lands in `extra`), and an unstamped
    /// reply is byte-identical to the old wire, decoding to `None` — "keep
    /// what is held", never a cleared name.
    #[test]
    fn content_key_reply_owner_label_is_wire_additive() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct LegacyReply {
            epoch: i64,
            sealed: String,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }
        let stamped = ContentKeyGetReply {
            epoch: 4,
            sealed: "ab".repeat(8),
            owner_handle: Some("alice".into()),
            owner_domain: Some("example.com".into()),
            ..Default::default()
        };
        let legacy: LegacyReply = decode(&encode_canonical(&stamped).unwrap()).unwrap();
        assert!(legacy.extra.contains_key("owner_handle"));
        assert!(legacy.extra.contains_key("owner_domain"));

        let unstamped = ContentKeyGetReply {
            epoch: 4,
            sealed: "ab".repeat(8),
            ..Default::default()
        };
        let bytes = encode_canonical(&unstamped).unwrap();
        let old: LegacyReply = decode(&bytes).unwrap();
        assert!(old.extra.is_empty(), "no owner keys ride when unset");
        let back: ContentKeyGetReply = decode(&encode_canonical(&old).unwrap()).unwrap();
        assert_eq!(back.owner_handle, None);
        assert_eq!(back.owner_domain, None);
    }

    #[test]
    fn member_set_access_round_trips() {
        round_trip(&MemberSetAccessRequest {
            name: "photos".into(),
            actor_id: "bb".repeat(32),
            access: "writer".into(),
            byte_cap: Some(1_048_576),
            ..Default::default()
        });
        // Uncapped grant — the ratified blank-cap-means-uncapped shape.
        round_trip(&MemberSetAccessRequest {
            name: "photos".into(),
            actor_id: "bb".repeat(32),
            access: "reader".into(),
            ..Default::default()
        });
        round_trip(&MemberSetAccessReply {
            ok: true,
            extra: BTreeMap::new(),
        });
        // A share carrying the D5 share-time grant.
        round_trip(&FolderShareRequest {
            name: "photos".into(),
            group_id: "ab".repeat(8),
            member_actor_id: Some("bb".repeat(32)),
            access: Some("writer".into()),
            ..Default::default()
        });
        // A member row carrying the Phase-1 enrichment.
        round_trip(&FolderActorMember {
            actor_id: "bb".repeat(32),
            handle: "bob".into(),
            role: "member".into(),
            access: Some("writer".into()),
            byte_cap: Some(4096),
            bytes_used: Some(1024),
            ..Default::default()
        });
    }

    // ── Place flags (folders re-model phase 2) ─────────────────────────────

    #[test]
    fn place_flags_round_trip_on_the_wire() {
        round_trip(&PlaceFlags {
            originates: true,
            accepts: false,
            applies_deletes: false,
            ..Default::default()
        });
    }

    // ── The flags on the wire ──────────────────────────────────────────────

    /// A member row's flags are required, and a `role` key from a nest that
    /// still spells one rides rule-4 `extra` rather than failing the decode —
    /// the role contraction's wire posture (`folders.md` § Implementation
    /// status today).
    #[test]
    fn a_member_row_carries_its_flags_and_a_stray_role_rides_extra() {
        let row = FolderMember {
            device_id: "bb".repeat(32),
            label: "phone".into(),
            flags: PlaceFlags::archive_place(),
            extra: BTreeMap::from([("role".to_string(), Value::String("backup".into()))]),
        };
        round_trip(&row);
        let back: FolderMember = decode(&encode_canonical(&row).unwrap()).unwrap();
        assert_eq!(back.flags.point(), (true, true, false));
        assert!(
            back.extra.contains_key("role"),
            "the stray key survives, ignored"
        );
    }

    /// A row with no `flags` key does not decode: the flags are the whole
    /// place, so there is nothing to default them to.
    #[test]
    fn a_member_row_without_flags_is_refused() {
        #[derive(Serialize)]
        struct RoleOnly {
            device_id: String,
            label: String,
            role: String,
        }
        let bytes = encode_canonical(&RoleOnly {
            device_id: "aa".repeat(32),
            label: "laptop".into(),
            role: "sync".into(),
        })
        .unwrap();
        assert!(decode::<FolderMember>(&bytes).is_err());
    }

    #[test]
    fn places_set_round_trips() {
        round_trip(&PlacesSetRequest {
            name: "photos".into(),
            device_id: "dd".repeat(32),
            flags: PlaceFlags::default_place(),
            ..Default::default()
        });
        round_trip(&PlacesSetRequest {
            name: String::new(),
            name_hash: Some(ByteBuf::from(vec![7u8; 32])),
            device_id: "dd".repeat(32),
            flags: PlaceFlags::new(true, false, false),
            ..Default::default()
        });
        round_trip(&PlacesSetReply {
            ok: true,
            folder: "photos".into(),
            device_id: "dd".repeat(32),
            flags: PlaceFlags::default_place(),
            ..Default::default()
        });
    }

    /// The claim half of the rule, in the direction that matters: **only** an
    /// explicit `public` audience can arm the verdict at all — and even a
    /// genuine owner attestation arms nothing under any other token. The empty
    /// string an audience-less nest sends, and any token this build cannot
    /// read, seal.
    #[test]
    fn only_an_explicit_public_audience_can_arm_the_verdict() {
        let genuine = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 1_000, None);
        let armed = public_row(Some(genuine.clone()));
        assert!(judge(&armed, AttestationMemory::default()).0);

        for sealed in [
            AUDIENCE_PRIVATE,
            AUDIENCE_SHARED,
            "",
            "Public",
            "public ",
            "world_readable",
        ] {
            let mut fs = public_row(Some(genuine.clone()));
            fs.audience = sealed.into();
            assert!(
                !judge(&fs, AttestationMemory::default()).0,
                "audience {sealed:?} must seal — nothing unparseable may resolve to unsealed"
            );
        }
    }

    /// The WebDAV fail-safe. The nest refuses `public` + served from both sides
    /// (`folders.md` § Target re-model, phase 4), so this state cannot arrive
    /// legitimately; if a corrupt projection carries it anyway, **serving wins
    /// and the content stays sealed** — a genuine attestation notwithstanding.
    /// Pinned because the conjunct reads like a second question and is the
    /// half a reader re-deriving the rule would drop.
    #[test]
    fn a_webdav_served_folder_never_arms_even_when_public_and_attested() {
        let mut corrupt = public_row(Some(AudienceAttestation::mint(
            &owner(),
            SITE_ID,
            SITE,
            1_000,
            None,
        )));
        corrupt.webdav_enabled = true;
        assert!(
            !judge(&corrupt, AttestationMemory::default()).0,
            "the nest-refused (public + served) combination must fail CLOSED"
        );
    }

    /// A list the seat read that carries **no row** for the folder is a sealed
    /// verdict that burns: a deleted or un-shared set is not world-readable,
    /// and the counter the seat was armed under is withdrawn with it. A row
    /// that is present judges exactly as [`FolderSummary::judge_declassification`].
    #[test]
    fn a_folder_absent_from_a_read_list_seals_and_burns() {
        let armed = AttestationMemory {
            floor: 1_000,
            armed: Some(1_000),
        };
        let (unsealed, memory) = FolderSummary::judge_listed_declassification(
            None,
            SITE,
            Some(&owner().actor_id()),
            armed,
        );
        assert!(!unsealed);
        assert_eq!(memory, armed.observe_sealed(), "the absent row burns");

        let row = public_row(Some(AudienceAttestation::mint(
            &owner(),
            SITE_ID,
            SITE,
            1_000,
            None,
        )));
        assert_eq!(
            FolderSummary::judge_listed_declassification(
                Some(&row),
                SITE,
                Some(&owner().actor_id()),
                AttestationMemory::default()
            ),
            judge(&row, AttestationMemory::default()),
            "a present row is the row's own verdict"
        );
    }

    // ── the owner-attested declassification ────────────────────────────────

    const SITE_ID: i64 = 41;
    const SITE: &str = "site";

    fn owner() -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret([7; 32])
    }

    /// A row the way an honest nest serves a folder its owner made public.
    fn public_row(att: Option<AudienceAttestation>) -> FolderSummary {
        FolderSummary {
            id: SITE_ID,
            name: SITE.into(),
            audience: AUDIENCE_PUBLIC.into(),
            audience_attestation: att,
            ..Default::default()
        }
    }

    fn judge(row: &FolderSummary, memory: AttestationMemory) -> (bool, AttestationMemory) {
        row.judge_declassification(SITE, Some(&owner().actor_id()), memory)
    }

    /// The finding itself: a nest reporting `public` with nothing the owner
    /// signed, or with something it made up, arms nothing.
    #[test]
    fn a_public_claim_without_a_genuine_attestation_seals() {
        let none = public_row(None);
        assert!(!judge(&none, AttestationMemory::default()).0, "bare claim");

        let genuine = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 1_000, None);

        let mut bad_sig = genuine.clone();
        bad_sig.sig = ByteBuf::from(vec![0u8; 64]);
        let mut bumped = genuine.clone();
        bumped.counter += 1;
        let mut short = genuine.clone();
        short.sig = ByteBuf::from(vec![1u8; 10]);
        let nest = fauna_core::identity::ActorKeypair::from_secret([9; 32]);
        let self_signed = AudienceAttestation::mint(&nest, SITE_ID, SITE, 1_000, None);
        // The sharper forgery: the nest signs the message that names the REAL
        // owner, with its own key, and carries its own key in `owner` — what a
        // verifier that took the key from the attestation would accept.
        let impersonating = {
            use ed25519_dalek::Signer as _;
            let message =
                audience_attestation_signed_message(&owner().actor_id().0, SITE_ID, SITE, 1_000);
            AudienceAttestation {
                owner: ByteBuf::from(nest.actor_id().0.to_vec()),
                folder_id: SITE_ID,
                counter: 1_000,
                sig: ByteBuf::from(nest.signing_key().sign(&message).to_bytes().to_vec()),
            }
        };
        let other_folder = AudienceAttestation::mint(&owner(), SITE_ID + 1, SITE, 1_000, None);
        let other_name = AudienceAttestation::mint(&owner(), SITE_ID, "holiday", 1_000, None);
        // The nest relabels another folder's genuine attestation as this one's.
        let mut relabelled = other_folder.clone();
        relabelled.folder_id = SITE_ID;

        for (why, att) in [
            ("zeroed signature", bad_sig),
            ("counter edited after signing", bumped),
            ("truncated signature", short),
            (
                "signed by someone other than the trusted owner",
                self_signed,
            ),
            (
                "the nest's key carried in `owner` over the real owner's message",
                impersonating,
            ),
            (
                "the owner's attestation for a different folder id",
                other_folder,
            ),
            (
                "the owner's attestation for a different folder name",
                other_name,
            ),
            (
                "a different folder's attestation with the id rewritten",
                relabelled,
            ),
        ] {
            let (verdict, _) = judge(&public_row(Some(att)), AttestationMemory::default());
            assert!(!verdict, "{why} must seal");
        }
    }

    /// A member seat with no MLS-recorded owner has nothing to verify against.
    #[test]
    fn no_trusted_owner_seals_even_a_genuine_attestation() {
        let row = public_row(Some(AudienceAttestation::mint(
            &owner(),
            SITE_ID,
            SITE,
            1,
            None,
        )));
        assert!(
            !row.judge_declassification(SITE, None, AttestationMemory::default())
                .0
        );
    }

    /// The honest path still works — and only while the row still claims it:
    /// the nest keeps serving the attestation after a flip-back, inert.
    #[test]
    fn a_genuine_attestation_arms_only_a_row_that_claims_public() {
        let att = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 1_000, None);
        let (verdict, memory) = judge(&public_row(Some(att.clone())), AttestationMemory::default());
        assert!(verdict);
        assert_eq!(
            memory,
            AttestationMemory {
                floor: 1_000,
                armed: Some(1_000)
            }
        );

        let mut flipped_back = public_row(Some(att.clone()));
        flipped_back.audience = AUDIENCE_PRIVATE.into();
        assert!(!judge(&flipped_back, AttestationMemory::default()).0);

        let mut served = public_row(Some(att));
        served.webdav_enabled = true;
        assert!(
            !judge(&served, AttestationMemory::default()).0,
            "WebDAV fail-safe"
        );
    }

    /// The owner's re-confirm surface asks this: the nest says public, and the
    /// seat's own verdict says sealed — every case the verifier seals, and only
    /// while the row CLAIMS public.
    #[test]
    fn public_unverified_holds_exactly_where_the_claim_fails_to_verify() {
        let me = owner().actor_id();
        let genuine = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 1_000, None);
        assert!(
            !public_row(Some(genuine.clone())).is_public_unverified_for(&me),
            "the owner's own genuine attestation verifies — nothing to re-confirm"
        );

        let predecessor = fauna_core::identity::ActorKeypair::from_secret([8; 32]);
        let mut forged = genuine.clone();
        forged.sig = ByteBuf::from(vec![0u8; 64]);
        for (why, att) in [
            ("absent — declassified before attestations existed", None),
            ("forged", Some(forged)),
            (
                "signed by a predecessor owner",
                Some(AudienceAttestation::mint(
                    &predecessor,
                    SITE_ID,
                    SITE,
                    1_000,
                    None,
                )),
            ),
        ] {
            assert!(
                public_row(att).is_public_unverified_for(&me),
                "{why} must read public-and-unverified"
            );
        }

        // Nothing is claimed, so nothing is owed: a private row, and the
        // nest-refused (public + WebDAV-served) fail-safe state, which already
        // rests sealed by rule rather than for want of a signature.
        let mut private = public_row(None);
        private.audience = AUDIENCE_PRIVATE.into();
        assert!(!private.is_public_unverified_for(&me));
        let mut served = public_row(None);
        served.webdav_enabled = true;
        assert!(
            !served.is_public_unverified_for(&me),
            "the WebDAV fail-safe state must never paint the re-confirm status"
        );
    }

    /// The row's own name is the acting name: a rename on the nest is a
    /// different folder name, and the old attestation no longer covers it.
    #[test]
    fn public_unverified_reads_the_rows_own_name() {
        let me = owner().actor_id();
        let mut renamed = public_row(Some(AudienceAttestation::mint(
            &owner(),
            SITE_ID,
            SITE,
            1_000,
            None,
        )));
        renamed.name = "holiday".into();
        assert!(renamed.is_public_unverified_for(&me));
    }

    /// Replay: the seat saw the folder stop being public, so the attestation it
    /// was armed under never re-arms it — but the owner's next flip does, even
    /// minted on a device whose clock is behind the first one's.
    #[test]
    fn a_withdrawn_attestation_cannot_be_replayed_but_a_re_flip_arms() {
        let first = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 5_000, None);
        let (_, memory) = judge(
            &public_row(Some(first.clone())),
            AttestationMemory::default(),
        );

        let mut flipped_back = public_row(Some(first.clone()));
        flipped_back.audience = AUDIENCE_PRIVATE.into();
        let (_, memory) = judge(&flipped_back, memory);
        assert_eq!(
            memory,
            AttestationMemory {
                floor: 5_001,
                armed: None
            }
        );

        let (replayed, memory) = judge(&public_row(Some(first.clone())), memory);
        assert!(!replayed, "the burned counter must not re-arm the seat");

        let slow_clock = 10;
        let second = AudienceAttestation::mint(&owner(), SITE_ID, SITE, slow_clock, Some(&first));
        assert_eq!(second.counter, 5_001);
        let (re_armed, memory) = judge(&public_row(Some(second)), memory);
        assert!(re_armed);

        // …and once armed under the second, the first is below the floor.
        let (old_again, _) = judge(&public_row(Some(first)), memory);
        assert!(!old_again);
    }

    /// A folder absent from a list the seat read burns the counter too; the
    /// meta-row encoding round-trips; a corrupt row fails closed.
    #[test]
    fn attestation_memory_burns_and_persists_fail_closed() {
        let armed = AttestationMemory {
            floor: 3,
            armed: Some(8),
        };
        assert_eq!(
            armed.observe_sealed(),
            AttestationMemory {
                floor: 9,
                armed: None
            }
        );
        let idle = AttestationMemory {
            floor: 3,
            armed: None,
        };
        assert_eq!(idle.observe_sealed(), idle);

        for m in [armed, idle, AttestationMemory::default()] {
            assert_eq!(AttestationMemory::from_meta(Some(&m.to_meta())), m);
        }
        assert_eq!(
            AttestationMemory::from_meta(None),
            AttestationMemory::default()
        );

        let att = AudienceAttestation::mint(&owner(), SITE_ID, SITE, u64::MAX - 1, None);
        for corrupt in ["", "7", "x:-", "7:y", "-1:-"] {
            let memory = AttestationMemory::from_meta(Some(corrupt));
            assert!(
                !judge(&public_row(Some(att.clone())), memory).0,
                "corrupt memory row {corrupt:?} must seal"
            );
        }
    }

    /// Wire-additive both ways: a row without the key decodes with no attestation, and the
    /// attested row and update round-trip.
    #[test]
    fn the_attestation_rides_the_wire_additively() {
        let att = AudienceAttestation::mint(&owner(), SITE_ID, SITE, 1_000, None);
        round_trip(&public_row(Some(att.clone())));
        round_trip(&FolderUpdateRequest {
            name: SITE.into(),
            audience: Some(AUDIENCE_PUBLIC.into()),
            audience_attestation: Some(att),
            ..Default::default()
        });
        let old = encode_canonical(&public_row(None)).unwrap();
        let decoded: FolderSummary = decode(&old).unwrap();
        assert_eq!(decoded.audience_attestation, None);
    }

    /// Residency fails closed to FULL: only the explicit, parsed opt-in may stop
    /// bytes resting (`file-sync.md` § Content residency).
    #[test]
    fn residency_fails_closed_to_full() {
        let opted_out = FolderSummary {
            residency: RESIDENCY_METADATA_ONLY.into(),
            ..Default::default()
        };
        assert!(opted_out.is_metadata_only());

        for full in ["", "full", "metadata", "MetadataOnly", "metadata_only "] {
            let fs = FolderSummary {
                residency: full.into(),
                ..Default::default()
            };
            assert!(
                !fs.is_metadata_only(),
                "residency {full:?} must upload as always — fail-closed to full"
            );
        }
    }

    /// `Default` — the shape every fixture and every absent-key row
    /// decodes to — is sealed and full on both axes.
    #[test]
    fn a_default_summary_is_sealed_and_full() {
        let fs = FolderSummary::default();
        assert!(
            !fs.judge_declassification(
                &fs.name,
                Some(&owner().actor_id()),
                AttestationMemory::default()
            )
            .0
        );
        assert!(!fs.is_metadata_only());
    }
}

/// What a **successful** roster read found out about this device's seat.
///
/// Two cases: the roster carries a row for this device, whose required
/// [`FolderMember::flags`] say what the seat does, or it carries none. A row
/// can no longer be unreadable — the legacy `role` fallback and the
/// `Unreadable` arm it fed retired with the role contraction; a newer nest's
/// future vocabulary rides the flags' rule-4 `extra`, which a reader ignores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatRead {
    /// The roster carries no row for this device.
    Absent,
    /// The roster says what this seat does.
    Flags(PlaceFlags),
}

impl SeatRead {
    /// Read one roster row.
    pub fn from_member(member: &FolderMember) -> Self {
        Self::Flags(member.flags.clone())
    }

    /// Read this device's seat out of a whole roster reply.
    pub fn find(members: &[FolderMember], device_id_hex: &str) -> Self {
        members
            .iter()
            .find(|m| m.device_id == device_id_hex)
            .map_or(Self::Absent, Self::from_member)
    }

    /// Whether this seat makes the device a **delivery seat for an on-demand
    /// presence** (`on-demand-files.md` § Apple File Provider binding: desired =
    /// the own folders where this device's place has `accepts: true`). Only a
    /// read place with `accepts` set answers yes: an **Absent** seat gets no
    /// presence — the toggle there is the enrol gesture, which writes the place
    /// first.
    ///
    /// Deliberately NOT the engine's `accepts_from_seat`, where an Absent seat
    /// accepts (a shared-set member with no row is a reader by default): a
    /// presence is a device-local surface the user switches on per place.
    pub fn delivers_presence(&self) -> bool {
        matches!(self, Self::Flags(flags) if flags.accepts)
    }
}

#[cfg(test)]
mod seat_read_presence_tests {
    use super::*;

    fn member(device_id: &str, flags: PlaceFlags) -> FolderMember {
        FolderMember {
            device_id: device_id.into(),
            flags,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_read_accepting_place_delivers_a_presence() {
        let accepting = PlaceFlags {
            accepts: true,
            ..Default::default()
        };
        let closed = PlaceFlags {
            originates: true,
            ..Default::default()
        };
        let roster = vec![member("aa", accepting), member("bb", closed)];
        assert!(SeatRead::find(&roster, "aa").delivers_presence());
        assert!(!SeatRead::find(&roster, "bb").delivers_presence());
        assert!(
            !SeatRead::find(&roster, "dd").delivers_presence(),
            "a place-less device gets no presence — the toggle enrols it first"
        );
    }
}

// ── The device-place editor's projection ──────────────────────────────────
//
// The post-create **device-place editor** — one enrolled seat's roster row ⇄
// the three flag checkboxes an app paints under an expanded `folder-row`.
//
// UI authority is `docs/goal/ui/folders.md` § Implementation status today (the
// editor's shape: roster read on expand behind a fresh guard, boxes rendered
// from the place's flags, a toggle writing the point **whole**
// through `fauna.folders.places.set` and repainting from a roster RE-READ);
// what a place *means* is `behavior/folders.md` § Target re-model. This module
// owns only the projection between the two, so the seven apps painting
// `folder-place-row` / `folder-place-originates` / `-accepts` /
// `-applies-deletes` resolve a seat through one implementation instead of
// seven.
//
// Lifted at the six apps' phase-4 trickle-down,
// because the rule had already been hand-rolled three times before a second
// app painted a single checkbox. Its one trap becomes unrepresentable here:
//
// - **The point applies WHOLE.** A checkbox writes all three flags, never the
//   one that moved — the two left alone would be cleared. [`toggled`] is what
//   an app's click handler calls, so "send the triple" is the only shape
//   available to it rather than a sentence in a doc comment it may not read.
//
// (Until the role contraction a second arm lived here: a seat whose row carried
// no flags and an unknown `role` painted no boxes. Flags are required on the
// wire now, so every seat paints them.)

/// The three `folder-place-*` checkboxes, addressed by the wire-stable names the
/// cross-app e2e contract already drives. Strings rather than an enum for the
/// same reason every sibling catalog in this crate uses them: they cross UniFFI
/// and wasm unchanged, and they are the ids the test suite clicks.
pub const PLACE_FLAG_ORIGINATES: &str = "originates";
/// Remote changes land on this seat.
pub const PLACE_FLAG_ACCEPTS: &str = "accepts";
/// A peer's delete deletes here (OFF = an archive seat).
pub const PLACE_FLAG_APPLIES_DELETES: &str = "applies_deletes";

/// One enrolled device seat, projected for the editor.
///
/// Everything an app needs to paint a `folder-place-row` and to write an edit
/// back: the identity to address the write with, the label to show, and the
/// flag triple.
///
/// `deny_unknown_fields` rather than the rule-4 `extra` catch-all every
/// client↔nest payload struct carries: this is a **projection**, never a wire
/// message. It is built here from [`FolderMember`] (which does carry the
/// catch-all, so an unknown key from the nest is preserved on the way in) and
/// its only serialization crossing is `fauna-wasm-folders::toggledPlaceRow` —
/// Rust to the web SPA and straight back, both compiled from one commit into
/// one artifact. The opt-out is scoped by WHO DECODES, not by which module a
/// struct lives in (`transport.md` § Schema and forward-compat discipline,
/// rule 4), and this decoder can never be version-skewed from its encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceRow {
    /// Hex device id — what `fauna.folders.places.set` addresses the seat by.
    pub device_id: String,
    /// The seat's human name — from the app's unsealed device roster when it
    /// holds the seat, else the label the nest sent (see [`place_rows`]).
    pub label: String,
    /// Files added on this device upload.
    pub originates: bool,
    /// Remote changes land on this device.
    pub accepts: bool,
    /// A peer's delete deletes here.
    pub applies_deletes: bool,
}

impl PlaceRow {
    /// This row's flags as a [`PlaceFlags`], for a caller composing the write.
    pub fn flags(&self) -> PlaceFlags {
        PlaceFlags::new(self.originates, self.accepts, self.applies_deletes)
    }
}

/// Project a whole device roster (`fauna.folders.members.list`) into the rows
/// the editor paints, in roster order — the indices are what
/// `folder-place-row[j]` addresses, so an app must not filter or re-sort them.
///
/// **`roster` names the seats** — `(device_id, label)` pairs from the device
/// list the app has already unsealed (`fauna.sync.devices.list` through
/// `DevicesMachine`). Every user-chosen device label rests sealed, so
/// `FolderMember.label` is empty for a named device, and the reply carries no
/// sealed sibling to open instead: the seal is under the registering owner's
/// root, which a reader of a foreign seat could not open
/// (`path-sealing.md` § device label, gap (a)). A seat the roster does not hold
/// keeps the nest's label — nameless, or a machine-written constant — until
/// cross-actor rows render the owning account's identity. Roster order never
/// moves a row.
pub fn place_rows<'r>(
    members: &[FolderMember],
    roster: impl IntoIterator<Item = (&'r str, &'r str)>,
) -> Vec<PlaceRow> {
    let names: std::collections::HashMap<&str, &str> = roster.into_iter().collect();
    let label_of = |member: &FolderMember| {
        names
            .get(member.device_id.as_str())
            .map_or_else(|| member.label.clone(), |label| (*label).to_string())
    };
    members
        .iter()
        .map(|member| PlaceRow {
            device_id: member.device_id.clone(),
            label: label_of(member),
            originates: member.flags.originates,
            accepts: member.flags.accepts,
            applies_deletes: member.flags.applies_deletes,
        })
        .collect()
}

/// Flip ONE checkbox on `row` and return the whole resulting point — the value
/// an app hands to `set_folder_place` / `places_set`.
///
/// `None` when `flag` is not one of the three constants above. Callers treat
/// `None` as "no write": there is no partial edit to fall back to, because a
/// place is only ever written whole.
pub fn toggled(row: &PlaceRow, flag: &str) -> Option<PlaceRow> {
    let mut next = row.clone();
    match flag {
        PLACE_FLAG_ORIGINATES => next.originates = !next.originates,
        PLACE_FLAG_ACCEPTS => next.accepts = !next.accepts,
        PLACE_FLAG_APPLIES_DELETES => next.applies_deletes = !next.applies_deletes,
        _ => return None,
    }
    Some(next)
}

// ── fauna.folders.deposit (≡ POST /api/v1/folders/{id}/deposit) ─────────────
//
// `file-sync.md` § Third-party deposit ingress: a principal holding
// `fauna:folder:deposit:<id>` posts one file; the nest seals a
// [`DepositEnvelope`] of it to the folder owner's recipient key and parks it
// in the folder's inbox segment. The reply says "accepted" and nothing else.

/// The largest body one deposit may carry, in bytes — both doors. It sits
/// under the WS-RPC message cap with room for the envelope, so a deposit the
/// HTTP door accepts is one the WS-RPC door could have carried too.
pub const MAX_DEPOSIT_BYTES: usize = 1 << 20;

/// The longest file name a deposit may carry, in UTF-8 bytes — the common
/// filesystem component limit, so adoption never has to truncate.
pub const MAX_DEPOSIT_NAME_BYTES: usize = 255;

/// Is `name` a file name a deposit may carry: one non-empty path component —
/// no separator of either platform, no NUL or other control character, not
/// `.` or `..` — of at most [`MAX_DEPOSIT_NAME_BYTES`]? Shared by both doors
/// and by adoption, which lands the item under exactly this name.
#[must_use]
pub fn is_deposit_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_DEPOSIT_NAME_BYTES
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDepositRequest {
    /// The folder's row id — the one the principal's
    /// `fauna:folder:deposit:<id>` scope names.
    pub folder_id: i64,
    /// The file's name ([`is_deposit_name`]).
    pub name: String,
    /// The file's media type as the depositor declared it; empty = unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    /// The file's bytes, at most [`MAX_DEPOSIT_BYTES`].
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderDepositReply {
    /// Always `true` on success — the only thing a depositor learns.
    pub accepted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What rests sealed in a folder's inbox segment: the deposited file, as the
/// owner's next client sync opens it with the recipient key (adoption,
/// `file-sync.md` § Third-party deposit ingress). Canonical CBOR; the name and
/// media type are content and ride inside the seal, never beside it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DepositEnvelope {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.folders.deposits.{list,retire} — the owner's half of the inbox ────
//
// Adoption (`file-sync.md` § Third-party deposit ingress) reads a folder's
// parked items, opens each with the owner's recipient key, records it as an
// ordinary change row, then retires it. Both kinds are the owner's own: the
// nest resolves the folder from the connection actor's own sets.

/// The most sealed bytes one [`FolderDepositsListReply`] carries beyond its
/// first item — one full-size deposit, so a page always fits the WS-RPC
/// message cap however large its first item is.
pub const DEPOSITS_LIST_PAGE_BYTES: usize = MAX_DEPOSIT_BYTES;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDepositsListRequest {
    pub folder_id: i64,
    /// List items with an id strictly above this one; `0` = from the start.
    #[serde(default)]
    pub after: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One parked item, as the inbox holds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ParkedDeposit {
    /// The item's id — what adoption is idempotent on, and what a retire
    /// names.
    pub id: i64,
    /// The recipient-sealed [`DepositEnvelope`].
    pub sealed: ByteBuf,
    /// When the nest accepted it, Unix seconds.
    pub received_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDepositsListReply {
    /// Oldest first, at least one when any item is parked past `after`.
    pub items: Vec<ParkedDeposit>,
    /// More items are parked past the last one here.
    #[serde(default)]
    pub more: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDepositsRetireRequest {
    pub folder_id: i64,
    pub deposit_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FolderDepositsRetireReply {
    /// `false` when no such item was parked — already retired by another
    /// seat, which is success for an idempotent adoption.
    pub retired: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod deposit_tests {
    use super::*;

    #[test]
    fn a_deposit_name_is_one_plain_component() {
        for good in ["report.pdf", "a", "näme with spaces.txt", ".hidden"] {
            assert!(is_deposit_name(good), "{good:?}");
        }
        let long = "x".repeat(MAX_DEPOSIT_NAME_BYTES + 1);
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "nul\0",
            "tab\tname",
            long.as_str(),
        ] {
            assert!(!is_deposit_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_envelope_round_trips_through_canonical_cbor() {
        let env = DepositEnvelope {
            name: "report.pdf".into(),
            content_type: "application/pdf".into(),
            body: ByteBuf::from(b"%PDF".to_vec()),
            extra: BTreeMap::new(),
        };
        let bytes = crate::encode_canonical(&env).expect("encodes");
        let back: DepositEnvelope = crate::decode_strict(&bytes).expect("decodes");
        assert_eq!(back, env);
    }
}
