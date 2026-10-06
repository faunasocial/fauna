//! The page-level renderable `DevicesSnapshot` + its sub-types. Clients read a
//! fresh copy on every observer tick and render the whole Devices page off it;
//! they never see the internal state. Mirrors
//! `fauna_folders_machine::snapshots` (the wizard's per-step snapshots).
//!
//! The sub-types transcribe the existing list shapes the WS-RPC kinds return
//! (`fauna.sync.devices.list`, `fauna.folders.list`, `fauna.sync.conflicts.list`)
//! into clean `uniffi::Record`s — dropping the wire types' `extra` flatten maps
//! so they cross the FFI boundary. The `From<wire>` conversions live in
//! `nest_api::ws_rpc` (gated behind `rpc-glue`, where `fauna-protocol` is in
//! scope), keeping this module dependency-light (`fauna-core` +
//! `fauna-folders-machine` only).

use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;
use fauna_folders_machine::FolderWizardSnapshot;

/// A device's place in one folder, as the page-level device list reports it.
/// Transcribes `fauna_protocol::sync::DeviceFolderRole` — the place's three
/// flags, which `fauna_core::format::device_place_label` composes into the
/// `device-folder-role-badge` chip text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeviceFolderRole {
    pub name: String,
    /// Files added or edited on this device upload.
    pub originates: bool,
    /// Remote changes land on this device.
    pub accepts: bool,
    /// A peer's delete deletes here.
    pub applies_deletes: bool,
}

/// One registered device on the Devices page. Transcribes the
/// `fauna.sync.devices.list` row (`fauna_protocol::sync::SyncDevice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeviceSummary {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    /// Comma-separated capabilities (e.g. `"read,write"`).
    pub capabilities: String,
    pub registered_at: i64,
    pub last_seen_at: i64,
    pub online: bool,
    /// This device was enrolled by the account's guardian
    /// (`family-safety.md` § Full visibility). Clients render the marker badge
    /// on the row and expect its removal to be refused — the pattern is
    /// transparent by construction, so the supervised account always sees which
    /// of its devices carries their guardian's oversight. Always `false` for an
    /// unsupervised account.
    pub guardian_marked: bool,
    pub folders: Vec<DeviceFolderRole>,
    /// Hex-encoded device PRINCIPAL granted on this row (the T10 writer
    /// key's public — what the R14 (account-data-plane.md § The ratified decisions) generation plane's wraps target). `None`
    /// on an unenrolled row (no principal yet): "unknown", never "keyless" — badge derivations stay
    /// silent without it (`ui/devices.md` § Custody facet piece 1).
    pub principal: Option<String>,
    /// The device's own last report of whether it takes part in
    /// peer-to-peer transfers (`behavior/p2p.md` § Per-device
    /// participation): what `device-p2p-participation-toggle` paints on a
    /// row that is not this device's own. `None` = never reported — the
    /// toggle paints the default (on) and says the state is unreported.
    pub p2p_participation: Option<bool>,
    /// Another of the account's devices asked this one to turn its peer
    /// transfers off and it has not folded that yet (it does at its next
    /// full pass) — the toggle's "turning off" reading.
    pub p2p_off_requested: bool,
    /// `device-p2p-participation-toggle`'s paint for this row — own-ness,
    /// checked, label, actionable — decided by the machine with the same
    /// own-row rule its gesture uses (`behavior/p2p.md` § Per-device
    /// participation → *Which row is this device's*), so no app re-derives
    /// it. Always `Some` on a row [`crate::DevicesMachine::snapshot`]
    /// publishes; optional and defaulted only so a roster serialised
    /// without it (web's JS objects into `place_rows`, app test fixtures)
    /// still reads — [`DeviceSummary::participation_paint`] then answers
    /// the sibling arm.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub p2p_participation_paint: Option<crate::P2pParticipationPaint>,
}

impl DeviceSummary {
    /// The row's published participation paint, or — on a row the machine
    /// did not paint — the sibling arm over its own report.
    pub fn participation_paint(&self) -> crate::P2pParticipationPaint {
        self.p2p_participation_paint.clone().unwrap_or_else(|| {
            crate::p2p_participation_paint(
                false,
                None,
                self.p2p_participation,
                self.p2p_off_requested,
            )
        })
    }
}

/// One **signed-in device without a matching entry** — a verified fleet member
/// no roster row accounts for, offered the member-addressed removal door
/// (`ui/devices.md` § Members without a matching entry;
/// `account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (4), *A disagreement is the user's to settle*). It has
/// no nest row, so no label: what a card shows is the fleet id's fingerprint
/// and the enrollment instant the member's own record asserts. Both are
/// rendered here, once, so every app paints the same strings
/// (`fauna_core::format::fleet_fingerprint` — the SAME formatter as
/// [`DevicesSnapshot::own_fingerprint`], the user's half of the comparison).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FleetMemberSummary {
    /// Hex-encoded 32-byte fleet id (the member's device principal).
    pub device_id: String,
    /// `device-member-fingerprint`'s text: `fleet_fingerprint(device_id)`.
    pub fingerprint: String,
    /// The member's self-asserted enrollment instant, unix ms — a hint, never
    /// proof (`device-member-enrolled-at`; the app formats it locally).
    pub enrolled_at_ms: i64,
}

/// One folder on the Devices page. Transcribes the `fauna.folders.list`
/// row (`fauna_protocol::folders::FolderSummary`).
///
/// `Default` is derived for **fixtures only** — production rows come from the
/// `From<WireFolder>` transcribe, never from a default. It exists so tests build
/// rows with `..Default::default()` instead of hand-listing ~20 fields: this
/// struct grows on a wire-additive cadence, and every hand-listed literal is both
/// a compile break for whoever adds the field and a merge conflict for two branches
/// growing it at once — the house convention for a growing wire type (the protocol
/// twin already derives it). A defaulted row is deliberately inert —
/// `id: 0`, no role, no access — so a fixture that forgets to set a field fails
/// toward "no affordance" rather than toward an owner row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FolderSummary {
    pub id: i64,
    pub name: String,
    /// Opaque JSON-string retention policy; `None` when unset.
    pub retention_policy: Option<String>,
    pub cached_snapshot_count: i64,
    pub cached_total_bytes: i64,
    pub cached_last_snapshot_at: Option<i64>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
    /// Hex-encoded raw MLS **group id** binding this set to a cross-user shared
    /// group. `Some` ⇒ the set is shared (the owner-side `folder-shared-badge`
    /// renders, and `ChannelId::from_group_id(mls_group_id)` yields the channel
    /// id `FoldersAuthor::remove_member` needs); `None` ⇒ owner-only. Transcribed
    /// from `fauna_protocol::folders::FolderSummary::mls_group_id`
    /// (`docs/goal/ui/folders.md` § Sharing).
    pub mls_group_id: Option<String>,
    /// The caller's role for this row (B3 member-list-visibility): `Some("owner")`
    /// — a set the caller owns; `Some("member")` — a set shared *with* the caller
    /// (present only when the page loads via `list_owned_and_shared`); `None` when
    /// no role is stamped (treat as owner). Transcribed from
    /// `fauna_protocol::folders::FolderSummary::role`.
    ///
    /// ⚠ **SAFETY:** a `"member"` row is only *rostered* nest-side — the nest
    /// cannot observe an MLS join. The client MUST render a member row **only if**
    /// it has actually joined the group, i.e.
    /// `MlsEngine::has_group(ChannelId::from_group_id(mls_group_id))` on the
    /// conversations rail's per-session engine, else a stranger's un-accepted knock
    /// appears unbidden. A knock surfaces only as a `folder-pending-share`.
    pub role: Option<String>,
    /// The caller's access to a `role == "member"` row: `Some("writer")` when the
    /// caller holds an explicit `writer` grant, else `None` ⇒ **reader** (the
    /// fail-safe default). A **writer** member row renders the local-folder
    /// binding UI (`folder-location-*`) and syncs read-write; a **reader** row stays
    /// read-only (no binding — `file-sync.md`'s iron rule). Never overloads `role`.
    /// Transcribed from `fauna_protocol::folders::FolderSummary::access`
    /// (`docs/goal/ui/folders.md` § Sharing, multi-writer Phase 1).
    pub access: Option<String>,
    /// Handle of the set's owner — for a `role == "member"` row; `None` for the
    /// caller's own sets or an unresolved handle. Transcribed from
    /// `FolderSummary::owner_handle`. **Render [`Self::owner_display`], not this
    /// field**, for the recipient badge — this is the raw handle, kept for
    /// non-display logic.
    pub owner_handle: Option<String>,
    /// The pre-computed "Shared by ‹…›" label — the **one string** a client
    /// renders for the recipient `folder-shared-badge`, so the six apps
    /// cannot drift on the fallback truncation (the per-app 12-char/ellipsis
    /// variants this replaces; priorities #1/#4 — the owner-side twin of the
    /// pending-share `shared_by_display`). Delegates to
    /// `fauna_core::format::account_display_label`: the owner handle when present
    /// and non-empty, else the canonical `short_id` of the owner's actor id.
    /// Empty string for the caller's **own** rows (no owner handle and no owner
    /// actor id) — those render "Shared · N", not a "Shared by" badge, so the
    /// empty label is never shown. Computed in the `From<WireFolder>` transcribe
    /// from `FolderSummary::owner_handle` + `owner_actor_id`.
    pub owner_display: String,
    /// Whether this set is currently served over WebDAV — the per-set exposure
    /// gate the Settings → Folders `folder-webdav-toggle` reflects and flips
    /// (`docs/goal/behavior/webdav-server.md` § Independent enablement point 2;
    /// `docs/goal/ui/folders.md` § Element IDs). **The owner's custody's
    /// word, never the nest's `folders.webdav_enabled` flag** (ruling
    /// (7)(b)(ii) rule (2), `writer-signed-change-records.md`):
    /// `DevicesMachine::render_folders` asks `DevicesNestApi::webdav_served`
    /// (custody's serve window) after the set names render, and writes the
    /// answer here; a seam with no readable custody reads not served. A
    /// reserved `__` set is never served.
    pub webdav_enabled: bool,
    /// The set's conflict policy (`"auto"` — merge text, else latest-wins — |
    /// `"latest_wins_always"`), the `folder-conflict-policy-select` value.
    /// `None` when unset (render as `"auto"`, the nest-side
    /// column default) and on `role == "member"` rows (the owner controls the
    /// policy; `member_summary` withholds it). Transcribed from
    /// `fauna_protocol::folders::FolderSummary::conflict_policy`
    /// (`docs/goal/behavior/file-sync.md` § Conflicts, policy).
    pub conflict_policy: Option<String>,
    /// The subscription tier this website folder is paywalled to, or `None` when it
    /// serves publicly — the per-set gate the Settings → Folders
    /// `folder-paywall-tier-select` reflects and sets (`docs/goal/ui/folders.md`
    /// § Web paywall; `docs/goal/behavior/monetization.md` § Pillar 2). Transcribed
    /// from `fauna_protocol::folders::FolderSummary::web_paywall_tier` (the
    /// nest's additive `folders.web_paywall_tier` column). v1 is set-only — the client
    /// offers no clear affordance until the nest-side revoke/rotation leg lands.
    pub web_paywall_tier: Option<String>,
    /// The set's **home-nest base URL** when it is a FOREIGN (cross-nest)
    /// membership — a `role == "member"` row synthesized from the member's own
    /// `fauna.state.folder-keys` foreign-set row ([`crate::machine::ForeignSetsSource`]; Phase 2 client
    /// read-side) because the own nest holds no row for it. Clients thread it
    /// into the leave relay (`fauna.folders.leave` additive `nest_url`) and
    /// may show a "from ‹nest›" hint. `None` for every same-nest row (owner or
    /// member). Such a row also carries `id == -1` (no nest row id).
    pub home_nest_url: Option<String>,
    /// The nest place's "keeps snapshots" knob — `None` is **unset**, meaning
    /// the nest-wide behavior, not `false`. The `folder-nest-snapshots-select`
    /// value (folders re-model phase 2; behavior owner
    /// `docs/goal/behavior/backup-restore.md` § 8b). Transcribed from
    /// `fauna_protocol::folders::FolderSummary::nest_place`.
    ///
    /// ⚠ `Some(true)` is a *preference*, never a guarantee: the nest's
    /// structural refusals apply on top, so a row rendering "keeps snapshots"
    /// may still legitimately have none.
    pub nest_snapshots: Option<bool>,
    /// The nest place's quiet period in seconds; `None` = unset ⇒ the nest-wide
    /// scheduler cadence. The `folder-nest-quiet-input` value.
    ///
    /// The policy's third knob, retention, is [`Self::retention_policy`] above —
    /// it kept its own field because it is a live column with a sealed display
    /// twin (`backup-restore.md` § 8b).
    pub nest_snapshot_quiet_secs: Option<i64>,
    /// The version-retention bounds pair, flattened (`file-versions.md`
    /// § Retention ruling 1 — the § 8b editor's fourth per-place knob): max
    /// listable versions per path / max version age in days. `0` = that bound
    /// unset; both zero = keep everything, the resting state (an absent
    /// `version_retention` on the wire lands as `(0, 0)`). Prefills the two
    /// `folder-version-retention-*` boxes via
    /// `fauna_folders_machine::version_retention_edit_from_bounds`.
    pub version_retention_max_versions: u32,
    pub version_retention_max_age_days: u32,
    /// The folder's **audience** (folders re-model phase 4): `"private"` |
    /// `"shared"` | `"public"` — `"public"` ⇒ the owner declassified it and its
    /// content/names/paths rest world-readable (`principles.md` § The user
    /// always controls their data, the one deliberate exception). Transcribed
    /// from `fauna_protocol::folders::FolderSummary::audience`; an empty
    /// wire value (a non-conforming nest — every nest projects a token) reads
    /// as `"private"`, the most restrictive audience, so no consumer meets an
    /// empty string and an empty value never reads "public".
    pub audience: String,
    /// Whether this folder is served as the owner's **website** (phase 4 — the
    /// per-folder toggle that replaced `mode == "web"`). Owner-side control;
    /// `false` on member rows, like [`Self::webdav_enabled`]. Transcribed from
    /// `fauna_protocol::folders::FolderSummary::website_enabled`.
    pub website_enabled: bool,
    /// The folder's **content residency** (folders re-model phase 5 —
    /// `file-sync.md` § Content residency): `"metadata_only"` ⇒ chunk bytes
    /// never rest on the nest (the owner's consent-gated choice); empty /
    /// `"full"` ⇒ today's behavior, the nest keeps content. Rides **both**
    /// projection arms — a member seat uploads bytes too, so it must see it.
    /// Transcribed from `fauna_protocol::folders::FolderSummary::residency`;
    /// a full folder sends none (empty), landing as empty (read fail-closed to full by
    /// `fauna_folders_machine::normalize_residency`).
    pub residency: String,
    /// Whether the folder's owner turned **exclusive editing** on — one device
    /// at a time may write (`file-sync.md` § Exclusive editing). Rides both
    /// projection arms. Transcribed from
    /// `fauna_protocol::folders::FolderSummary::exclusive_editing`, whose
    /// serde default already reads an absent field as OFF — the flag fails
    /// open to un-governed, and this transcription keeps that reading.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub exclusive_editing: bool,
    /// Who may write the folder right now, when [`Self::exclusive_editing`]
    /// is on; `None` = not held. A
    /// SNAPSHOT for rendering — the acquire stays the atomic arbiter. Never
    /// rendered raw: `crate::folder_lease_status` resolves it to the line
    /// `folder-lease-status` paints, naming the holder by its label.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub lease: Option<FolderLeaseSummary>,
    /// The owner's signed **audience attestation** the nest serves on the row
    /// (`encryption-at-rest.md` § Readable classes → *The declassification is
    /// owner-ATTESTED*), transcribed verbatim — the nest stores it opaquely and
    /// verifies nothing. `None` = none served (never minted). Read ONLY through [`Self::is_public_unverified_for`],
    /// which hands it to the one verifier; never judged here.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub audience_attestation: Option<FolderAudienceAttestation>,
}

/// A folder's served audience attestation as plain data — the
/// [`fauna_protocol::folders::AudienceAttestation`] fields with the byte
/// strings as `Vec<u8>`, so the record crosses UniFFI and serde alike. The
/// bytes round-trip unchanged; this type holds no rule of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FolderAudienceAttestation {
    pub owner: Vec<u8>,
    pub folder_id: i64,
    pub counter: u64,
    pub sig: Vec<u8>,
}

impl From<fauna_protocol::folders::AudienceAttestation> for FolderAudienceAttestation {
    fn from(a: fauna_protocol::folders::AudienceAttestation) -> Self {
        Self {
            owner: a.owner.into_vec(),
            folder_id: a.folder_id,
            counter: a.counter,
            sig: a.sig.into_vec(),
        }
    }
}

impl From<FolderAudienceAttestation> for fauna_protocol::folders::AudienceAttestation {
    fn from(a: FolderAudienceAttestation) -> Self {
        Self {
            owner: fauna_protocol::ByteBuf::from(a.owner),
            folder_id: a.folder_id,
            counter: a.counter,
            sig: fauna_protocol::ByteBuf::from(a.sig),
        }
    }
}

impl FolderSummary {
    /// Is this owner row public on the nest's say-so while its attestation
    /// does not verify under `own` (this seat's own actor id)? The paint
    /// condition for `folder-audience-unattested` and its re-confirm button.
    /// Delegates to [`fauna_protocol::folders::FolderSummary::is_public_unverified_for`]
    /// over exactly the fields that verdict reads, so the rule stays spelled
    /// once.
    #[must_use]
    pub fn is_public_unverified_for(&self, own: &fauna_core::identity::ActorId) -> bool {
        fauna_protocol::folders::FolderSummary {
            id: self.id,
            name: self.name.clone(),
            audience: self.audience.clone(),
            webdav_enabled: self.webdav_enabled,
            audience_attestation: self.audience_attestation.clone().map(Into::into),
            ..Default::default()
        }
        .is_public_unverified_for(own)
    }
}

/// A folder's live exclusive-editing lease, as the folder projection reports
/// it. Transcribes `fauna_protocol::folders::FolderLeaseState`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FolderLeaseSummary {
    /// Hex-encoded 32-byte device id of the holder — a roster KEY, never text
    /// for the user (the roster's label is).
    pub device_id: String,
    /// Unix epoch seconds the lease lapses at unless renewed. A past value
    /// reads as *not held*: the nest sweeps expired rows lazily.
    pub expires_at: i64,
}

/// One candidate version in a sync conflict — a manifest the user may choose to
/// keep. Transcribes `fauna_protocol::folders::ConflictCandidate`. The chosen
/// `manifest_hash` is forwarded to `resolve_conflict` as the winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConflictCandidateSummary {
    /// Hex BLAKE3 manifest hash — the version kept if this candidate wins.
    pub manifest_hash: String,
    /// Hex-encoded device id that produced this version.
    pub device_id: String,
    pub size_bytes: i64,
    pub created_at: i64,
    /// M2 content-key generation this candidate's manifest was sealed under,
    /// for a bound (MLS-keyed) set — echoed verbatim into the review-list
    /// "use the other version" re-point so a sealed set's restored head stays
    /// openable. `None` for unsealed sets.
    pub content_key_version: Option<u64>,
}

/// One sync conflict on the Folders page — auto-resolved (the review-list
/// surface, ratified 2026-07-10) or still-unresolved (a mark-only report
/// awaiting resolution). Transcribes the
/// `fauna.sync.conflicts.list` row (`fauna_protocol::folders::SyncConflict`),
/// fetched with `include_resolved = true`. `candidates` is the diverging
/// versions (the retained parents on a resolved row); empty for a
/// mark-only conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConflictSummary {
    pub id: i64,
    /// Folder name.
    pub folder: String,
    /// Hex-encoded device id that produced the conflicting change.
    pub device_id: String,
    pub path: String,
    pub conflict_type: String,
    pub details: Option<String>,
    pub created_at: i64,
    pub candidates: Vec<ConflictCandidateSummary>,
    /// When this conflict resolved (unix seconds); `None` = unresolved (render
    /// informationally — no blocking chooser; a later resolve or
    /// detection pass settles it).
    pub resolved_at: Option<i64>,
    /// `"merged"` (clean three-way text merge) | `"latest_wins"` | `None`
    /// (unresolved, or resolved by a chooser with no resolution stamp) — the `conflict-type-badge`
    /// text on a review row.
    pub resolution: Option<String>,
    /// Winning version's manifest hash (the head devices converged on; the
    /// merged result on a `"merged"` row — NOT one of `candidates`). The
    /// review-list re-point targets the latest candidate ≠ this hash.
    pub winning_manifest_hash: Option<String>,
    /// Whether this resolved row retains a candidate *other than* the winning
    /// head — the gate for the `conflict-resolve-button` one-tap re-point
    /// (`use_other_version`); always `false` while unresolved. Computed once
    /// at transcribe (the `owner_display` pattern) so no client re-derives
    /// the winner/candidate comparison — an ungated button drives the
    /// resolve path against an arbitrary candidate (the android drift this
    /// field retires).
    pub has_other_version: bool,
    /// The display-ready `conflict-file-info` line: `"{set}: {path} → "` +
    /// the first 8 hex of the winning head once resolved, the bare
    /// `"{set}: {path}"` before. Computed once at transcribe; clients render
    /// it verbatim.
    pub file_info: String,
}

/// One **followed public folder** on the Folders page (`docs/goal/ui/folders.md`
/// § Following a public folder; behavior owner
/// `docs/goal/behavior/folders.md` § Publicly-synced follow).
///
/// A distinct row kind from [`FolderSummary`], not a variant of it — which is
/// what the ratified element IDs say too (`folder-followed-item` /
/// `folder-followed-status` / `folder-unfollow-button`, never `folder-item`).
/// A followed folder has no device roster, no location binding, no share
/// section and no toggles; it has no MLS group, so it is not on the
/// membership axis `FolderSummary::{role,access,mls_group_id}` describes, and
/// it carries a status those rows have nowhere to put. Squeezing it into
/// `FolderSummary` would have meant a sentinel id, four inapplicable fields and
/// a carve-out in the member join-filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FollowedFolderSummary {
    /// The home nest's stable `folders.id` — the pinned address every read
    /// after the first uses, and half the identity of the follow record.
    pub folder_id: i64,
    /// The folder's home nest base URL; empty ⇒ homed on the user's own nest
    /// (the same-nest follow). The other half of the follow's identity.
    pub home_nest_url: String,
    /// Hex owner actor id — what the row's owner label falls back to.
    pub owner_actor_id: String,
    /// The owner's handle **as last verified** — the handle the user followed
    /// them by, shown only while it still names `owner_actor_id`
    /// (`fauna_client_folders::public_follow::owner_handle_verdict`); `None`
    /// for a follow made by actor id, or one whose handle has since moved on.
    /// **Render [`Self::owner_display`], not this field** — the same split
    /// [`FolderSummary::owner_handle`] / `owner_display` keeps.
    pub owner_handle: Option<String>,
    /// The ONE owner string every app paints on the followed row
    /// (`ui/folders.md` § Following a public folder: *name + owner handle +
    /// badge + status*) and the Media filter option appends
    /// (`ui/media.md` § Followed public folders) — precomputed by
    /// `fauna_core::format::account_display_label`, so the handle, or else the
    /// canonical short form of the actor id, and never a per-app truncation.
    pub owner_display: String,
    /// The folder's plaintext name as of the last successful read. Public
    /// names are world-readable by the ratified exception, so this is not
    /// secret.
    pub display_name: String,
    /// Whether the home nest still serves this folder.
    ///
    /// `false` is the **revoke**: the owner flipped the audience back (or
    /// deleted the folder), and the plane answers the same `not_found` an
    /// absent folder gets. The row stays visible in a loud *no longer
    /// available* state until the user removes it, and a re-flip resumes it —
    /// so this is never a reason to drop the row.
    ///
    /// ⚠ Availability means *the home nest refused*, never *the read failed*.
    /// A transport fault leaves the last known value standing, because
    /// rendering "no longer available" on a dropped connection would tell the
    /// user their follow was revoked every time their network blipped
    /// (`fauna_client_folders::public_follow::FollowError` draws the same line).
    pub available: bool,
}

/// The whole renderable Devices page in one record — the page-level analogue of
/// the wizard's aggregate `FolderWizardSnapshot`. A single observer tick fully
/// describes the page. See `docs/goal/ui/devices.md` § State & data shape →
/// *Broader DevicesSnapshot*.
/// `Default` is the empty page (no devices, folders, follows or conflicts; no
/// wizard; no error) — the state the machine starts in.
///
/// It exists so **fixtures build with `..Default::default()`** instead of
/// hand-listing every field: this record grows on the same axis from several
/// branches at once, and a hand-listed fixture turns each additive field into a
/// compile break plus an add/add merge collision — the house convention for a
/// growing record, the same one `FolderSummary`'s own `Default` carries. The
/// `followed` field added by the publicly-synced follow broke five such
/// fixtures, which is what prompted this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DevicesSnapshot {
    pub devices: Vec<DeviceSummary>,
    pub folders: Vec<FolderSummary>,
    /// Followed public folders — a separate list from [`Self::folders`], the
    /// way they are a separate row kind on the page. Empty unless a followed
    /// source is wired (`DevicesMachine::set_followed_folders_source`).
    pub followed: Vec<FollowedFolderSummary>,
    pub conflicts: Vec<ConflictSummary>,
    /// Whether this actor's per-handle web address is switched on
    /// (`fauna.web.get_subdomain_enabled`) — the second half of the website
    /// toggle's tri-state hint (`fauna_folders_machine::website_serve_hint`).
    /// `None` = unknown (an unwired adapter, a failed read): the
    /// hint then hedges rather than claiming the site is live. Read
    /// best-effort with the page; never a page error.
    pub website_address_enabled: Option<bool>,
    /// `Some` while a folder creation wizard is open (the embedded
    /// `FolderWizardMachine`'s snapshot), `None` otherwise.
    pub wizard: Option<FolderWizardSnapshot>,
    /// The page-level `error-message` (last gesture / refresh failure),
    /// localized client-side; `None` when clear.
    pub error: Option<LocalizedText>,
    /// Signed-in devices without a matching entry (`device-member-card`,
    /// indexed) — every verified fleet member no roster row accounts for, in
    /// fleet-id order. Empty on a machine with no `FleetRemoval` door wired
    /// and while the runtime is not up.
    pub members: Vec<FleetMemberSummary>,
    /// This device's own fleet id (hex), when the door answered; what
    /// [`Self::own_fingerprint`] is rendered from.
    pub own_fleet_id: Option<String>,
    /// `device-own-fingerprint`'s text — this device's fleet id through the
    /// same `fleet_fingerprint` every member card uses, painted on this
    /// device's own roster row so the user compares like with like. `None`
    /// exactly when [`Self::own_fleet_id`] is.
    pub own_fingerprint: Option<String>,
    /// THIS device's own peer participation — the device-local authority
    /// (`behavior/p2p.md` § Per-device participation), read through the
    /// injected `P2pParticipation` door on every refresh. What
    /// `device-p2p-participation-toggle` paints on this device's own row;
    /// `None` while no door is wired or the runtime is not up, and the own
    /// row then paints its reported state like any other.
    pub own_p2p_participation: Option<bool>,
}

// ── Wire → snapshot transcription (drops the wire `extra` flatten maps) ──────
//
// These live here, beside the summary types they build, and NOT in
// `nest_api::ws_rpc` where they were written. That module is
// `#[cfg(feature = "rpc-glue")]`, so at default features the impls vanished
// while `machine.rs` still called `.into()` unconditionally — 8×E0277, a crate
// that did not compile without a feature every real consumer happened to
// enable. Nothing here needs the transport: both sides are ungated types
// (`fauna_protocol` wire rows in, `crate::snapshots` summaries out), so a
// transport module was never their home.

use fauna_protocol::folders::{ConflictCandidate, FolderSummary as WireFolder, SyncConflict};
use fauna_protocol::sync::{DeviceFolderRole as WireRole, SyncDevice};

impl From<WireRole> for DeviceFolderRole {
    fn from(r: WireRole) -> Self {
        Self {
            name: r.name,
            originates: r.flags.originates,
            accepts: r.flags.accepts,
            applies_deletes: r.flags.applies_deletes,
        }
    }
}

impl From<SyncDevice> for DeviceSummary {
    fn from(d: SyncDevice) -> Self {
        Self {
            device_id: d.device_id,
            label: d.label,
            capabilities: d.capabilities,
            registered_at: d.registered_at,
            last_seen_at: d.last_seen_at,
            online: d.online,
            guardian_marked: d.guardian_marked,
            folders: d.folders.into_iter().map(Into::into).collect(),
            principal: d.principal,
            p2p_participation: d.p2p_participation,
            p2p_off_requested: d.p2p_off_requested,
            // Painted by `DevicesMachine::snapshot`, which knows the own row.
            p2p_participation_paint: None,
        }
    }
}

impl From<WireFolder> for FolderSummary {
    fn from(f: WireFolder) -> Self {
        // Fold the raw owner handle + actor id into the single precomputed badge
        // label so no client re-drifts on the fallback truncation (the owner-side
        // twin of `shared_by_display`). Empty for the caller's own rows (both
        // absent), which render "Shared · N" and never show this label.
        let owner_display = fauna_core::format::account_display_label(
            f.owner_handle.as_deref(),
            f.owner_actor_id.as_deref().unwrap_or(""),
        );
        // Every nest projects a token; an empty one (non-conforming) reads as
        // the most restrictive audience — never "public", never derived.
        let audience = if f.audience.is_empty() {
            "private".to_string()
        } else {
            f.audience.clone()
        };
        Self {
            id: f.id,
            name: f.name,

            retention_policy: f.retention_policy,
            cached_snapshot_count: f.cached_snapshot_count,
            cached_total_bytes: f.cached_total_bytes,
            cached_last_snapshot_at: f.cached_last_snapshot_at,
            include_paths: f.include_paths,
            exclude_paths: f.exclude_paths,
            mls_group_id: f.mls_group_id,
            role: f.role,
            access: f.access,
            owner_handle: f.owner_handle,
            owner_display,
            webdav_enabled: f.webdav_enabled,
            conflict_policy: f.conflict_policy,
            web_paywall_tier: f.web_paywall_tier,
            // Wire rows are always same-nest (the own nest has no rows for
            // foreign sets); foreign rows are synthesized in `foreign_rows`.
            home_nest_url: None,
            // `nest_place` is omitted entirely by a nest whose folder rests
            // unset — that lands as
            // `None` on both knobs, which is exactly "nothing authoritative
            // said". Never defaulted to `false`/`0`.
            nest_snapshots: f.nest_place.as_ref().and_then(|p| p.snapshots),
            nest_snapshot_quiet_secs: f.nest_place.as_ref().and_then(|p| p.quiet_secs),
            version_retention_max_versions: f
                .version_retention
                .as_ref()
                .map_or(0, |v| v.max_versions_per_path),
            version_retention_max_age_days: f
                .version_retention
                .as_ref()
                .map_or(0, |v| v.max_age_days),
            audience,
            website_enabled: f.website_enabled,
            // Verbatim from the wire — the reading (empty ⇒ full, fail-closed)
            // is `fauna_folders_machine::normalize_residency`'s job at the paint
            // site, not this transcription's, so an unknown value survives here
            // rather than being silently coerced.
            residency: f.residency,
            exclusive_editing: f.exclusive_editing,
            lease: f.lease.map(|l| FolderLeaseSummary {
                device_id: l.device_id,
                expires_at: l.expires_at,
            }),
            audience_attestation: f.audience_attestation.map(Into::into),
        }
    }
}

impl From<ConflictCandidate> for ConflictCandidateSummary {
    fn from(c: ConflictCandidate) -> Self {
        Self {
            manifest_hash: c.manifest_hash,
            device_id: c.device_id,
            size_bytes: c.size_bytes,
            created_at: c.created_at,
            content_key_version: c.content_key_version,
        }
    }
}

impl From<SyncConflict> for ConflictSummary {
    fn from(c: SyncConflict) -> Self {
        // Review-row derivations, computed once here (the `owner_display`
        // pattern): a re-point is only offered when a resolved row retains a
        // candidate other than the winning head, and the info line carries
        // the shortened winning head once resolved.
        let has_other_version = match &c.winning_manifest_hash {
            Some(w) => c.candidates.iter().any(|cand| &cand.manifest_hash != w),
            None => false,
        };
        let file_info = match &c.winning_manifest_hash {
            Some(w) => format!("{}: {} → {}", c.folder, c.path, w.get(..8).unwrap_or(w)),
            None => format!("{}: {}", c.folder, c.path),
        };
        Self {
            id: c.id,
            folder: c.folder,
            device_id: c.device_id,
            path: c.path,
            conflict_type: c.conflict_type,
            details: c.details,
            created_at: c.created_at,
            candidates: c.candidates.into_iter().map(Into::into).collect(),
            resolved_at: c.resolved_at,
            resolution: c.resolution,
            winning_manifest_hash: c.winning_manifest_hash,
            has_other_version,
            file_info,
        }
    }
}

/// Append the skipping device's label to each skipped catch-up change's
/// `file_info` line, so the user sees WHICH device is behind (`conflicts.md`
/// § Skipped catch-up changes reach the review list: on that row `device_id`
/// names the device that skipped, not a writer). A device absent from the list
/// (removed, or a read that failed) is named by its id's first 8 hex.
pub(crate) fn name_skipping_devices(conflicts: &mut [ConflictSummary], devices: &[DeviceSummary]) {
    for c in conflicts
        .iter_mut()
        .filter(|c| c.conflict_type == fauna_protocol::folders::CONFLICT_TYPE_CATCHUP_FAILED)
    {
        let label = devices
            .iter()
            .find(|d| d.device_id == c.device_id)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| c.device_id.get(..8).unwrap_or(&c.device_id).to_string());
        c.file_info = format!("{} ({label})", c.file_info);
    }
}

#[cfg(test)]
mod wire_transcription_tests {
    use super::*;
    use fauna_protocol::folders::{ConflictCandidate, FolderSummary as WireFolder};

    /// The served attestation reaches the paint site intact, and the snapshot's
    /// predicate gives the protocol verifier's answer: an owner's genuine
    /// attestation verifies after transcription; the same row without it does
    /// not (the bytes must survive the `Vec<u8>` hop unchanged).
    #[test]
    fn the_audience_attestation_survives_transcription_into_the_verifier() {
        let owner = fauna_core::identity::ActorKeypair::from_secret([5; 32]);
        let att = fauna_protocol::folders::AudienceAttestation::mint(&owner, 9, "site", 42, None);
        let wire = WireFolder {
            id: 9,
            name: "site".into(),
            audience: "public".into(),
            audience_attestation: Some(att.clone()),
            ..Default::default()
        };
        let attested: FolderSummary = wire.clone().into();
        assert_eq!(
            fauna_protocol::folders::AudienceAttestation::from(
                attested.audience_attestation.clone().expect("transcribed")
            ),
            att
        );
        assert!(!attested.is_public_unverified_for(&owner.actor_id()));

        let bare: FolderSummary = WireFolder {
            audience_attestation: None,
            ..wire
        }
        .into();
        assert!(bare.is_public_unverified_for(&owner.actor_id()));
    }

    /// The audience is transcribed as the nest projected it; an empty token
    /// (non-conforming nest) reads as `"private"` even on a bound folder —
    /// never derived from `mls_group_id`, never `"public"`.
    #[test]
    fn audience_is_transcribed_and_an_empty_token_reads_private() {
        let bound = |audience: &str| -> FolderSummary {
            WireFolder {
                id: 3,
                name: "set".into(),
                mls_group_id: Some("ab".repeat(16)),
                audience: audience.into(),
                ..Default::default()
            }
            .into()
        };
        assert_eq!(bound("shared").audience, "shared");
        assert_eq!(bound("public").audience, "public");
        assert_eq!(bound("").audience, "private");
    }

    #[test]
    fn from_wire_transcribes_mls_group_id_role_and_owner_handle() {
        fn wire(
            mls_group_id: Option<&str>,
            role: Option<&str>,
            owner_handle: Option<&str>,
            webdav_enabled: bool,
        ) -> WireFolder {
            WireFolder {
                id: 1,
                name: "shared-docs".into(),
                retention_policy: None,
                cached_snapshot_count: 0,
                cached_total_bytes: 0,
                cached_last_snapshot_at: None,
                include_paths: None,
                exclude_paths: None,
                mls_group_id: mls_group_id.map(str::to_string),
                role: role.map(str::to_string),
                owner_handle: owner_handle.map(str::to_string),
                webdav_enabled,
                ..Default::default()
            }
        }
        // A shared set's raw group id flows through the transcription so the
        // owner-side `folder-shared-badge` + the remove-channel derivation
        // (`ChannelId::from_group_id`) work client-side (folders.md § Sharing).
        let shared: FolderSummary = wire(Some("deadbeef"), Some("owner"), None, true).into();
        assert_eq!(shared.mls_group_id.as_deref(), Some("deadbeef"));
        assert_eq!(shared.role.as_deref(), Some("owner"));
        // The per-set WebDAV serve flag flows through so the `folder-webdav-toggle`
        // reflects nest state (webdav-server.md § Independent enablement point 2).
        assert!(shared.webdav_enabled);
        // A set shared *with* the caller carries role=member + the owner handle
        // (the "Shared by ‹handle›" badge — B3 member-list-visibility).
        let member: FolderSummary = wire(Some("beef"), Some("member"), Some("alice"), false).into();
        assert_eq!(member.role.as_deref(), Some("member"));
        assert_eq!(member.owner_handle.as_deref(), Some("alice"));
        // An owner-only (unshared) set stays `None`, unserved.
        let owner_only: FolderSummary = wire(None, Some("owner"), None, false).into();
        assert_eq!(owner_only.mls_group_id, None);
        assert_eq!(owner_only.owner_handle, None);
        assert!(!owner_only.webdav_enabled);

        // A paywalled website set carries its tier through so the
        // `folder-paywall-tier-select` reflects nest state (folders.md § Web
        // paywall). A public set stays `None`.
        let paywalled: FolderSummary = WireFolder {
            website_enabled: true,
            web_paywall_tier: Some("gold".into()),
            ..Default::default()
        }
        .into();
        assert_eq!(paywalled.web_paywall_tier.as_deref(), Some("gold"));
        assert!(paywalled.website_enabled);
        assert_eq!(owner_only.web_paywall_tier, None);
    }

    /// `owner_display` folds the owner handle + actor id into the one recipient
    /// badge label (the owner-side twin of `shared_by_display`): handle when
    /// present, else the canonical `short_id` of the owner actor id, else empty
    /// for the caller's own rows. Kills the per-app fallback-truncation drift
    /// (priorities #1/#4) — in particular the apple bare-`…` degradation.
    #[test]
    fn from_wire_computes_owner_display_fallback() {
        let actor = "a1".repeat(32); // a 64-hex owner actor id
        let base = |handle: Option<&str>, owner_actor_id: Option<&str>| WireFolder {
            id: 1,
            name: "shared".into(),
            role: Some("member".into()),
            owner_handle: handle.map(str::to_string),
            owner_actor_id: owner_actor_id.map(str::to_string),
            ..Default::default()
        };
        // Handle present ⇒ the handle wins.
        let with_handle: FolderSummary = base(Some("alice"), Some(&actor)).into();
        assert_eq!(with_handle.owner_display, "alice");
        // Handle absent (cross-nest / unset) ⇒ short_id of the owner actor id,
        // NOT a bare ellipsis (the degradation this enrichment fixes).
        let handle_less: FolderSummary = base(None, Some(&actor)).into();
        assert_eq!(
            handle_less.owner_display,
            fauna_core::format::short_id(&actor)
        );
        assert_ne!(handle_less.owner_display, "…");
        // The caller's own row (both absent) ⇒ empty; it renders "Shared · N".
        let own: FolderSummary = base(None, None).into();
        assert_eq!(own.owner_display, "");
        // An empty handle string folds to the actor-id fallback
        // (`account_display_label` treats "" as absent).
        let empty_handle: FolderSummary = base(Some(""), Some(&actor)).into();
        assert_eq!(
            empty_handle.owner_display,
            fauna_core::format::short_id(&actor)
        );
    }

    /// The conflict review-row derivations are computed ONCE at transcribe
    /// (the `owner_display` pattern): `has_other_version` gates the
    /// `conflict-resolve-button` re-point (a resolved row must retain a
    /// candidate ≠ the winning head to re-point at), and `file_info` is the
    /// `conflict-file-info` line (`"{set}: {path} → {winner8}"` once resolved,
    /// the bare `"{set}: {path}"` before). Kills four hand-rolled client
    /// copies and the android drift (an ungated resolve button + a
    /// winner-less `"{set} · {path}"` line).
    #[test]
    fn from_wire_computes_conflict_review_row_derivations() {
        let winner = "c0ffee00".repeat(8); // 64-hex manifest hash
        let loser = "deadbeef".repeat(8);
        let cand = |hash: &str| ConflictCandidate {
            manifest_hash: hash.to_string(),
            ..Default::default()
        };
        let wire = |winning: Option<&str>, candidates: Vec<ConflictCandidate>| SyncConflict {
            id: 7,
            folder: "docs".into(),
            path: "notes/todo.md".into(),
            conflict_type: "concurrent_edit".into(),
            winning_manifest_hash: winning.map(str::to_string),
            candidates,
            ..Default::default()
        };

        // Resolved with a retained loser ⇒ the one-tap re-point is offered,
        // and the info line carries the first 8 hex of the winning head.
        let repointable: ConflictSummary =
            wire(Some(&winner), vec![cand(&winner), cand(&loser)]).into();
        assert!(repointable.has_other_version);
        assert_eq!(repointable.file_info, "docs: notes/todo.md → c0ffee00");

        // Resolved but every candidate IS the winner (candidate-free (mark-only) /
        // merged-in-place) ⇒ nothing to re-point at.
        let mark_only: ConflictSummary = wire(Some(&winner), vec![cand(&winner)]).into();
        assert!(!mark_only.has_other_version);
        let candidate_less: ConflictSummary = wire(Some(&winner), vec![]).into();
        assert!(!candidate_less.has_other_version);

        // Unresolved (no winning head) ⇒ never re-pointable, bare info line.
        let unresolved: ConflictSummary = wire(None, vec![cand(&loser)]).into();
        assert!(!unresolved.has_other_version);
        assert_eq!(unresolved.file_info, "docs: notes/todo.md");

        // A shorter-than-8 winning hash passes through whole (defensive).
        let short: ConflictSummary = wire(Some("ab12"), vec![]).into();
        assert_eq!(short.file_info, "docs: notes/todo.md → ab12");
    }
}
