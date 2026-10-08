//! Folder management WS-RPC handlers (bearer connection) — part of the
//! WS-RPC-everywhere migration (tracked internally). A behavior-preserving
//! transport migration of the bearer-authed user folder routes
//! (`user_folder_routes` + `lease_routes`):
//!
//! - `fauna.folders.{create,list,update,delete,devices}`
//! - `fauna.folders.members.{add,list,remove}`
//! - `fauna.folders.share` (cross-user shared folders, Slice 2 — net-new, not
//!   a transport migration; binds a set to a client-created MLS group)
//! - `fauna.folders.content_key.{put,get}` (shared folders, Slice 3 — net-new;
//!   opaque storage for the M2 content-key envelope)
//! - `fauna.folders.lease.{acquire,release}`
//! - `fauna.sync.conflicts.{list,report,resolve}`
//!
//! Each handler reuses the same `CacheDb` method the HTTP twin calls and scopes
//! on the connection `actor_id` (the twins did `bearer.0.0`). Gate `User |
//! Admin` (the twins were plain `BearerAuth`; an admin owns folders too) —
//! enforced in `bridge_method_allowlist::is_permitted`. Wire types +
//! design decisions: `libs/fauna-protocol/src/folders.rs`.
//!
//! Error namespaces follow the kind namespace: `fauna.folders.*` for the
//! folder kinds, `fauna.sync.*` for the `sync.conflicts.*` kinds.

use std::time::Duration;

use fauna_protocol::folders::{
    ConflictCandidate, ConflictReportReply, ConflictReportRequest, ConflictResolveReply,
    ConflictResolveRequest, ConflictsListReply, ConflictsListRequest, FolderCreateReply,
    FolderCreateRequest, FolderDeleteReply, FolderDeleteRequest, FolderDevice, FolderDevicesReply,
    FolderDevicesRequest, FolderMember, FolderSetWebPaywallReply, FolderSetWebPaywallRequest,
    FolderShareReply, FolderShareRequest, FolderSummary, FolderUpdateReply, FolderUpdateRequest,
    FoldersListReply, FoldersListRequest, FoldersPublicFetchReply, FoldersPublicFetchRequest,
    KIND_FOLDERS_CONTENT_KEY_GET, KIND_FOLDERS_CONTENT_KEY_PUT, KIND_FOLDERS_CREATE,
    KIND_FOLDERS_DELETE, KIND_FOLDERS_DEVICES, KIND_FOLDERS_LEASE_ACQUIRE,
    KIND_FOLDERS_LEASE_RELEASE, KIND_FOLDERS_LEAVE, KIND_FOLDERS_LIST, KIND_FOLDERS_MEMBERS_EVICT,
    KIND_FOLDERS_MEMBERS_LIST, KIND_FOLDERS_MEMBERS_LIST_ACTORS,
    KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE, KIND_FOLDERS_MEMBERS_REMOVE,
    KIND_FOLDERS_MEMBERS_SET_ACCESS, KIND_FOLDERS_PLACES_SET, KIND_FOLDERS_PUBLIC_FETCH,
    KIND_FOLDERS_READ_TOKEN_GET, KIND_FOLDERS_SERVED_ROWS_ADOPT, KIND_FOLDERS_SET_WEB_PAYWALL,
    KIND_FOLDERS_SHARE, KIND_FOLDERS_UPDATE, KIND_FOLDERS_WRITE_TOKEN_GET, LeaseAcquireReply,
    LeaseAcquireRequest, LeaseReleaseReply, LeaseReleaseRequest, MemberRemoveReply,
    MemberRemoveRequest, MembersListReply, MembersListRequest, PlacesSetReply, PlacesSetRequest,
    ServedRowsAdoptReply, ServedRowsAdoptRequest, SyncConflict,
};
use fauna_protocol::{RpcError, decode_strict as decode};
use serde_bytes::ByteBuf;

use crate::db::{ConflictCandidateRow, ResolveWinner};
use crate::routes::{AppState, parse_32_bytes as parse_device_id};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for the `fauna.folders.*` kinds.
const FS: &str = "folders";
/// Error namespace for the `fauna.sync.conflicts.*` kinds.
const SYNC: &str = "sync";

// ── error / encode helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn coded(ns: &str, code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns(ns, code, detail)
}

fn internal(ns: &str, err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(ns, err)
}

/// Parse a wire `name_hash` into a fixed 32-byte array — see
/// `crate::routes::parse_name_hash`.
fn parse_name_hash(ns: &str, name_hash: &Option<ByteBuf>) -> Result<Option<[u8; 32]>, RpcError> {
    crate::routes::parse_name_hash(name_hash, |msg| coded(ns, "invalid_request", msg))
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
async fn require_permission(
    state: &AppState,
    ns: &str,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<(), RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, |e| {
        internal(ns, e)
    })
    .await?;
    Ok(())
}

/// Map a `CacheDb` error onto the twin's status semantics. The twins inspected
/// `e.to_string()` for substrings, but `create_folder_with_options` wraps the
/// rusqlite UNIQUE error in `.context(...)`, so the marker rode the cause chain
/// and `to_string()` (top context only) missed it — the twin's conflict
/// detection was a latent 500 (same class as the events B12b co-host bug). We
/// match the **full** chain (`{e:#}`) so the duplicate-name conflict is
/// detected correctly.
fn map_db_error(ns: &str, e: anyhow::Error) -> RpcError {
    let msg = format!("{e:#}");
    if msg.contains("UNIQUE constraint") {
        coded(ns, "conflict", "folder with that name already exists")
    } else if msg.contains("not found") || msg.contains("not owned") {
        coded(ns, "not_found", msg)
    } else {
        internal(ns, msg)
    }
}

/// Serialize an optional string array to the JSON string the
/// `folders.{include,exclude}_paths` columns store.
fn paths_json(paths: &Option<Vec<String>>) -> Option<String> {
    paths
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
}

/// Parse a stored JSON string-array column back into the wire `Vec<String>`.
///
/// **Verified fold direction** (`nest/common.md` § Unreadable stored values,
/// row 90): an undecodable column folds to `None` — same as a genuinely-NULL
/// column — rather than distinguishing "corrupt" from "never set". This is
/// safe because every production reader of this field is sealed-first
/// (`fauna_core::label_custody::render_include_paths`/`render_exclude_paths`,
/// consulted by both the `fauna-sync` daemon and the shared devices-machine
/// UI projection) and already collapses "plaintext missing/undecodable" into
/// its own `Omit` outcome regardless of cause — the distinction this
/// function's `None` erases was never observable one layer up. Two
/// consequences of `Omit` bound the residual risk: an **already-provisioned**
/// `fauna-sync` daemon keeps its last-known-good local filter (`Omit` means
/// "don't overwrite", never "reset to unfiltered" — see
/// `bins/fauna-sync/src/main.rs::apply_folder_row`), so an authored
/// exclude/include list does not silently vanish for a device that already
/// has it cached. The one gap this doesn't close — a device binding to this
/// set for the **first time** while it is both corrupted and not yet
/// seal-backfilled (S6-c) — inherits the "sync everything" default, same as
/// a genuinely never-restricted set. Closing that gap needs either a
/// three-state wire signal (ruled out — no shared wrapper type) or blocking
/// first sync until paths are known-good (a bootstrap redesign, not a fold
/// change); the ruling's own "corruption is rare, no boot-time integrity
/// sweep, every value is re-authorable from an app" tolerance already prices
/// in this narrow, self-healing (re-save clears it) intersection.
fn parse_paths(stored: Option<&str>) -> Option<Vec<String>> {
    stored.and_then(|s| serde_json::from_str(s).ok())
}

// ── fauna.folders.create (≡ POST /api/v1/file-sets) ────────────────────────

/// Validate a wire set nonce (`mls-group-key-material.md` § M2 → *Writer-signed
/// change records*, ruling (2)): exactly 32 bytes, and never on a reserved
/// (`__`) set — those are out of scope by set class, never bound and never
/// signed. The nest checks the shape only; the value is the client's.
fn checked_set_nonce<'a>(
    nonce: Option<&'a ByteBuf>,
    set_name: &str,
) -> Result<Option<&'a [u8]>, RpcError> {
    let Some(nonce) = nonce else {
        return Ok(None);
    };
    if nonce.len() != fauna_protocol::sync_writer_sig::SET_NONCE_LEN {
        return Err(coded(FS, "invalid_request", "set_nonce must be 32 bytes"));
    }
    if fauna_core::sync::is_reserved_folder_name(set_name) {
        return Err(coded(
            FS,
            "invalid_request",
            "reserved (\"__\") sets carry no set nonce",
        ));
    }
    Ok(Some(&nonce[..]))
}

fn create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_CREATE).await?;
            // A folder has no type (`folders.md` § Target re-model): a create
            // that still carries a `mode` key rides rule-4 `extra` and is
            // ignored — no refusal arm, since no older client exists
            // (`folders.md` § Implementation status today, the design pass).
            let req: FolderCreateRequest = decode(&payload).map_err(malformed)?;

            // Same setter rule as the update handler: a *setter* must not
            // silently degrade an unknown policy (readers degrade → auto).
            if let Some(ref cp) = req.conflict_policy
                && !matches!(cp.as_str(), "auto" | "latest_wins_always")
            {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "conflict_policy must be \"auto\" or \"latest_wins_always\"",
                ));
            }
            // Phase 4 (audience): a folder may be BORN declassified — a website
            // folder created "public" rests plaintext from its first chunk, so
            // no re-seal pass is ever owed. "private" is the default spelled
            // out; "shared" is entered through the share flow (bind + reseal),
            // never spelled at create.
            let create_audience: Option<&str> = match req.audience.as_deref() {
                None | Some("private") => None,
                Some("public") => Some("public"),
                Some("shared") => {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "audience \"shared\" is entered by sharing the folder, not at create",
                    ));
                }
                Some(other) => {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        format!("unknown audience {other:?}: expected \"private\" or \"public\""),
                    ));
                }
            };
            // The reserved (`__`) namespace belongs to the nest, so this kind
            // refuses it UNCONDITIONALLY (`reserved-folders.md` § The management
            // surface refuses the namespace — whole): rails mint at the DB layer
            // (`get_or_create_reserved_folder`), and a custody copy is minted by
            // one of the two nest-side provisioners on the first custody write
            // (`federation_handlers::resolve_backup_custody_set`,
            // `sync_handlers::writable_or_provisioned_backup_set`) — never by a
            // client, which is what makes `folders.custody_copy` a flag no client
            // can set.
            if crate::db::snapshots::is_reserved_folder_name(&req.name) {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "the \"__\" folder namespace is reserved for the nest",
                ));
            }
            // The address and the name must agree while both ride the wire: the
            // nest derives the resting `name_hash` from `name` itself, so a
            // disagreeing hash is a client bug that would otherwise seal the
            // name under a salt nothing addresses the row by.
            //
            // A sealed create travels by hash alone (`path-sealing.md` § the
            // set-name plane): no plaintext name, so the hash is the address
            // and the seal is the only name the row will ever rest. It must
            // carry both, and never for a `public` set, whose name is its URL
            // segment and so rides plaintext.
            let by_hash: Option<[u8; 32]> = if req.name.is_empty() {
                let hash = req
                    .name_hash
                    .as_ref()
                    .and_then(|h| <[u8; 32]>::try_from(&h[..]).ok())
                    .ok_or_else(|| {
                        coded(
                            FS,
                            "invalid_request",
                            "a create names its set by name or by a 32-byte name_hash",
                        )
                    })?;
                if req.name_sealed.is_none() {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a create by name_hash alone must carry name_sealed",
                    ));
                }
                if create_audience == Some("public") {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a public folder's name is its URL segment: create it by name",
                    ));
                }
                Some(hash)
            } else {
                if let Some(ref h) = req.name_hash
                    && h[..] != fauna_core::path_crypto::set_name_hash(&req.name)[..]
                {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "name_hash is not the hash of name",
                    ));
                }
                None
            };

            let opts = crate::db::FolderOptions {
                retention_policy: req.retention_policy.clone(),
                // A create carries no path list: the lists seal under the
                // row id this INSERT mints, so they arrive on the first
                // keyed `fauna.folders.update` (paths are content).
                include_paths: None,
                exclude_paths: None,
                conflict_policy: req.conflict_policy.clone(),
                // Stamped by the shared keyed create (`create_set`); `None`
                // only from a custody-less client, backfilled on update.
                name_sealed: req.name_sealed.as_ref().map(|b| b.to_vec()),
                // Same writer (S6-e). Unlike the path lists, this one
                // *can* ride create at all: its salt is the name's own
                // hash, not the id the INSERT is about to mint.
                retention_policy_sealed: req.retention_policy_sealed.as_ref().map(|b| b.to_vec()),
                audience: create_audience.map(String::from),
                set_nonce: checked_set_nonce(req.set_nonce.as_ref(), &req.name)?
                    .map(<[u8]>::to_vec),
                // A client create is never a custody copy — the flag is
                // the two nest-side provisioners' alone.
                custody_copy: false,
            };
            let id = match by_hash {
                Some(hash) => {
                    state
                        .db
                        .create_sealed_folder_by_hash(hash, &actor_id, opts)
                        .await
                }
                None => {
                    state
                        .db
                        .create_folder_with_options(&req.name, &actor_id, opts)
                        .await
                }
            }
            .map_err(|e| map_db_error(FS, e))?;

            encode_reply(&FolderCreateReply {
                id,
                name: req.name,
                retention_policy: req.retention_policy,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.list (≡ GET /api/v1/file-sets) ───────────────────────────

/// Project the resting nest-place columns onto the wire, or `None` when nothing
/// is set (folders re-model phase 2 § Places → the nest place).
///
/// **Omitting the unset case is the point, not a micro-optimization**: a folder
/// whose owner never touched the policy must serialize byte-identically to how it
/// did before the field existed, so this addition stays a pure widening for every
/// existing folder on the wire exactly as it is at rest.
///
/// Rides **both** projection arms, on the same grading as `retention_policy`
/// beside it: this is set-level policy about what the nest keeps, not a disclosure
/// of the owner's machine — unlike the include/exclude pair, which is owner-only
/// because it is their absolute filesystem layout.
fn nest_place_of(fs: &crate::db::FolderRow) -> Option<fauna_protocol::folders::NestPlacePolicy> {
    let policy = fauna_protocol::folders::NestPlacePolicy {
        snapshots: fs.nest_snapshots,
        quiet_secs: fs.nest_snapshot_quiet_secs,
        extra: Default::default(),
    };
    (!policy.is_unset()).then_some(policy)
}

/// The version-retention bounds off the resting column, under [`nest_place_of`]'s
/// omit-when-unset rule and audience grading (`file-versions.md` § Retention
/// ruling 1: both projections, same grading as `retention_policy`). An
/// unparseable column projects as absent — the *pruner* is the layer that
/// refuses-and-logs it (`backup::version_prune`); a projection has no side
/// channel for "present but unreadable" that would not just invent a policy.
fn version_retention_of(
    fs: &crate::db::FolderRow,
) -> Option<fauna_protocol::folders::VersionRetention> {
    let raw = fs.version_retention.as_deref()?;
    serde_json::from_str::<fauna_protocol::folders::VersionRetention>(raw)
        .ok()
        .filter(|vr| !vr.is_unset())
}

/// The ONE derivation of a folder's wire audience (phase 4 — folders.md
/// § Target re-model). The at-rest `folders.audience` column stores only the
/// owner's explicit **declassification** (`'public'`); bound-ness rests
/// authoritatively in `mls_group_id`, so the tri-state is derived here and
/// nowhere else: public if declassified, else shared if bound, else private.
/// A declassified *bound* folder is `public` — the group survives the public
/// window (it is the write-access roster and what a flip-back to "shared"
/// returns to), but the audience the content rests for is the world.
/// Derive the wire `residency` value (phase 5 — `file-sync.md` § Content
/// residency): the at-rest column stores only the owner's explicit opt-in
/// (`'metadata_only'`); anything else — NULL, or a value a future writer
/// mints that this binary cannot parse — projects as full (the empty string,
/// omitted on the wire). Fail-closed in the direction that matters: only an
/// explicit, parsed opt-in may stop bytes resting.
pub(crate) fn residency_of(fs: &crate::db::FolderRow) -> &'static str {
    if fs.nest_content_residency.as_deref() == Some("metadata_only") {
        "metadata_only"
    } else {
        ""
    }
}

/// The *effective* residency an update handler's request would leave in
/// place — this request's own `residency` flip if present, else the
/// persisted column (phase 5). The pairwise serving refusals read this in
/// both directions off both fields (`residency` here, `audience` inline at
/// its own single call site) so a serve-ON and a residency-flip cannot ride
/// one request in either order.
fn effective_metadata_only(requested_residency: Option<&str>, fs: &crate::db::FolderRow) -> bool {
    match requested_residency {
        Some("metadata_only") => true,
        Some(_) => false, // this same request flips it back to full
        None => fs.nest_content_residency.as_deref() == Some("metadata_only"),
    }
}

/// The stored owner attestation, as served. **Opaque pass-through — the nest
/// verifies nothing** (`encryption-at-rest.md` § Readable classes → *The
/// declassification is owner-ATTESTED*: the seats verify it against the nest,
/// so a check here would protect no one and must never gate anything). A blob
/// that no longer decodes serves as absent, which every seat reads as sealed.
fn served_attestation(
    fs: &crate::db::FolderRow,
) -> Option<fauna_protocol::folders::AudienceAttestation> {
    fs.audience_attestation
        .as_deref()
        .and_then(|blob| fauna_protocol::decode_strict(blob).ok())
}

pub(crate) fn audience_of(fs: &crate::db::FolderRow) -> &'static str {
    if fs.is_public_audience() {
        "public"
    } else if fs.mls_group_id.is_some() {
        "shared"
    } else {
        "private"
    }
}

/// Project one folder's live lease row onto the wire, or `None` when nobody
/// holds it (`file-sync.md` § Exclusive editing).
///
/// The map comes from `CacheDb::live_upload_leases_for`, which already dropped
/// expired rows — this only renames the fields. Kept a pure function of that
/// map so both projection arms answer identically: the holder is a fact about
/// the folder, not about who is asking, and a member who cannot write must
/// still be told why.
///
/// **The device id is disclosed only to the account that holds the lease.** A
/// sync device id is client-asserted, so a holder named to a different account
/// would let that account's client render (and a writer try to impersonate) a
/// device that is not theirs. To any other reader the lease is still reported —
/// the folder IS read-only for them — with an empty `device_id`, which every
/// client already renders as "another device is editing" (it matches no label).
fn lease_state_of(
    leases: &std::collections::HashMap<i64, crate::db::operations::LiveLease>,
    folder_id: i64,
    reader: &[u8; 32],
) -> Option<fauna_protocol::folders::FolderLeaseState> {
    leases
        .get(&folder_id)
        .map(|lease| fauna_protocol::folders::FolderLeaseState {
            device_id: if lease.actor_id.as_slice() == reader.as_slice() {
                hex::encode(&lease.device_id)
            } else {
                String::new()
            },
            expires_at: lease.expires_at,
            extra: Default::default(),
        })
}

/// The derived `ChannelId` of a bound set — the key its envelope and floor
/// rest under — or `None` for an unbound one.
fn channel_of(fs: &crate::db::FolderRow) -> Option<[u8; 32]> {
    fs.mls_group_id
        .as_deref()
        .map(|g| fauna_mls::types::ChannelId::from_group_id(g).0)
}

/// A bound set's content-key floor out of a batch read (`None` when unbound or
/// no floor is established).
fn content_key_floor_of(
    floors: &std::collections::HashMap<[u8; 32], i64>,
    fs: &crate::db::FolderRow,
) -> Option<u64> {
    channel_of(fs).and_then(|channel| content_key_floor_to_wire(floors.get(&channel).copied()))
}

/// The wire form of a stored content-key floor — the ONE conversion for every
/// carrier of it (`fauna.folders.list`'s rows and the federated content-key
/// read). The nest stores it `i64`; a negative value cannot be a generation,
/// and projects as none.
pub(crate) fn content_key_floor_to_wire(floor: Option<i64>) -> Option<u64> {
    floor.and_then(|floor| u64::try_from(floor).ok())
}

/// Project one owner-owned [`FolderRow`] into a `role == "owner"` summary — the
/// caller's own set, with its full config (paths, cadence, cached stats).
fn owner_summary(
    fs: crate::db::FolderRow,
    lease: Option<fauna_protocol::folders::FolderLeaseState>,
    content_key_floor: Option<u64>,
) -> FolderSummary {
    let nest_place = nest_place_of(&fs);
    let version_retention = version_retention_of(&fs);
    let audience = audience_of(&fs).to_string();
    let audience_attestation = served_attestation(&fs);
    let residency = residency_of(&fs).to_string();
    FolderSummary {
        id: fs.id,
        name: fs.name,
        nest_place,
        version_retention,
        retention_policy: fs.retention_policy,
        cached_snapshot_count: fs.cached_snapshot_count,
        cached_total_bytes: fs.cached_total_bytes,
        cached_last_snapshot_at: fs.cached_last_snapshot_at,
        include_paths: parse_paths(fs.include_paths.as_deref()),
        exclude_paths: parse_paths(fs.exclude_paths.as_deref()),
        // Project the shared-set binding so the owner's sync daemon can detect a
        // bound set and load its M2 content keys (5d(c)). The raw group id is not
        // secret — confidentiality is in the per-set content keys.
        mls_group_id: fs.mls_group_id.as_deref().map(hex::encode),
        role: Some("owner".to_string()),
        // The owner has no member-access grant (they own the set) — the
        // reader/writer axis is meaningful only for `role == "member"` rows.
        access: None,
        owner_handle: None,
        // The caller owns this set — it shows a "Shared · N" badge, never a
        // "Shared by ‹…›" one — so there is no owner-display fallback to carry.
        owner_actor_id: None,
        // The owner's per-set WebDAV serve flag — read/written by the owner's
        // Settings → Folders `folder-webdav-toggle` (webdav-server.md
        // § Independent enablement).
        webdav_enabled: fs.webdav_enabled,
        audience,
        // The residency is the nest place's content property (phase 5) — like
        // the audience, it rides both arms; derived above the struct.
        residency,
        // The owner's standing one-device-at-a-time choice, and the live holder
        // beside it. Both ride BOTH arms — see `member_summary` for why a
        // member needs them just as much as the owner does.
        exclusive_editing: fs.exclusive_editing,
        lease,
        // Rides BOTH arms, like the audience it vouches for: a member's engine
        // verifies it against the MLS-authenticated owner before it unseals.
        audience_attestation,
        // Rides BOTH arms: the owner's own capability host holds itself to the
        // floor too — the record gate's owner exemption does not cover it.
        content_key_floor,
        // The owner's website toggle (phase 4) — an owner-side control, like
        // `webdav_enabled` two fields up.
        website_enabled: fs.website_enabled,
        conflict_policy: Some(fs.conflict_policy),
        web_paywall_tier: fs.web_paywall_tier,
        // The sealed name and its salt ride together, always — a seal without its
        // `name_hash` is unrenderable once the plaintext scrubs (`file-sync.md`
        // § Sealed names & paths; the hole S2b hit on `fauna.media.list`).
        name_sealed: fs.name_sealed.map(ByteBuf::from),
        name_hash: fs.name_hash.map(ByteBuf::from),
        // The owner-sealed selective-sync pair (S6-c) rides the OWNER row and
        // only the owner row — see `member_summary` for the other half of that
        // boundary. No salt companion is needed: these seal under `id`, the
        // non-`Option` field at the top of this same struct, so the "carrier
        // without its salt" hole S2b and S4 both hit cannot form here.
        include_paths_sealed: fs.include_paths_sealed.map(ByteBuf::from),
        exclude_paths_sealed: fs.exclude_paths_sealed.map(ByteBuf::from),
        // The sealed retention policy rides BOTH arms, unlike the pair above it
        // — see `member_summary` for the graded reason. Its salt is `name_hash`,
        // already projected a few fields up, so the pair is never separated.
        retention_policy_sealed: fs.retention_policy_sealed.map(ByteBuf::from),
        extra: Default::default(),
        // The nest's trailing copy of the client-minted set nonce — echoed so
        // the owner's reconcile can see when it differs (custody (f)); it selects
        // nothing client-side (c).
        set_nonce: fs.set_nonce.map(ByteBuf::from),
    }
}

/// Project a group-bound set the caller is a *roster member* of (but does NOT
/// own) into a `role == "member"` summary — the "shared with me" row (B3).
///
/// **Least-disclosure boundary:** a member sees only what identifies the shared
/// set and who shared it — `name`, `mls_group_id`, `role`, `owner_handle`, and
/// the owner's `actor_id` (the display fallback for the recipient badge) — plus
/// the set-level metadata (cached content stats). The owner's local
/// selective-sync **paths are withheld** (`include_paths`/`exclude_paths` →
/// `None`): they can leak the owner's filesystem layout, and a member neither
/// syncs the owner's folders nor manages their config. (The owner id is not
/// secret — it identifies the sharer, whom the recipient must already know to
/// have joined the set; it only lets the badge degrade to `short_id` instead of
/// a bare ellipsis when the handle is unresolved.)
fn member_summary(
    fs: crate::db::FolderRow,
    owner: &[u8; 32],
    owner_handle: Option<String>,
    access: Option<String>,
    lease: Option<fauna_protocol::folders::FolderLeaseState>,
    content_key_floor: Option<u64>,
) -> FolderSummary {
    let nest_place = nest_place_of(&fs);
    // The audience is the set's IDENTITY, not an owner-side control — a member
    // of a declassified folder must know their uploads rest world-readable
    // (their own engine takes the plaintext arm off exactly this field).
    let audience = audience_of(&fs).to_string();
    let audience_attestation = served_attestation(&fs);
    // The residency rides the member arm too, and MUST: a writer member's
    // seat uploads bytes exactly as the owner's does, so its skip-the-bytes
    // gate reads this field off its own projection (phase 5 — same reasoning
    // as the audience above).
    let residency = residency_of(&fs).to_string();
    // Same grading as `retention_policy` below (file-versions.md § Retention
    // ruling 1): set-level policy about what the nest keeps, not a disclosure
    // of the owner's machine.
    let version_retention = version_retention_of(&fs);
    FolderSummary {
        id: fs.id,
        name: fs.name,
        nest_place,
        version_retention,
        retention_policy: fs.retention_policy,
        cached_snapshot_count: fs.cached_snapshot_count,
        cached_total_bytes: fs.cached_total_bytes,
        cached_last_snapshot_at: fs.cached_last_snapshot_at,
        // Owner's local paths are NOT disclosed to a member (filesystem-layout
        // leak) — a member receives content, they do not sync the owner's folders.
        include_paths: None,
        exclude_paths: None,
        mls_group_id: fs.mls_group_id.as_deref().map(hex::encode),
        role: Some("member".to_string()),
        // The caller's reader/writer grant on this shared set (multi-writer
        // Phase 1) — `Some("writer")` lets the client bind folders + sync
        // read-write; absent/reader ⇒ read-only. Looked up from
        // `folder_member_access` in `list_core`.
        access,
        // Empty ("no handle set") folds to `None` so the client shows no badge
        // rather than a blank "Shared by".
        owner_handle: owner_handle.filter(|h| !h.is_empty()),
        // The owner's actor id — the recipient badge's display fallback when the
        // handle above is unresolved (cross-nest / handle-unset owner). The
        // client-side transcribe folds handle + this into one `owner_display`.
        owner_actor_id: Some(hex::encode(owner)),
        // The WebDAV serve flag is an OWNER-side control (whether the owner's MDA
        // serves the set); a member neither owns the row nor serves the owner's
        // set, so it is not disclosed here — same least-disclosure boundary as the
        // owner's local paths above.
        webdav_enabled: false,
        // Rides the member arm — see the derivation note above the struct.
        audience,
        // Rides the member arm — see the derivation note above the struct.
        residency,
        // NOT withheld, unlike the owner-side serving controls below, and for
        // the same reason the residency above is not: a writer member's seat
        // uploads exactly as the owner's does, so it must know to take the
        // lease — and a READER member, who can never acquire one at all
        // (`lease.acquire` is writable-folder-gated), has no other way to learn
        // why the folder is read-only. Withholding the holder here would leave
        // that seat with a refusal it cannot explain.
        exclusive_editing: fs.exclusive_editing,
        lease,
        // Rides BOTH arms, like the audience it vouches for: a member's engine
        // verifies it against the MLS-authenticated owner before it unseals.
        audience_attestation,
        // NOT withheld: a writer member seals, so it must know the generation
        // it may seal under, and the envelope it opens already carries every
        // generation up to this one — the floor discloses nothing new.
        content_key_floor,
        // The website toggle is the owner's serving control — withheld, same
        // boundary as `webdav_enabled` above. (What a member's engine needs —
        // "does my upload rest plaintext" — is the audience, which rides.)
        website_enabled: false,
        // The conflict policy is likewise the owner's sync-behavior control —
        // members are read-only and never run the owner's resolver.
        conflict_policy: None,
        // The paywall tier is the owner's monetization control — same
        // least-disclosure boundary as the two owner-side controls above.
        web_paywall_tier: None,
        // NOT withheld, unlike the owner-side controls above: a roster member is
        // exactly the audience the name is sealed *to* (they hold the set's M2
        // content keys), and they already receive the plaintext `name` on this row
        // today. Withholding the seal would simply blank a member's set name at
        // the flip. The salt rides with it for the usual reason.
        name_sealed: fs.name_sealed.map(ByteBuf::from),
        name_hash: fs.name_hash.map(ByteBuf::from),
        // WITHHELD, and this is the one place the set-name precedent above must
        // NOT be copied (path-sealing S6-c). The two
        // fields sit three lines apart and pull opposite ways: a roster member is
        // the audience the *name* is sealed to, but they are not the audience for
        // the owner's local filesystem layout — which is why the plaintext
        // `include_paths`/`exclude_paths` are already `None` above. Shipping the
        // sealed pair here would widen disclosure in the name of sealing, and it
        // seals under the owner's root anyway, so a member could not open it —
        // they would receive an unopenable blob whose mere presence discloses
        // that filters exist. Defence in depth behind that root choice
        // (`fauna_core::label_custody::seal_include_paths` takes a `BackupKey`
        // precisely so the member-openable root is unrepresentable).
        include_paths_sealed: None,
        exclude_paths_sealed: None,
        // NOT withheld — and the contrast with the two lines above is the whole
        // point (path-sealing S6-e). A member receives `retention_policy`'s
        // plaintext on this very row (see the field, well above), so a member is
        // inside this seal's audience the same way they are inside the *name*'s;
        // withholding it — or sealing it under the owner root — would blank a
        // member's retention display the moment the plaintext scrubs. That is a
        // NARROWING, which arrives disguised as hardening and is no more
        // sanctioned than the widening caught three lines up. The rule the
        // two neighbours jointly establish: grade the field's *current* readers,
        // never the nearest precedent.
        retention_policy_sealed: fs.retention_policy_sealed.map(ByteBuf::from),
        extra: Default::default(),
        // Both arms echo it (custody (f)); a member's verifying copy arrives in
        // the owner-sealed content-key envelope (h), never from here.
        set_nonce: fs.set_nonce.map(ByteBuf::from),
    }
}

/// Enumeration of the caller's user-facing folders for `fauna.folders.list`.
/// Extracted from [`list_handler`] (mirroring the `_core` split of `share_core` /
/// `actor_members_list_core`) so the projection is unit-testable.
///
/// - **Owner rows (always):** the caller's own sets (`role == "owner"`), bound or
///   not. Reserved internal sets (`__drafts`, `__mail`, `__index`,
///   `__conv/*`, …) are excluded — they are not user backup targets and must not
///   appear in the management surface (`file-sync.md` § Reserved folders).
///   `is_reserved_folder_name` is the single source of truth for the `__`
///   convention; the shared `get_folders_for_actor_full` query still returns
///   them for internal consumers (export, conflict id→name resolution).
/// - **Member rows (`include_shared_with_me` only):** group-bound sets the caller
///   is a **roster member** of but does not own (`role == "member"`), the B3
///   member-list-visibility projection. The strict membership boundary
///   (`is_actor_in_channel`, the same gate `resolve_readable_folder` applies) —
///   deliberately **no** admin broad grant (this is the personal folders page,
///   not media discovery). Reserved `__conv/*` sets (the caller's own
///   conversations, which are also group-bound roster channels) are excluded, so
///   a DM/group never surfaces here.
///
/// ⚠ The opt-in gate keeps the owner-scoped contract the sync daemons + the
/// owner-side author flow rely on **byte-identical** (they never set the flag).
/// ⚠ A `role == "member"` row is only *rostered* — the nest cannot observe an MLS
/// join. The **client** must filter to sets it has actually joined
/// (`has_group(ChannelId::from_group_id(mls_group_id))`); see `FolderSummary.role`.
async fn list_core(
    db: &crate::db::CacheDb,
    actor_id: &[u8; 32],
    include_shared_with_me: bool,
) -> Result<FoldersListReply, RpcError> {
    let owned = db
        .get_folders_for_actor_full(actor_id)
        .await
        .map_err(|e| internal(FS, e))?;
    let owned: Vec<_> = owned
        .into_iter()
        .filter(|fs| !crate::db::snapshots::is_reserved_folder_name(&fs.name))
        .collect();
    // ONE lease read for the whole owner pass, not one per row: a folder list
    // is polled on a tick by every seat of every account, and a per-row query
    // here would scale that tick with the user's folder count.
    let owner_ids: Vec<i64> = owned.iter().map(|fs| fs.id).collect();
    let owner_leases = db
        .live_upload_leases_for(&owner_ids)
        .await
        .map_err(|e| internal(FS, e))?;
    // The same one-read rule for the bound sets' content-key floors.
    let owner_channels: Vec<[u8; 32]> = owned.iter().filter_map(channel_of).collect();
    let owner_floors = db
        .content_key_floors_for(&owner_channels)
        .await
        .map_err(|e| internal(FS, e))?;
    let mut folders: Vec<FolderSummary> = owned
        .into_iter()
        .map(|fs| {
            let lease = lease_state_of(&owner_leases, fs.id, actor_id);
            let floor = content_key_floor_of(&owner_floors, &fs);
            owner_summary(fs, lease, floor)
        })
        .collect();

    if include_shared_with_me {
        // Collected before building, so the member pass takes one lease read
        // too rather than one per shared set.
        let mut member_rows: Vec<(
            crate::db::FolderRow,
            [u8; 32],
            Option<String>,
            Option<String>,
        )> = Vec::new();
        for fs in db
            .get_group_bound_folders()
            .await
            .map_err(|e| internal(FS, e))?
        {
            // Skip the caller's own sets (already in the owner pass) and reserved
            // `__conv/*` (the caller's conversations, not folders).
            if fs.actor_id.as_slice() == actor_id.as_slice()
                || crate::db::snapshots::is_reserved_folder_name(&fs.name)
            {
                continue;
            }
            // NOT-NULL by the query, but stay defensive.
            let Some(group_id) = fs.mls_group_id.as_deref() else {
                continue;
            };
            let channel_id = fauna_mls::types::ChannelId::from_group_id(group_id).0;
            if !db
                .is_actor_in_channel(actor_id, &channel_id)
                .await
                .map_err(|e| internal(FS, e))?
            {
                continue;
            }
            let owner = <[u8; 32]>::try_from(fs.actor_id.as_slice())
                .map_err(|_| coded(FS, "internal", "folder owner id is not 32 bytes"))?;
            let owner_handle = db.get_handle(&owner).await.map_err(|e| internal(FS, e))?;
            // The caller's reader/writer grant on this shared set (multi-writer
            // Phase 1). Absent `folder_member_access` row ⇒ reader (the fail-safe);
            // only an explicit `writer` makes the set bindable in the client.
            let access = db
                .get_folder_member_role(&channel_id, actor_id)
                .await
                .map_err(|e| internal(FS, e))?
                .map(|role| role.access);
            member_rows.push((fs, owner, owner_handle, access));
        }
        let member_ids: Vec<i64> = member_rows.iter().map(|(fs, ..)| fs.id).collect();
        let member_leases = db
            .live_upload_leases_for(&member_ids)
            .await
            .map_err(|e| internal(FS, e))?;
        let member_channels: Vec<[u8; 32]> = member_rows
            .iter()
            .filter_map(|(fs, ..)| channel_of(fs))
            .collect();
        let member_floors = db
            .content_key_floors_for(&member_channels)
            .await
            .map_err(|e| internal(FS, e))?;
        for (fs, owner, owner_handle, access) in member_rows {
            let lease = lease_state_of(&member_leases, fs.id, actor_id);
            let floor = content_key_floor_of(&member_floors, &fs);
            folders.push(member_summary(
                fs,
                &owner,
                owner_handle,
                access,
                lease,
                floor,
            ));
        }
    }

    Ok(FoldersListReply {
        folders,
        extra: Default::default(),
    })
}

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_LIST).await?;
            let req: FoldersListRequest = decode(&payload).map_err(malformed)?;
            let reply = list_core(
                &state.db,
                &actor_id,
                req.include_shared_with_me.unwrap_or(false),
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.update (≡ PUT /api/v1/file-sets/{name}) ───────────────────

fn update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_UPDATE).await?;
            let req: FolderUpdateRequest = decode(&payload).map_err(malformed)?;

            // Conflict policy takes exactly the ConflictPolicy wire strings
            // (file-sync.md § Conflicts). Unknown values are rejected here (a
            // *setter* must not silently degrade) even though *readers*
            // degrade unknown → auto for forward compat.
            if let Some(ref cp) = req.conflict_policy
                && !matches!(cp.as_str(), "auto" | "latest_wins_always")
            {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "conflict_policy must be \"auto\" or \"latest_wins_always\"",
                ));
            }

            // S5b (`file-sync.md` § Sealed names & paths): resolve hash-first,
            // once, unconditionally — every downstream reserved-name check and
            // the update itself key off the resolved `fs.name`, never `req.name`
            // (which is empty on a hash-addressed request). A missing set errors
            // here instead of falling through to the update's `updated == false`
            // path; same observable `not_found`, detected earlier.
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let fs = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            // The reserved (`__`) namespace belongs to the nest: NO field of a
            // reserved set is a client's to set (`reserved-folders.md` § The
            // management surface refuses the namespace — whole). One refusal on
            // the RESOLVED name — a hash-addressed request carries none — replaces
            // the former per-field ones (a mode change, a WebDAV / website serve-on,
            // an audience or residency change, a lease, a name seal): serving a
            // rail would let `webdav_record_change` write chunked manifests into
            // its `sync_changes` (the § 9 direct-blob hazard), and a rail's name is
            // a routing constant that never seals.
            if crate::db::snapshots::is_reserved_folder_name(&fs.name) {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "reserved (\"__\") folders belong to the nest; no field of one can be \
                     changed",
                ));
            }

            // WebDAV serving (`webdav-server.md` § What the namespace is): every
            // non-reserved folder may be served — the reserved refusal above is
            // the only namespace rule. Serve-OFF is refused only while the
            // owner has not yet adopted the served era's rows (below).
            if req.webdav_enabled == Some(false) && fs.webdav_enabled {
                // `writer-signed-change-records.md` ruling (7)(b): once the
                // flag falls, every reader stops exempting the set's WebDAV
                // pseudo-device rows, so an adoptable one still unsigned would
                // vanish from every app. The count is ruling (7)(b)(i)(2)'s —
                // the adoptable rows only, so a row the owner's sweep must not
                // sign never holds the flag.
                let unadopted = unadopted_served_rows(&state, &fs).await?;
                if unadopted > 0 {
                    return Err(served_rows_unadopted(unadopted));
                }
            }
            if req.webdav_enabled == Some(true) {
                // Never on a public-audience folder (phase 4): DAV serving
                // conveys the set's M2 content key to the MDA, and a public
                // folder has none — its content rests plaintext. The *effective*
                // audience (this request's flip if present, else the persisted
                // state) decides, so serve-ON + declassify cannot ride one
                // request in either order.
                let effective_public = match req.audience.as_deref() {
                    Some("public") => true,
                    Some(_) => false, // this same request flips it back
                    None => fs.is_public_audience(),
                };
                if effective_public {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        // Every cross-toggle refusal names the repair
                        // (`ui/folders.md` § Audience and website serving —
                        // "the refusal text names the repair"); this arm is
                        // the one that used to state only the conflict.
                        "a public folder cannot be served over WebDAV; its content rests \
                         unsealed and DAV serving is content-key-sealed — make the folder \
                         private or shared first",
                    ));
                }
                // …and never on a metadata-only folder (phase 5): a DAV mount
                // reads bytes off the nest store, which deliberately holds
                // none. Effective state, like the audience above.
                if effective_metadata_only(req.residency.as_deref(), &fs) {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a metadata-only folder cannot be served over WebDAV; its content \
                         does not rest on the nest — set residency back to full first",
                    ));
                }
            }

            // Phase 4 — audience transitions (`folders.md` § Target re-model;
            // `principles.md` § The user always controls their data owns the
            // public exception). The column stores only the DECLASSIFICATION;
            // the wire tri-state maps onto stamp/clear with bound-ness
            // consistency enforced here, in the one writer. The nest records
            // the state flip only — un-sealing (declassify) and re-sealing
            // (flip-back) of the back-catalogue are client-driven.
            let audience_param: Option<Option<&str>> = match req.audience.as_deref() {
                None => None,
                Some(requested) => {
                    match requested {
                        "public" => {
                            // Declassify. Refused while WebDAV-served (DAV serving
                            // is M2-sealed — the mirror of the serve-ON gate above)
                            // and while paywalled (paywalled web content is
                            // `shared`-audience machinery by ratified design; a
                            // world-readable paywall is no paywall).
                            if req.webdav_enabled.unwrap_or(fs.webdav_enabled) {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a WebDAV-served folder cannot be made public; turn off \
                                     WebDAV serving first",
                                ));
                            }
                            if fs.web_paywall_tier.is_some() {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a paywalled folder cannot be made public; clear the \
                                     paywall first",
                                ));
                            }
                            Some(Some("public"))
                        }
                        // Flip-backs clear the column; the derived state must
                        // match what the caller asked for, so a bound folder
                        // cannot be talked into "private" (its members keep
                        // their access) nor an unbound one into "shared" (the
                        // share flow is what binds).
                        "private" => {
                            if fs.mls_group_id.is_some() {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a group-bound folder flips back to \"shared\", not \
                                     \"private\"",
                                ));
                            }
                            Some(None)
                        }
                        "shared" => {
                            if fs.mls_group_id.is_none() {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "audience \"shared\" is entered by sharing the folder; an \
                                     unbound folder flips back to \"private\"",
                                ));
                            }
                            Some(None)
                        }
                        other => {
                            return Err(coded(
                                FS,
                                "invalid_request",
                                format!(
                                    "unknown audience {other:?}: expected \"private\", \
                                     \"shared\", or \"public\""
                                ),
                            ));
                        }
                    }
                }
            };

            // Phase 5 — content residency (`file-sync.md` § Content residency).
            // The column stores only the owner's explicit opt-in
            // (`'metadata_only'`); NULL = full. The nest records the state flip
            // (and, on the →metadata_only consent, drops its chunk bytes for
            // the folder — spawned after the commit below); the seats' upload
            // gates and the GC's expected-absence ride the projected value.
            let residency_param: Option<Option<&str>> = match req.residency.as_deref() {
                None => None,
                Some(requested) => {
                    match requested {
                        "full" => Some(None),
                        "metadata_only" => {
                            // Pairwise serving refusals, this direction
                            // (metadata_only moves second; each serving arm
                            // holds the other direction): every serving
                            // surface reads bytes off the nest store, and a
                            // site or DAV mount that is up only while the
                            // owner's laptop is on is a broken serving
                            // promise. Effective state — this request's flip
                            // if present, else persisted — so toggle +
                            // residency cannot ride one request in either
                            // order.
                            if req.website_enabled.unwrap_or(fs.website_enabled) {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a website-serving folder cannot go metadata-only; turn \
                                     off website serving first",
                                ));
                            }
                            if req.webdav_enabled.unwrap_or(fs.webdav_enabled) {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a WebDAV-served folder cannot go metadata-only; turn off \
                                     WebDAV serving first",
                                ));
                            }
                            if fs.web_paywall_tier.is_some() {
                                return Err(coded(
                                    FS,
                                    "invalid_request",
                                    "a paywalled folder cannot go metadata-only; clear the \
                                     paywall first",
                                ));
                            }
                            // The interim cross-nest refusal, this direction
                            // (`file-sync.md` § Relay serving → *Until that
                            // leg is built, the pair is refused*;
                            // `welcome_deliver_core` holds the other): a
                            // member on another nest can fetch nothing by
                            // relay yet, so the flip is refused while the
                            // folder's channel holds a foreign-member row.
                            // Only the FLIP — a folder already metadata-only
                            // is an existing pair and is left as it is.
                            if fs.nest_content_residency.as_deref() != Some("metadata_only")
                                && let Some(group_id) = fs.mls_group_id.as_deref()
                            {
                                let channel =
                                    fauna_mls::types::ChannelId::from_group_id(group_id).0;
                                let foreign_members = state
                                    .db
                                    .list_foreign_channel_members(&channel)
                                    .await
                                    .map_err(|e| internal(FS, e))?;
                                if !foreign_members.is_empty() {
                                    return Err(coded(
                                        FS,
                                        "invalid_request",
                                        "a folder with a member on another nest cannot go \
                                         metadata-only yet: that member could not reach its \
                                         content; remove them first",
                                    ));
                                }
                            }
                            Some(Some("metadata_only"))
                        }
                        other => {
                            return Err(coded(
                                FS,
                                "invalid_request",
                                format!(
                                    "unknown residency {other:?}: expected \"full\" or \
                                     \"metadata_only\""
                                ),
                            ));
                        }
                    }
                }
            };

            // Exclusive editing (v72 — `file-sync.md` § Exclusive editing): the
            // owner's standing "one device at a time may write" choice. OFF is
            // always allowed; ON is refused on a reserved `__` rail, the same
            // rule the serving toggles below apply and for a sharper reason —
            // a rail is nest-side infrastructure with no user at a keyboard, so
            // there are no two devices to coordinate and a lease could only
            // wedge it.
            //
            // Deliberately NOT cross-gated against anything else on this row.
            // It does not touch bytes, serving, audience or residency, and it
            // is explicitly NOT a conflict-policy variant: the policy decides
            // what happens after a divergence, this tries to stop one before,
            // and a lease-governed folder still needs a policy for what a lease
            // cannot cover (an offline edit, an expired lease, a member who
            // never took one). Both stay meaningful at once.
            // The owner's audience attestation: SHAPE-checked only (a bound on
            // what the row can hold), never verified — see `served_attestation`.
            let attestation_blob = match req.audience_attestation.as_ref() {
                None => None,
                Some(att) if att.owner.len() == 32 && att.sig.len() == 64 => {
                    Some(fauna_protocol::encode_canonical(att).map_err(|e| internal(FS, e))?)
                }
                Some(_) => {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "audience_attestation: owner must be 32 bytes and sig 64",
                    ));
                }
            };
            let exclusive_editing_param = req.exclusive_editing;

            // Phase 4 — the website toggle. OFF is always allowed; ON is refused
            // on a metadata-only folder (phase 5 — the nest holds no bytes to
            // serve). It is deliberately NOT audience-gated: a paywalled
            // website is sealed (`shared`-audience machinery), and a sealed
            // un-paywalled website simply serves nothing (the serve walk fails
            // closed) until the owner declassifies or sets a tier — the app UI
            // leads that choice.
            if req.website_enabled == Some(true) {
                // The other direction of the phase-5 pairwise refusal: serving
                // moves second onto a metadata-only folder. Effective state,
                // as everywhere in this handler.
                if effective_metadata_only(req.residency.as_deref(), &fs) {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a metadata-only folder cannot serve a website; its content does \
                         not rest on the nest — set residency back to full first",
                    ));
                }
            }

            // The owner's overwrite of the stored set nonce (custody (f)). The
            // resolve above is owner-scoped, so a member never reaches here.
            let set_nonce = checked_set_nonce(req.set_nonce.as_ref(), &fs.name)?;

            // wire `Some(v)` → set, `None` → leave unchanged.
            let include_str = paths_json(&req.include_paths);
            let exclude_str = paths_json(&req.exclude_paths);
            // `retention_policy` plaintext rests deliberately (the ARMED
            // auto-prune ruling, `encryption-at-rest.md` § Carve-outs: the
            // nest parses its numeric knobs server-side — not a label; the
            // sealed sibling is the display copy).
            let retention_param: Option<Option<&str>> = req.retention_policy.as_deref().map(Some);
            // S9 flip, selective-sync plaintext: a KEYED writer (plaintext +
            // seal together) rests the seal only — the plaintext column
            // CLEARS in the same write. A KEYLESS writer of a non-empty list
            // is refused (paths are content, `encryption-at-rest.md`
            // § Carve-outs): no create carries a list any more, and the one
            // durable editor (`DevicesMachine::set_folder_paths`) seals
            // whenever it holds the owner's key, so nothing legitimate rests
            // plaintext. An empty list names no path and still clears keyless.
            for (paths, sealed, which) in [
                (
                    &req.include_paths,
                    &req.include_paths_sealed,
                    "include_paths",
                ),
                (
                    &req.exclude_paths,
                    &req.exclude_paths_sealed,
                    "exclude_paths",
                ),
            ] {
                if paths.as_ref().is_some_and(|p| !p.is_empty()) && sealed.is_none() {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        format!("{which} rests only sealed: send it with its seal"),
                    ));
                }
            }
            let include_param: Option<Option<&str>> =
                match (&req.include_paths, &req.include_paths_sealed) {
                    (Some(_), Some(_)) => Some(None),
                    (Some(_), None) => include_str.as_deref().map(Some),
                    (None, _) => None,
                };
            let exclude_param: Option<Option<&str>> =
                match (&req.exclude_paths, &req.exclude_paths_sealed) {
                    (Some(_), Some(_)) => Some(None),
                    (Some(_), None) => exclude_str.as_deref().map(Some),
                    (None, _) => None,
                };

            // NB the reserved-`__` refusal above is deliberately NOT extended to
            // the selective-sync seals, and the asymmetry is principled rather
            // than an omission: a sealed `name` would be unreadable to the ~28
            // nest decision sites that ROUTE on the literal reserved names, while
            // `include_paths`/`exclude_paths` are store-and-serve columns the nest
            // never parses — sealing one breaks no nest logic. (A reserved rail
            // has no user-chosen selective-sync lists to begin with; the nest's
            // own `get_or_create_*` paths never set them.)
            //
            // ⚠ The selective-sync pair moves TOGETHER (path-sealing S6-c, the
            // `register_sync_device`/`label_sealed` rule from S6-b). A request
            // that writes the plaintext also writes the seal — *including* when
            // it carries none, which clears a stale seal rather than leaving a
            // row whose plaintext says one thing and whose seal opens to the list
            // it replaced (post-flip the user would be shown the stale filesystem
            // layout with nothing failing). A seal-only request — no plaintext —
            // stamps in place, which is the S8 backfill shape.
            //
            // `Option<ByteBuf>::as_deref()` yields `Option<&Vec<u8>>`, not
            // `Option<&[u8]>` — the S2 papercut, twice.
            let include_sealed_param: Option<Option<&[u8]>> =
                match (&req.include_paths, &req.include_paths_sealed) {
                    (Some(_), sealed) => Some(sealed.as_ref().map(|b| &b[..])),
                    (None, Some(sealed)) => Some(Some(&sealed[..])),
                    (None, None) => None,
                };
            let exclude_sealed_param: Option<Option<&[u8]>> =
                match (&req.exclude_paths, &req.exclude_paths_sealed) {
                    (Some(_), sealed) => Some(sealed.as_ref().map(|b| &b[..])),
                    (None, Some(sealed)) => Some(Some(&sealed[..])),
                    (None, None) => None,
                };
            // The retention pair moves together on the identical rule (S6-e) —
            // a plaintext save always writes the seal slot, so a keyless writer
            // clears rather than strands a seal opening to the policy it
            // replaced; a seal-only request stamps in place for S8.
            let retention_sealed_param: Option<Option<&[u8]>> =
                match (&req.retention_policy, &req.retention_policy_sealed) {
                    (Some(_), sealed) => Some(sealed.as_ref().map(|b| &b[..])),
                    (None, Some(sealed)) => Some(Some(&sealed[..])),
                    (None, None) => None,
                };

            // The nest place is sent whole and applied whole: `Some(policy)`
            // replaces both knobs, so a knob absent from the struct CLEARS back to
            // unset (the nest-wide default) rather than lingering. That is the
            // only way three states per knob survive a wire that cannot
            // round-trip a nested Option — `NestPlacePolicy`'s own docs carry the
            // reasoning. `None` here leaves the whole policy untouched.
            let (nest_snapshots_param, nest_quiet_param) = match &req.nest_place {
                Some(place) => {
                    // Refuse rather than normalize: a negative quiet period is a
                    // caller bug, and silently clamping it to 0 would turn "wait
                    // for quiet" into "cut on every tick" without telling anyone.
                    if place.quiet_secs.is_some_and(|q| q < 0) {
                        return Err(coded(
                            FS,
                            "bad_request",
                            "nest_place.quiet_secs must not be negative",
                        ));
                    }
                    // Bounded above too: an owner's own value reaches the
                    // nest-wide scheduler, and an unbounded one panicked it for
                    // every folder (2026-10-08).
                    if place.quiet_secs.is_some_and(|q| {
                        q > fauna_protocol::folders::NestPlacePolicy::MAX_QUIET_SECS
                    }) {
                        return Err(coded(
                            FS,
                            "bad_request",
                            "nest_place.quiet_secs must not exceed seven days",
                        ));
                    }
                    (Some(place.snapshots), Some(place.quiet_secs))
                }
                None => (None, None),
            };

            // Version retention is sent whole and applied whole like the nest
            // place above (file-versions.md § Retention ruling 1). A
            // binds-nothing policy (both bounds 0 — the editor's "clear"
            // gesture) rests as SQL NULL, the honest keep-everything value —
            // never as a zero-JSON that would make NotSet two shapes at rest.
            let version_retention_json: Option<Option<String>> =
                req.version_retention.as_ref().map(|vr| {
                    (!vr.is_unset()).then(|| {
                        serde_json::to_string(&fauna_protocol::folders::VersionRetention {
                            max_versions_per_path: vr.max_versions_per_path,
                            max_age_days: vr.max_age_days,
                            // Canonical column JSON: unknown wire extras are
                            // forward-compat padding, not policy — resting them
                            // would make this nest's canonical shape depend on
                            // a newer client's vocabulary.
                            extra: Default::default(),
                        })
                        .expect("2-field struct serializes")
                    })
                });

            // A public folder's name is its URL segment and rests plaintext
            // (`encryption-at-rest.md` § Carve-outs), so a →public flip of a set
            // whose name rests only sealed must restore it — and only the caller
            // holds it: the request's `name`, checked against the row's hash so
            // the URL can never differ from the set it publishes
            // (`path-sealing.md` § the set-name plane).
            let restored_name: Option<&str> = match audience_param {
                Some(Some("public")) if fs.name.is_empty() => {
                    let named = !req.name.is_empty()
                        && fs.name_hash.as_deref()
                            == Some(&fauna_core::path_crypto::set_name_hash(&req.name)[..]);
                    if !named {
                        return Err(coded(
                            FS,
                            "invalid_request",
                            "making a folder public publishes its name as its address; send \
                             the folder's name with the change",
                        ));
                    }
                    Some(req.name.as_str())
                }
                _ => None,
            };
            let updated = state
                .db
                .update_folder_by_id(
                    fs.id,
                    crate::db::FolderUpdate {
                        retention_policy: retention_param,
                        include_paths: include_param,
                        exclude_paths: exclude_param,
                        webdav_enabled: req.webdav_enabled,
                        conflict_policy: req.conflict_policy.as_deref(),
                        name_sealed: req.name_sealed.as_ref().map(|b| &b[..]),
                        name: restored_name,
                        include_paths_sealed: include_sealed_param,
                        exclude_paths_sealed: exclude_sealed_param,
                        retention_policy_sealed: retention_sealed_param,
                        nest_snapshots: nest_snapshots_param,
                        nest_snapshot_quiet_secs: nest_quiet_param,
                        version_retention: version_retention_json
                            .as_ref()
                            .map(|opt| opt.as_deref()),
                        audience: audience_param,
                        website_enabled: req.website_enabled,
                        residency: residency_param,
                        exclusive_editing: exclusive_editing_param,
                        audience_attestation: attestation_blob.as_deref(),
                        set_nonce,
                    },
                )
                .await
                .map_err(|e| internal(FS, e))?;
            if !updated {
                return Err(coded(FS, "not_found", "folder not found"));
            }
            // Phase 5: the →metadata_only consent DROPS the nest's chunk bytes
            // for this folder, now — R10 (account-data-plane.md § The ratified decisions)'s consent arm, applied at every seat
            // count (`file-sync.md` § Content residency; the owner's confirm
            // named exactly this). Spawned: the walk is GC-scale work no WS
            // deadline should carry, and crash-safety needs no second signal —
            // the committed column IS the decision point, and the GC's
            // residency arm reclaims anything an interrupted pass left (the
            // boot-reconcile shape, `nest/common.md` § Client-state
            // recoverability). Best-effort + logged, like the re-render below.
            let residency_flipped_on = matches!(residency_param, Some(Some("metadata_only")))
                && fs.nest_content_residency.as_deref() != Some("metadata_only");
            if residency_flipped_on && let Some(svc) = state.backup_service.clone() {
                let db = state.db.clone();
                let folder_id = fs.id;
                let folder_name = fs.name.clone();
                // SCOPED, not bare: this task carries `svc.encryption_key()`,
                // which is exactly the material the deployment-seed rotation's
                // generation teardown supersedes. A bare `tokio::spawn` here
                // survives that teardown holding the old key with every other
                // gate green — the failure
                // `state::tests::boot_worker_spawns_are_generation_scoped_or_marked`
                // exists to catch. Scoped rather than marked for the same
                // reason: no marker class fits a task that outlives its request
                // AND holds key material.
                // The record walk's post-body source (`gc.rs` step 2f) — the
                // same oracle every blob-deleting caller sweeps against.
                let post_segments = state.post_segments.clone();
                let scope = state.clone();
                scope.spawn_scoped(async move {
                    let store = svc.local_blob_store();
                    match crate::backup::gc::drop_folder_chunk_bytes(
                        &db,
                        &store,
                        crate::backup::gc::PostBodySource {
                            segments: &post_segments,
                        },
                        1800, // the production GC grace — pins more, never less
                        svc.encryption_key(),
                        folder_id,
                    )
                    .await
                    {
                        Ok(n) => tracing::info!(
                            folder = %fauna_core::log_redact::log_folder_name(&folder_name),
                            dropped = n,
                            "metadata-only flip: nest-held chunk bytes dropped"
                        ),
                        Err(e) => tracing::warn!(
                            folder = %fauna_core::log_redact::log_folder_name(&folder_name),
                            error = %e,
                            "metadata-only flip: chunk-byte drop failed; the GC's residency \
                             arm reclaims on its next pass"
                        ),
                    }
                });
            }
            // A SERVING transition — the website toggle changing, or
            // the audience entering/leaving `public` — re-renders the actor's
            // site. The rendered half cannot be gated at its serve arm (a
            // `web_rendered` row has no folder provenance, and a folder-less
            // default site is legitimate), so the transition fires the
            // idempotent re-render, whose folder-gated inputs drop a
            // no-longer-serving folder's templates and whose whole-site replacement
            // drops their stale pages. Both directions fire: the ON legs
            // restore a re-enabled site without waiting for an unrelated
            // trigger. The update's own transaction recorded the owed render
            // (`db/sync_storage.rs`), and the render below is keyed on that
            // marker rather than on `website_changed || audience_changed`, so
            // a RETRY of an update torn before its render — which changes
            // nothing — still renders. It fails closed: a transition that
            // takes a folder off the site is a revoke, and a render that
            // errors clears the site rather than keep serving the folder's
            // pages (`web-content-hosting.md` § Routing, render, serving →
            // *A revoke is durable*). Logged — the folder row is the
            // authoritative state and already committed.
            // Whether this folder's plaintext files serve — the render-input
            // gate's OWN predicate, either side of the update
            // (`FolderRow::serves_plaintext_web_files`). Decided here the same
            // way `update_folder_for_user` decides the owed-render mark, so the
            // backfill and the mark can never disagree about what a serving
            // transition IS (the retired `mode` leg once moved this gate while
            // a watch on `website_enabled`/`audience` alone missed it; asking the one predicate keeps
            // any future column covered).
            let served_before = fs.serves_plaintext_web_files();
            let served_after = crate::db::serves_plaintext_web_files(
                req.website_enabled.unwrap_or(fs.website_enabled),
                audience_param.unwrap_or(fs.audience.as_deref()),
            );
            // Row 182 — the enable-time backfill: a serving transition landing
            // in an enabled state (re)builds this folder's `web_files`
            // projection from its live plaintext sync heads BEFORE the
            // transition re-render below reads them (`web-content-hosting.md`
            // § Content model owns the projection claim). Best-effort, logged
            // — the folder row is authoritative and already committed, the
            // same posture as the render.
            if !served_before
                && served_after
                && let Err(e) = crate::web_files_projection::reconcile_web_files_projection(
                    &state.db, &actor_id, fs.id,
                )
                .await
            {
                tracing::warn!("web_files reconcile after folder serving transition failed: {e}");
            }
            if let Some(wcs) = &state.web_content_service
                && let Err(e) = wcs
                    .render_owed(&actor_id, "a folder serving transition")
                    .await
            {
                tracing::warn!("web re-render after folder serving transition: {e:#}");
            }
            encode_reply(&FolderUpdateReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.served_rows.adopt ─────────────────────────────────────────

/// The set's WebDAV pseudo-device rows still owed an adoption — unsigned and
/// adoptable by the one shared predicate
/// (`fauna_protocol::sync_writer_sig::served_row_adoptable`, applied in Rust
/// over the row as `changes.list` serves it, never mirrored in SQL —
/// `writer-signed-change-records.md` ruling (7)(b)(i)(1)–(2)).
async fn unadopted_served_rows(
    state: &AppState,
    fs: &crate::db::FolderRow,
) -> Result<u64, RpcError> {
    let owner: [u8; 32] = fs
        .actor_id
        .as_slice()
        .try_into()
        .map_err(|_| internal(FS, "the set's owner id is not 32 bytes"))?;
    let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&owner);
    let rows = state
        .db
        .unsigned_sync_changes_for_device(fs.id, &pseudo)
        .await
        .map_err(|e| internal(FS, e))?;
    Ok(rows
        .iter()
        .map(crate::sync_handlers::change_to_wire)
        .filter(fauna_protocol::sync_writer_sig::served_row_adoptable)
        .count() as u64)
}

/// `fauna.folders.served_rows_unadopted`, carrying `{ "unadopted": n }`.
fn served_rows_unadopted(count: u64) -> RpcError {
    let mut e = RpcError::new(
        RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTED,
        "error.folders.served_rows_unadopted",
    );
    e.details = Some(Box::new(fauna_protocol::Value::Map(
        [(
            "unadopted".to_string(),
            fauna_protocol::Value::Integer(i128::from(count)),
        )]
        .into_iter()
        .collect(),
    )));
    e
}

/// `fauna.folders.served_rows.adopt` — the owner signs a page of the set's
/// WebDAV pseudo-device rows IN PLACE before the flip OFF
/// (`writer-signed-change-records.md` ruling (7)(b)). Every named row must be
/// this set's, carry the owner's pseudo `device_id` and pass
/// `served_row_adoptable`, and every signature must verify over the stored
/// row's statement (`SignedChange::for_row_as`, the owner as actor) under the
/// set's STORED nonce — all before any write, the page refused whole on the
/// first that fails. Then the pairs are filled in one transaction; a row
/// already signed is left as it is.
fn served_rows_adopt_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_SERVED_ROWS_ADOPT).await?;
            let req: ServedRowsAdoptRequest = decode(&payload).map_err(malformed)?;
            if req.signatures.len() > fauna_protocol::folders::SERVED_ROWS_ADOPT_PAGE {
                return Err(coded(
                    FS,
                    "invalid_request",
                    format!(
                        "{} signatures exceed the page of {}",
                        req.signatures.len(),
                        fauna_protocol::folders::SERVED_ROWS_ADOPT_PAGE
                    ),
                ));
            }
            // Owner-only: the set resolves from the connection actor's own sets.
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let fs = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
            let set_nonce = crate::change_signature::stored_set_nonce(&fs)
                .map_err(|refusal| refusal.into_rpc(FS))?;
            let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&actor_id);

            let seqs: Vec<i64> = req.signatures.iter().map(|s| s.seq).collect();
            let mut seen = std::collections::HashSet::new();
            if let Some(dup) = seqs.iter().find(|s| !seen.insert(**s)) {
                return Err(coded(
                    FS,
                    "invalid_request",
                    format!("seq {dup} is signed twice in one page"),
                ));
            }
            let rows = state
                .db
                .sync_changes_by_seq(fs.id, &seqs)
                .await
                .map_err(|e| internal(FS, e))?;
            let by_seq: std::collections::HashMap<i64, &crate::db::SyncChangeRow> =
                rows.iter().map(|r| (r.seq, r)).collect();

            let mut verified = Vec::with_capacity(req.signatures.len());
            for carried in &req.signatures {
                let row = by_seq.get(&carried.seq).ok_or_else(|| {
                    coded(
                        FS,
                        "invalid_request",
                        format!("seq {} names no row of this set", carried.seq),
                    )
                })?;
                if row.device_id.as_deref() != Some(&pseudo[..]) {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        format!(
                            "seq {} is not a WebDAV pseudo-device row of this set",
                            carried.seq
                        ),
                    ));
                }
                let wire = crate::sync_handlers::change_to_wire(row);
                if !fauna_protocol::sync_writer_sig::served_row_adoptable(&wire) {
                    let mut e = RpcError::new(
                        RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTABLE,
                        "error.folders.served_rows_unadoptable",
                    );
                    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
                        "seq {} is not the honest recorder's row shape (ruling (7)(b)(i))",
                        carried.seq
                    ))));
                    return Err(e);
                }
                let statement = fauna_protocol::sync_writer_sig::SignedChange::for_row_as(
                    &wire, set_nonce, actor_id,
                )
                .map_err(|e| coded(FS, "invalid_request", e))?;
                let signature = crate::change_signature::verify_carried(
                    &state.db,
                    &statement,
                    crate::change_signature::CarriedSignature {
                        signature: Some(&carried.signature[..]),
                        signer_key: Some(&req.signer_key[..]),
                    },
                    // The owner's own connection to the nest holding their
                    // grants: a delegated signer resolves by reference, as at
                    // the record door.
                    crate::change_signature::CertCarriage::ByReference,
                )
                .await
                .map_err(|refusal| refusal.into_rpc(FS))?;
                verified.push((carried.seq, signature));
            }

            let page: Vec<(i64, crate::db::RowSignature<'_>)> = verified
                .iter()
                .map(|(seq, sig)| (*seq, sig.as_row()))
                .collect();
            let adopted = state
                .db
                .fill_sync_change_signatures(fs.id, &page)
                .await
                .map_err(|e| internal(FS, e))?;
            // Remember each delegated signer's cert for the list replies' side
            // table, once its rows are accepted — as the record door does.
            for (_, sig) in &verified {
                sig.remember_cert(&state.db)
                    .await
                    .map_err(|e| internal(FS, e))?;
            }
            let remaining = unadopted_served_rows(&state, &fs).await?;
            encode_reply(&ServedRowsAdoptReply {
                adopted,
                remaining,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.set_web_paywall ──────────────────────────────────────────

/// Set or clear a website-enabled folder's paywall tier (`monetization.md` § Pillar 2,
/// the folder half). Caller-scoped: the set and the tier are both the
/// caller's. `tier: Some(name)` validates the set is web-type + not a
/// reserved rail + the tier exists, then stamps `folders.web_paywall_tier`;
/// `None` clears unconditionally (un-paywalling is always allowed, the
/// webdav serve-OFF twin). The column is the entitlement seam the visitor
/// token mint consults; the at-rest sealing itself is driven client-side
/// (content-key genesis/re-seal — the `FoldersAuthor` orchestration).
fn set_web_paywall_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_SET_WEB_PAYWALL).await?;
            let req: FolderSetWebPaywallRequest = decode(&payload).map_err(malformed)?;

            // S5b: resolve hash-first, once, unconditionally — the mutation
            // below keys off `fs.name`, never `req.name` (empty on a
            // hash-addressed request).
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let fs = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            if let Some(ref tier) = req.tier {
                // Phase 5: a paywalled site serves bytes off the nest store,
                // which a metadata-only folder deliberately keeps empty — the
                // third serving surface of the pairwise refusal (`file-sync.md`
                // § Content residency). Checked FIRST: it names the deepest
                // structural problem.
                if fs.nest_content_residency.as_deref() == Some("metadata_only") {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a metadata-only folder cannot be paywalled; its content does not \
                         rest on the nest — set residency back to full first",
                    ));
                }
                // Website folders only.
                // Phase 4 re-keyed this from `mode == "web"` to the website
                // toggle (only a website-enabled folder feeds `web_files`);
                // the legacy `mode == "web"` arm retired with the spelling
                // itself (folders.md § Implementation status today, the mode
                // contraction — no production path ever wrote it).
                if !fs.website_enabled {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "web paywalling is only available for website-enabled folders",
                    ));
                }
                // A public folder rests plaintext, world-readable by ratified
                // design — a paywall over it would be theatre. The owner flips
                // the audience back first (which is also what re-seals future
                // content so the paywall has something to protect).
                if fs.is_public_audience() {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "a public folder cannot be paywalled; make it private or shared first",
                    ));
                }
                // A reserved rail is internal and never web-served.
                if crate::db::snapshots::is_reserved_folder_name(&fs.name) {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "reserved (\"__\") folders are internal rails and are never served",
                    ));
                }
                // The tier must be the caller's own existing subscription tier —
                // the entitlement seam Pillar 3's engine consults must resolve.
                if state
                    .db
                    .get_subscription_tier(&actor_id, tier)
                    .await
                    .map_err(|e| internal(FS, e))?
                    .is_none()
                {
                    return Err(coded(
                        FS,
                        "invalid_request",
                        "no such subscription tier for this account",
                    ));
                }
            }

            // **Gate surface `payments.paywall.designate`**, the folder half
            // (`dynamic-features.md` § Charter members; the tier half lives on
            // `fauna.subscriptions.tiers.create`). Only the `Some(tier)` arm is
            // an operation to bind: clearing a paywall is de-escalation, and a
            // tier that can only tighten must never be able to trap content
            // behind a paywall the owner can no longer remove.
            //
            // Compiled out of a store-safe nest for the same reason the tier
            // half is: an excised build keeps storing and re-serving the
            // designation a full client authored, and gates no comparison
            // because it has none.
            #[cfg(feature = "payments")]
            if req.tier.is_some() {
                crate::feature_gate::gate(
                    &state,
                    &actor_id,
                    &fauna_core::feature_gate::GateOp {
                        feature: fauna_core::feature_gate::GatedFeature::Payments,
                        surface: fauna_core::feature_gate::SURFACE_PAYMENTS_PAYWALL_DESIGNATE,
                        new_counterparties: 0,
                        magnitude: 0,
                    },
                )
                .await?;
            }

            let updated = state
                .db
                .set_folder_web_paywall_tier_by_id(fs.id, req.tier.as_deref())
                .await
                .map_err(|e| internal(FS, e))?;
            if !updated {
                return Err(coded(FS, "not_found", "folder not found"));
            }
            encode_reply(&FolderSetWebPaywallReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.delete (≡ DELETE /api/v1/file-sets/{name}) ────────────────

fn delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_DELETE).await?;
            let req: FolderDeleteRequest = decode(&payload).map_err(malformed)?;

            // Resolve + ownership check (the op lock needs the id). S5b:
            // hash-first, then `folder.name` (never `req.name`) addresses the
            // delete below.
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            // The reserved (`__`) namespace is the nest's own rails (`__mls`,
            // `__drafts`, `__mail`, …): deleting one clears the
            // `sync_changes` rows its sealed irrecoverable material is reachable
            // through, an unrecoverable state no client
            // call may reach (`nest/common.md` § Client-state recoverability).
            // The ONE deletable reserved shape is a custody copy
            // (`folders.custody_copy`) provisioned on a DESTINATION nest —
            // deleting it as the owner IS the owner-authenticated
            // destination-removal teardown (`behavior/backup-destinations.md`
            // § Destination-removal / supersede handshake). Keys on the RESOLVED
            // row, never `req.name` (empty on a hash-addressed request).
            if crate::db::snapshots::is_reserved_folder_name(&folder.name)
                && !crate::db::snapshots::is_reserved_custody_copy(
                    folder.custody_copy,
                    &folder.name,
                )
            {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "reserved (\"__\") folders are the nest's internal rails and cannot \
                     be deleted; only a backup custody copy may be",
                ));
            }

            // Op lock — concurrent destructive ops are rejected (twin: 409).
            let locked = state
                .db
                .try_acquire_op_lock("delete", folder.id, "user-delete")
                .await
                .map_err(|e| internal(FS, e))?;
            if !locked {
                return Err(coded(
                    FS,
                    "conflict",
                    "folder is locked by another operation",
                ));
            }

            let result = state.db.delete_folder_by_id(folder.id).await;
            // Always release the lock, even on failure.
            if let Err(e) = state.db.release_op_lock("delete", folder.id).await {
                tracing::error!("release op lock error: {e}");
            }
            match result.map_err(|e| internal(FS, e))? {
                true => {
                    // Deleting a website-capable folder is a serving
                    // transition like the toggle — its `web_files` projection
                    // rows died in the delete tx, and this re-render drops the
                    // dead folder's pages from the rendered half. That tx
                    // recorded the owed render for a website-capable folder,
                    // which is what this keys on; it fails closed like every
                    // revoking door.
                    if let Some(wcs) = &state.web_content_service
                        && let Err(e) = wcs
                            .render_owed(&actor_id, "a website folder's deletion")
                            .await
                    {
                        tracing::warn!("web re-render after website-folder delete: {e:#}");
                    }
                    encode_reply(&FolderDeleteReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                false => Err(coded(FS, "not_found", "folder not found")),
            }
        })
    })
}

// ── fauna.folders.devices (≡ GET /api/v1/file-sets/{name}/devices) ──────────

fn devices_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_DEVICES).await?;
            let req: FolderDevicesRequest = decode(&payload).map_err(malformed)?;

            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            let devices = state
                .db
                .get_folder_devices(folder.id, &actor_id)
                .await
                .map_err(|e| internal(FS, e))?
                .into_iter()
                .map(|d| FolderDevice {
                    device_id: hex::encode(&d.device_id),
                    label: d.label,
                    last_change_at: d.last_change_at,
                    change_count: d.change_count,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&FolderDevicesReply {
                devices,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.members.list (≡ GET /api/v1/file-sets/{name}/members) ─────

fn members_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_MEMBERS_LIST).await?;
            let req: MembersListRequest = decode(&payload).map_err(malformed)?;

            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            let members = state
                .db
                .list_folder_members_with_labels(folder.id, &actor_id)
                .await
                .map_err(|e| internal(FS, e))?
                .into_iter()
                .map(|p| FolderMember {
                    device_id: hex::encode(&p.device_id),
                    label: p.label,
                    flags: p.flags,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&MembersListReply {
                members,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.members.list_actors ───────────────────────────────────────
// The *actor* (user) roster of a shared folder — the owner-side "Shared with"
// list, distinct from `members.list` (the *device* roster). Projected over the
// set's derived-`ChannelId` `actor_channels` roster, gated by `folder_authz`
// exactly as `content_key.get` is. `docs/goal/ui/folders.md` § Sharing.

/// Core of `fauna.folders.members.list_actors` (testable without an `AppState`):
/// project the shared set's derived-`ChannelId` `actor_channels` roster into the
/// owner-side "Shared with" list. Owner/member-gated via `folder_authz` (same
/// read gate as `content_key.get`); an owner-only (unshared) set has no actor
/// roster → `not_shared`. Each roster actor carries its nest-resolved handle
/// (empty when unknown) and a `role` of `"owner"` (the set's `actor_id`) or
/// `"member"`.
async fn actor_members_list_core(
    db: &crate::db::CacheDb,
    caller: &[u8; 32],
    req: &fauna_protocol::folders::ActorMembersListRequest,
) -> Result<fauna_protocol::folders::ActorMembersListReply, RpcError> {
    // Member-aware resolve: owner OR roster member; non-member/absent → None,
    // folding to `not_found` (the ST-RES-1 name-existence oracle stays closed).
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let fs =
        crate::folder_authz::resolve_readable_folder(db, &req.name, name_hash.as_ref(), caller)
            .await
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
    let channel_id = derive_channel_id(&fs)?; // `not_shared` for an owner-only set
    let owner = <[u8; 32]>::try_from(fs.actor_id.as_slice())
        .map_err(|_| coded(FS, "internal", "folder owner id is not 32 bytes"))?;

    let members = actor_roster_for_channel(db, &channel_id, &owner)
        .await
        .map_err(|e| internal(FS, e))?;

    Ok(fauna_protocol::folders::ActorMembersListReply {
        members,
        // Same-nest read: the caller's own nest already projects `access` onto
        // the member `FolderSummary`. The stamp exists for the relay arm
        // (`members.list_actors_remote`).
        caller_access: None,
        residency: None,
        extra: Default::default(),
    })
}

/// **The one actor-roster projection behind both doors** — the same-nest
/// `fauna.folders.members.list_actors` (which resolves the readable set and
/// derives its channel first) and the federated
/// `fauna.federation.folder.actors.fetch` (which gates the foreign member and
/// resolves the claimed set first, then blanks every `handle` — ids-only across
/// nests; `federation.md` § Cross-nest…, *The cross-nest writer roster read*).
/// The `actor_channels` roster (the owner as the `role == "owner"` row) unioned
/// with the cross-nest `channel_foreign_members`, each member carrying its
/// `folder_member_access` grant — and, on the owner row and each `writer` row,
/// the succession statements that end at it
/// ([`carried_succession_statements`]).
pub(crate) async fn actor_roster_for_channel(
    db: &crate::db::CacheDb,
    channel_id: &[u8; 32],
    owner: &[u8; 32],
) -> anyhow::Result<Vec<fauna_protocol::folders::FolderActorMember>> {
    let owner = *owner;
    let actors = db.list_channel_actors(channel_id).await?;

    // Access grants (multi-writer Phase 1) — one batch read, keyed by actor.
    // A member with no row is a `reader` (the fail-safe default); the owner row
    // carries no grant fields at all (owners aren't granted — they own).
    let roles: std::collections::HashMap<[u8; 32], crate::db::channels::FolderMemberRoleRow> = db
        .list_folder_member_access(channel_id)
        .await?
        .into_iter()
        .collect();

    let mut members = Vec::with_capacity(actors.len());
    for actor in actors {
        let handle = db.get_handle(&actor).await?.unwrap_or_default();
        let role = if actor == owner { "owner" } else { "member" };
        let (access, byte_cap, bytes_used) = if actor == owner {
            (None, None, None)
        } else {
            match roles.get(&actor) {
                Some(r) => (Some(r.access.clone()), r.byte_cap, Some(r.bytes_used)),
                None => (Some("reader".to_string()), None, Some(0)),
            }
        };
        let succession_statements =
            carried_succession_statements(db, &actor, role, access.as_deref()).await?;
        members.push(fauna_protocol::folders::FolderActorMember {
            actor_id: hex::encode(actor),
            handle,
            role: role.to_string(),
            access,
            byte_cap,
            bytes_used,
            succession_statements,
            ..Default::default()
        });
    }

    // Union the cross-nest members (Phase 2, `ui/folders.md` § Sharing →
    // Cross-nest members): a foreign member has no `actor_channels` row — their
    // membership IS the `channel_foreign_members` row this nest wrote at
    // Welcome-relay time — so without this union the owner's "Shared with"
    // roster silently omits them. `remote: true` is the additive marker; the
    // handle stays empty (a foreign member is not a local `users` row) and the
    // access projection is the same grant table (a Phase-3 cross-nest writer's
    // grant renders identically).
    for actor in db.list_foreign_channel_members(channel_id).await? {
        let (access, byte_cap, bytes_used) = match roles.get(&actor) {
            Some(r) => (Some(r.access.clone()), r.byte_cap, Some(r.bytes_used)),
            None => (Some("reader".to_string()), None, Some(0)),
        };
        let succession_statements =
            carried_succession_statements(db, &actor, "member", access.as_deref()).await?;
        members.push(fauna_protocol::folders::FolderActorMember {
            actor_id: hex::encode(actor),
            handle: String::new(),
            role: "member".to_string(),
            access,
            byte_cap,
            bytes_used,
            remote: Some(true),
            succession_statements,
            ..Default::default()
        });
    }

    Ok(members)
}

/// The succession statements one roster row carries
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(b) source (i)): for a **writer** — the owner row and each `writer`-access
/// row, the reader's own predicate — the landed chain that ends at that
/// member, verbatim and oldest first, from every succession this nest holds
/// (its own accounts' and the peer-delivered ones). A reader-access row
/// carries none: the read discloses a retired id for a writer only
/// (`federation.md` § Cross-nest…, *The cross-nest writer roster read*). The
/// nest vouches for nothing here — a reader verifies each link itself.
async fn carried_succession_statements(
    db: &crate::db::CacheDb,
    actor: &[u8; 32],
    role: &str,
    access: Option<&str>,
) -> anyhow::Result<Vec<fauna_protocol::ByteBuf>> {
    if !fauna_protocol::sync_row_verify::roster_member_is_writer(role, access) {
        return Ok(Vec::new());
    }
    Ok(db
        .succession_statements_into(actor)
        .await?
        .into_iter()
        .map(fauna_protocol::ByteBuf::from)
        .collect())
}

// ── fauna.folders.members.set_access (multi-writer Phase 1) ─────────────────
// The owner grants or edits a member's `reader`/`writer` access (+ byte cap) on
// their shared set — the write behind `folder-member-role-select` /
// `folder-member-cap-input` (`ui/folders.md` § Sharing owns the access
// model). Role transitions never rotate: a demoted writer already holds the
// keys (removal is `members.evict`, which rotates).

/// Core of `fauna.folders.members.set_access` (testable without an
/// `AppState`): owner-scoped lookup (ST-RES-1 `not_found` fold) + the
/// first-binder claimant gate (owning *some* set bound
/// to the group is not enough), exactly mirroring `content_key_put_core`.
/// Upserts the `(channel, actor)` role row, preserving `bytes_used`. The
/// target need not be rostered yet — a pre-join grant is harmless (the write
/// gate requires roster membership too) and is exactly what the share-time
/// grant path records.
async fn set_access_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    req: &fauna_protocol::folders::MemberSetAccessRequest,
) -> Result<fauna_protocol::folders::MemberSetAccessReply, RpcError> {
    let member = fauna_core::hex32::decode(req.actor_id.trim())
        .map_err(|_| coded(FS, "invalid_request", "actor_id must be 32-byte hex"))?;
    if req.access != "reader" && req.access != "writer" {
        return Err(coded(
            FS,
            "invalid_request",
            "access must be \"reader\" or \"writer\"",
        ));
    }
    if req.byte_cap.is_some_and(|c| c < 0) {
        return Err(coded(FS, "invalid_request", "byte_cap must be >= 0"));
    }

    // Owner-scoped lookup folds non-owner/absent to one `not_found` (ST-RES-1).
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let fs = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(&h, owner).await,
        None => db.get_folder_for_actor(&req.name, owner).await,
    }
    .map_err(|e| internal(FS, e))?
    .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
    let channel_id = derive_channel_id(&fs)?; // `not_shared` for an unbound set
    require_channel_claimant(db, &channel_id, owner).await?;

    db.set_folder_member_access(&channel_id, &member, &req.access, req.byte_cap)
        .await
        .map_err(|e| internal(FS, e))?;

    Ok(fauna_protocol::folders::MemberSetAccessReply {
        ok: true,
        extra: Default::default(),
    })
}

fn members_set_access_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_MEMBERS_SET_ACCESS).await?;
            let req = decode(&payload).map_err(malformed)?;
            let reply = set_access_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

fn actor_members_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_MEMBERS_LIST_ACTORS).await?;
            let req = decode(&payload).map_err(malformed)?;
            let reply = actor_members_list_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.members.list_actors_remote ────────────────────────────────

/// The cross-nest writer roster read's client leg (`federation.md` §
/// Cross-nest…, *The cross-nest writer roster read*): a foreign set's roster
/// lives on its home nest, so this nest originates
/// `fauna.federation.folder.actors.fetch` there and threads the home nest's
/// projection (ids-only) and `caller_access` stamp back. The home nest applies
/// the membership gate; this handler adds no policy beyond the relay and writes
/// nothing (a roster read that registered a row would vouch for a phantom).
///
/// A **distinct kind**, never an additive `nest_url` on `members.list_actors`
/// — the roster is an authorization input, and an old member-nest ignoring the
/// field would answer the caller's own same-named set as a clean success (the
/// `fauna.conversations.channel.actors_remote` reason; full statement on
/// [`fauna_protocol::folders::ActorMembersListRemoteRequest`]). A peer-side
/// `unauthenticated` (an old home nest predating the kind) maps to the typed
/// `peer_nest_outdated`; every error leaves the reader's roster unread — the
/// correct fail-closed degradation.
fn actor_members_list_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                FS,
                &actor_id,
                KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE,
            )
            .await?;
            let req: fauna_protocol::folders::ActorMembersListRemoteRequest =
                decode(&payload).map_err(malformed)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "nest_url must name the set's home nest (same-nest reads use members.list_actors)",
                ));
            }
            // Validate the id shape locally; the home nest is authoritative for
            // membership.
            if hex::decode(&req.channel_id).map(|b| b.len()) != Ok(32) {
                return Err(coded(
                    FS,
                    "invalid_request",
                    "channel_id must be 32 hex-encoded bytes",
                ));
            }
            match crate::federation_pool::originate_folder_actors_fetch(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.channel_id,
            )
            .await
            {
                Ok(Ok(fetched)) => encode_reply(&fauna_protocol::folders::ActorMembersListReply {
                    members: fetched.members,
                    caller_access: fetched.caller_access,
                    residency: fetched.residency,
                    extra: Default::default(),
                }),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest writer roster read",
                )),
                Err(pool_err) => {
                    tracing::error!("federation folder actors fetch relay: {pool_err}");
                    Err(internal(FS, "federation roster read failed"))
                }
            }
        })
    })
}

// ── fauna.folders.members.remove ────────────────────────────────────────────
// (≡ DELETE /api/v1/file-sets/{name}/members/{device_id})

fn members_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_MEMBERS_REMOVE).await?;
            let req: MemberRemoveRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded(FS, "invalid_request", "invalid device_id hex"))?;

            // Owner-scoped, hash-first (S5b): a set the caller does not own
            // folds into the same `not_found` as an absent one.
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            let removed = state
                .db
                .remove_folder_member(folder.id, &device_id)
                .await
                .map_err(|e| map_db_error(FS, e))?;
            if !removed {
                return Err(coded(FS, "not_found", "member not found"));
            }
            encode_reply(&MemberRemoveReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.places.set ────────────────────────────────────────────────
// The one add/edit door onto a device place (folders re-model § Places): it
// enrols a device that holds no place and rewrites the flags of one that does.
// The role-speaking `members.add` retired with the role contraction.

fn places_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_PLACES_SET).await?;
            let req: PlacesSetRequest = decode(&payload).map_err(malformed)?;

            // Every flag point is writable and rests exactly as sent — never
            // rounded, which would hand the seat behavior nobody asked for in
            // a space where one direction deletes the user's files
            // (`principles.md` § No user-data loss).
            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded(FS, "invalid_request", "invalid device_id hex"))?;

            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.name, &actor_id).await,
            }
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            state
                .db
                .set_folder_place_flags(folder.id, &actor_id, &device_id, &req.flags)
                .await
                .map_err(|e| map_db_error(FS, e))?;
            encode_reply(&PlacesSetReply {
                ok: true,
                // Echo what the caller addressed by — never the resting
                // `folders.name`, which the S9 path
                // flip leaves empty for a sealed name and which would make
                // this reply's shape depend on how the row happens to rest.
                folder: req.name,
                device_id: req.device_id,
                flags: req.flags,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.lease.acquire (≡ POST /api/v1/file-sets/{name}/lease) ─────

fn lease_acquire_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_LEASE_ACQUIRE).await?;
            let req: LeaseAcquireRequest = decode(&payload).map_err(malformed)?;

            // Upload lease = a write op → the owner OR a `writer`-granted member
            // (multi-writer Phase 1; one of the exactly-three widened kinds,
            // file-sync.md § Multi-writer shared sets). Absent/not-writable folds
            // to one `not_found`, closing the name-existence oracle (ST-RES-1).
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = crate::folder_authz::resolve_writable_folder(
                &state.db,
                &req.name,
                name_hash.as_ref(),
                &actor_id,
            )
            .await
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded(FS, "invalid_request", "invalid device_id"))?;

            let acquired = state
                .db
                .try_acquire_upload_lease(folder.id, &actor_id, &device_id, 300)
                .await
                .map_err(|e| internal(FS, e))?;
            if !acquired {
                return Err(coded(FS, "conflict", "lease held by another device"));
            }
            encode_reply(&LeaseAcquireReply {
                acquired: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.folders.lease.release (≡ DELETE /api/v1/file-sets/{name}/lease) ───

fn lease_release_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_LEASE_RELEASE).await?;
            let req: LeaseReleaseRequest = decode(&payload).map_err(malformed)?;

            // Lease release = a write op → owner OR `writer`-granted member
            // (multi-writer Phase 1); folds to `not_found` (ST-RES-1, as for
            // lease.acquire).
            let name_hash = parse_name_hash(FS, &req.name_hash)?;
            let folder = crate::folder_authz::resolve_writable_folder(
                &state.db,
                &req.name,
                name_hash.as_ref(),
                &actor_id,
            )
            .await
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

            // release is HOLDER-SCOPED, and the holder is the
            // authenticated ACTOR plus its device. `device_id` (required —
            // a request without it is malformed at decode) is client-asserted
            // and so scopes nothing alone; the delete is keyed
            // `(folder, actor, device_id)`, so a writer can free the lease its
            // own account's device holds but never another actor's active lease,
            // whatever device id it names (acquire is holder-checked the same
            // way; an unscoped release would be the asymmetry a writer could
            // exploit to grief-grab). The former absent-field arm
            // — an owner-only holder-blind clear for an old client or a
            // crash-stuck lease — was retired 2026-09-24 under the
            // compat-remnant sweep: no caller sent it, and a crash-stuck lease
            // lapses on the TTL takeover.
            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded(FS, "invalid_request", "invalid device_id"))?;
            state
                .db
                .release_upload_lease(folder.id, &actor_id, &device_id)
                .await
                .map_err(|e| internal(FS, e))?;
            encode_reply(&LeaseReleaseReply {
                released: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.conflicts.report (≡ POST /api/v1/sync/conflicts) ───────────────

fn conflict_report_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, SYNC, &actor_id, "fauna.sync.conflicts.report").await?;
            let req: ConflictReportRequest = decode(&payload).map_err(malformed)?;

            // Conflict report = a write op → owner OR `writer`-granted member
            // (multi-writer Phase 1; the third of the exactly-three widened
            // kinds, file-sync.md § Multi-writer shared sets). ST-RES-1 fold.
            let name_hash = parse_name_hash(SYNC, &req.name_hash)?;
            let folder = crate::folder_authz::resolve_writable_folder(
                &state.db,
                &req.folder,
                name_hash.as_ref(),
                &actor_id,
            )
            .await
            .map_err(|e| internal(SYNC, e))?
            .ok_or_else(|| coded(SYNC, "not_found", "folder not found"))?;
            let device_bytes = hex::decode(&req.device_id)
                .map_err(|_| coded(SYNC, "invalid_request", "invalid device_id hex"))?;

            // THE approved compat break (S9 flip — `encryption-at-rest.md`
            // § Carve-outs): a sealless conflict report is refused on a
            // sealed plane. Every production reporter seals (the S6-a
            // funnels — engine + daemon `ConflictLabels::seal`); accepting a
            // sealless one would rest an unrenderable conflict row and mint
            // label-less (un-appliable) propagation records at resolve time.
            // The ratified plaintext class is exempt: a `public`-audience
            // folder (phase 4) — `rests_plaintext_paths`, the one owner (the
            // legacy `web`-mode spelling of the same class is retired).
            if req.path_sealed.is_none() && !folder.rests_plaintext_paths() {
                return Err(coded(
                    SYNC,
                    "path_seal_required",
                    "this nest rests no plaintext paths (S9 flip): the conflict report \
                     must carry path_sealed — the app must send it",
                ));
            }

            // Decode the diverging candidate versions (hex on the wire, BLOB in
            // the store). Empty = candidate-free (mark-only) conflict.
            let candidates = req
                .candidates
                .iter()
                .map(|c| {
                    Ok(ConflictCandidateRow {
                        manifest_hash: hex::decode(&c.manifest_hash).map_err(|_| {
                            coded(
                                SYNC,
                                "invalid_request",
                                "invalid candidate manifest_hash hex",
                            )
                        })?,
                        device_id: hex::decode(&c.device_id).map_err(|_| {
                            coded(SYNC, "invalid_request", "invalid candidate device_id hex")
                        })?,
                        size_bytes: c.size_bytes,
                        created_at: c.created_at,
                        content_key_version: c.content_key_version,
                    })
                })
                .collect::<Result<Vec<_>, RpcError>>()?;

            // Auto-resolve (ratified 2026-07-10): a `resolution` marks the
            // conflict pre-resolved by the detecting device — the row lands
            // resolved (the review-list surface). It must name how it resolved
            // and carry the winner it recorded.
            let winner_bytes = match (&req.resolution, &req.winning_manifest_hash) {
                (None, _) => None,
                (Some(kind), Some(winner_hex)) => {
                    if !matches!(kind.as_str(), "merged" | "latest_wins") {
                        return Err(coded(
                            SYNC,
                            "invalid_request",
                            "resolution must be \"merged\" or \"latest_wins\"",
                        ));
                    }
                    Some(hex::decode(winner_hex).map_err(|_| {
                        coded(SYNC, "invalid_request", "invalid winning_manifest_hash hex")
                    })?)
                }
                (Some(_), None) => {
                    return Err(coded(
                        SYNC,
                        "invalid_request",
                        "a resolved report requires winning_manifest_hash",
                    ));
                }
            };
            // Assemble the propagation half: size/generation come from the
            // winning candidate when the winner IS a candidate; a non-candidate
            // winner (a merged result) must carry them on the wire. Attribution
            // follows the choose-winner precedent — the winning candidate's
            // device self-echo-skips its own head row; a merged winner is
            // attributed to the reporter (who wrote it locally).
            let resolution = match (req.resolution.as_deref(), winner_bytes) {
                (Some(kind), Some(winner)) => {
                    let winning_candidate = candidates.iter().find(|c| c.manifest_hash == winner);
                    let (winning_size_bytes, winning_content_key_version, winner_device_id) =
                        match winning_candidate {
                            Some(c) => (c.size_bytes, c.content_key_version, c.device_id.clone()),
                            None => (
                                req.winning_size_bytes.ok_or_else(|| {
                                    coded(
                                        SYNC,
                                        "invalid_request",
                                        "a non-candidate winner requires winning_size_bytes",
                                    )
                                })?,
                                req.winning_content_key_version,
                                device_bytes.clone(),
                            ),
                        };
                    Some(crate::db::ResolvedReport {
                        resolution: kind.to_string(),
                        winning_manifest_hash: winner,
                        winning_size_bytes,
                        winning_content_key_version,
                        winner_device_id,
                        winning_derived_through: req.winning_derived_through,
                        losing_derived_through: req.losing_derived_through,
                        winning_carries_novelty: req.winning_carries_novelty,
                    })
                }
                _ => None,
            };
            let resolved = resolution.is_some();
            // The client's routing hash when well-formed, else the nest's
            // derivation — the one value the conflict row and both minted
            // change rows file under (`report_conflict_signed`).
            let path_hash = req
                .path_hash
                .as_ref()
                .and_then(|b| <[u8; 32]>::try_from(&b[..]).ok());
            let (verified, verified_loser) = match &resolution {
                Some(r) => {
                    let routing =
                        path_hash.unwrap_or_else(|| fauna_core::sync::path_hash(&req.path));
                    let sealed = req.path_sealed.as_ref().map(|b| b[..].to_vec());
                    let winner = verify_report_winner_signature(
                        &state,
                        &actor_id,
                        &folder,
                        r,
                        routing,
                        sealed.clone(),
                        crate::change_signature::CarriedSignature::new(
                            req.winner_signature.as_ref(),
                            req.winner_signer_key.as_ref(),
                        ),
                    )
                    .await?;
                    // Ruling (10)(d): the retained loser is signed by the
                    // same signer, under the same key.
                    let loser = match r.retained_loser(&candidates, &device_bytes) {
                        Some(loser) => Some(
                            verify_report_loser_signature(
                                &state,
                                &actor_id,
                                &folder,
                                r,
                                loser,
                                routing,
                                sealed,
                                req.loser_signature.as_ref(),
                                req.winner_signer_key.as_ref(),
                            )
                            .await?,
                        ),
                        None => None,
                    };
                    (winner, loser)
                }
                None => (None, None),
            };

            let id = state
                .db
                .report_conflict_signed(
                    &actor_id,
                    folder.id,
                    &device_bytes,
                    &req.path,
                    &req.conflict_type,
                    req.details.as_deref(),
                    &candidates,
                    resolution,
                    // Client-keyed companions (path-sealing S6-a). The nest
                    // stores them opaquely — it holds no key to open or verify
                    // them with, exactly as for every other sealed sibling. A
                    // malformed `path_hash` (not 32 bytes) is dropped rather
                    // than refused: the row still lands and stays addressable
                    // by the nest's own derivation, which is the same
                    // fail-soft `path_label_salt` applies on the read side.
                    crate::db::SealedConflictLabels {
                        path_hash,
                        path_sealed: req.path_sealed.as_ref().map(|b| b[..].to_vec()),
                        details_sealed: req.details_sealed.as_ref().map(|b| b[..].to_vec()),
                    },
                    // S9 flip: only the ratified plaintext classes — a
                    // `public`-audience folder (phase 4) — keeps resting
                    // plaintext conflict rows; everything else rests the scrub sentinel + the
                    // sealed pair.
                    folder.rests_plaintext_paths(),
                    crate::db::ReportRowSignatures {
                        winner: verified.as_ref().map(|v| v.as_row()),
                        loser: verified_loser.as_ref().map(|v| v.as_row()),
                    },
                )
                .await
                // The rows a resolved report mints are metered like any record
                // (2026-09-28): the same typed refusals the record door answers —
                // `storage_quota_exceeded`, `member_cap_exceeded`,
                // `invalid_size` — and nothing landed.
                .map_err(crate::sync_handlers::quota_err)?;
            if let Some(v) = &verified {
                v.remember_cert(&state.db)
                    .await
                    .map_err(|e| internal(SYNC, e))?;
            }
            // The resolved report is a THIRD change-recording rail, and it owes
            // the same nudge as the other two. `report_conflict` above inserts
            // the loser's retention row AND the winner head row that "every
            // device converges on via normal catch-up" — but nothing told the
            // other devices to catch up, so they waited out the rescan interval
            // (300 s by default) while the nest already held the merged result.
            // That is exactly the gap `notify_sync_changed` was added to close
            // for `record_change_core`, and then again for the data plane's
            // `handle_file_changed` — the rails must agree. Measured 2026-08-01
            // by the two-seat twin's leg 4: the seat that auto-merged pushed the
            // merged head, its peer never heard, and the run failed on the peer
            // with the merge sitting on the nest the whole time. Fires only when
            // rows were actually recorded (`resolved`); an unresolved chooser
            // report records no change row and has nothing to pull.
            if resolved {
                crate::sync_handlers::notify_sync_changed(&state, &folder).await;
            }
            encode_reply(&ConflictReportReply {
                id,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.conflicts.list (≡ GET /api/v1/sync/conflicts) ──────────────────

fn conflicts_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, SYNC, &actor_id, "fauna.sync.conflicts.list").await?;
            let req: ConflictsListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_conflicts_for_actor(&actor_id, req.include_resolved == Some(true))
                .await
                .map_err(|e| internal(SYNC, e))?;
            // folder_id → row (the twin builds the same map for the response).
            // Kept as the full row, not just `.name`, so the set-name seal +
            // salt pair (path-sealing S5c-2) rides the conflict list the same
            // way it already rides `fauna.folders.list` / `fauna.media.list`.
            // Owner/participant-audience surface — no per-reader projection.
            let folders = state
                .db
                .get_folders_for_actor_full(&actor_id)
                .await
                .map_err(|e| internal(SYNC, e))?;
            let fs_map: std::collections::HashMap<i64, crate::db::FolderRow> =
                folders.into_iter().map(|fs| (fs.id, fs)).collect();

            // Candidate versions, grouped per conflict (hex on the wire).
            let mut cand_map: std::collections::HashMap<i64, Vec<ConflictCandidate>> =
                std::collections::HashMap::new();
            for (conflict_id, c) in state
                .db
                .list_conflict_candidates_for_actor(&actor_id)
                .await
                .map_err(|e| internal(SYNC, e))?
            {
                cand_map
                    .entry(conflict_id)
                    .or_default()
                    .push(ConflictCandidate {
                        manifest_hash: hex::encode(&c.manifest_hash),
                        device_id: hex::encode(&c.device_id),
                        size_bytes: c.size_bytes,
                        created_at: c.created_at,
                        content_key_version: c.content_key_version,
                        extra: Default::default(),
                    });
            }

            let conflicts = rows
                .into_iter()
                .map(|c| {
                    let fs = fs_map.get(&c.folder_id);
                    // Strict pair: `zip` so a set whose seal was never stamped
                    // (or one row's name and hash disagree, which cannot
                    // happen but costs nothing to guard) ships neither half —
                    // a seal without its salt is unrenderable once the
                    // plaintext scrubs.
                    let (folder_sealed, folder_hash) = fs
                        .and_then(|fs| fs.name_sealed.clone().zip(fs.name_hash.clone()))
                        .map(|(sealed, hash)| {
                            (
                                Some(fauna_protocol::ByteBuf::from(sealed)),
                                Some(fauna_protocol::ByteBuf::from(hash)),
                            )
                        })
                        .unwrap_or((None, None));
                    SyncConflict {
                        id: c.id,
                        folder: fs
                            .map(|fs| fs.name.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        device_id: hex::encode(&c.device_id),
                        path: c.path,
                        conflict_type: c.conflict_type,
                        details: c.details,
                        created_at: c.created_at,
                        candidates: cand_map.remove(&c.id).unwrap_or_default(),
                        resolved_at: c.resolved_at,
                        resolution: c.resolution,
                        winning_manifest_hash: c.winning_manifest_hash.as_deref().map(hex::encode),
                        path_hash: fauna_protocol::ByteBuf::from(c.path_hash),
                        path_sealed: c.path_sealed.map(fauna_protocol::ByteBuf::from),
                        details_sealed: c.details_sealed.map(fauna_protocol::ByteBuf::from),
                        folder_sealed,
                        folder_hash,
                        extra: Default::default(),
                    }
                })
                .collect();
            encode_reply(&ConflictsListReply {
                conflicts,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.conflicts.resolve ──────────────────────────────────────────────
// (≡ POST /api/v1/sync/conflicts/{id}/resolve)

/// Verify the chooser's writer signature over the winner head row the
/// choose-winner resolve will mint (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*, ruling (1)(ii)): the statement is the row
/// exactly as the nest mints it — the winning candidate's `device_id`, size and
/// generation, the conflict's `path_hash` and `path_sealed`, `change_type =
/// "modify"`, no causal stamp, not a resolution — signed by the chooser (the
/// set owner; the resolve is owner-scoped) under the set's stored nonce, the
/// signer resolved by reference like any same-nest record. An unsigned choose
/// is refused `signature_required`;
/// `Ok(None)` for a conflict/candidate the resolve itself will refuse with its
/// own typed outcome.
async fn verify_choose_winner_signature(
    state: &AppState,
    actor_id: &[u8; 32],
    conflict_id: i64,
    winning_manifest_hash: &[u8],
    carried: crate::change_signature::CarriedSignature<'_>,
) -> Result<Option<crate::change_signature::VerifiedSignature>, RpcError> {
    let refuse = |r: crate::change_signature::IngestRefusal| r.into_rpc(SYNC);
    if carried.is_unsigned() {
        return Err(refuse(crate::change_signature::IngestRefusal::Required));
    }
    let Some(facts) = state
        .db
        .conflict_winner_facts(actor_id, conflict_id, winning_manifest_hash)
        .await
        .map_err(|e| internal(SYNC, e))?
    else {
        return Ok(None);
    };
    let fs = state
        .db
        .get_folder_by_id(facts.folder_id)
        .await
        .map_err(|e| internal(SYNC, e))?
        .ok_or_else(|| internal(SYNC, "conflict's folder vanished"))?;
    let set_nonce = crate::change_signature::stored_set_nonce(&fs).map_err(refuse)?;
    let bytes32 = |b: &[u8], what: &str| -> Result<[u8; 32], RpcError> {
        b.try_into()
            .map_err(|_| internal(SYNC, format!("stored {what} is not 32 bytes")))
    };
    let statement = fauna_protocol::sync_writer_sig::SignedChange {
        set_nonce,
        actor_id: *actor_id,
        device_id: bytes32(&facts.device_id, "candidate device_id")?,
        path_hash: bytes32(&facts.path_hash, "path_hash")?,
        manifest_hash: Some(
            bytes32(winning_manifest_hash, "winning manifest hash").map_err(|_| {
                coded(
                    SYNC,
                    "invalid_request",
                    "winning_manifest_hash is not 32 bytes",
                )
            })?,
        ),
        change_type: "modify".into(),
        size_bytes: facts.size_bytes,
        content_key_version: facts.content_key_version.map(|v| v as u64),
        path_sealed: facts.path_sealed,
        thumbnail_hash: None,
        derived_through: None,
        is_resolution: false,
        is_retention: false,
    };
    crate::change_signature::verify_carried(
        &state.db,
        &statement,
        carried,
        crate::change_signature::CertCarriage::ByReference,
    )
    .await
    .map(Some)
    .map_err(refuse)
}

/// Verify the reporter's writer signature over the winner head row a resolved
/// report will mint (`mls-group-key-material.md` § M2 → *Writer-signed change
/// records*, ruling (1)(ii), the resolved-report clause): the statement is the
/// row exactly as `report_conflict_signed` mints it — the winner's
/// `device_id` (the winning candidate's, else the reporter's), its size and
/// generation, the request's `path_hash` and `path_sealed`, `change_type =
/// "modify"`, no thumbnail, `derived_through` = `winning_derived_through` AS
/// SENT (the nest's winner-stamp upgrade is retired, so the reporter can know
/// every signed field), and `is_resolution = !winning_carries_novelty` —
/// signed by the reporting actor under the set's stored nonce, the signer
/// resolved by reference like any same-nest record. An unsigned report is
/// refused `signature_required`.
async fn verify_report_winner_signature(
    state: &AppState,
    actor_id: &[u8; 32],
    folder: &crate::db::FolderRow,
    report: &crate::db::ResolvedReport,
    path_hash: [u8; 32],
    path_sealed: Option<Vec<u8>>,
    carried: crate::change_signature::CarriedSignature<'_>,
) -> Result<Option<crate::change_signature::VerifiedSignature>, RpcError> {
    let refuse = |r: crate::change_signature::IngestRefusal| r.into_rpc(SYNC);
    if carried.is_unsigned() {
        return Err(refuse(crate::change_signature::IngestRefusal::Required));
    }
    let set_nonce = crate::change_signature::stored_set_nonce(folder).map_err(refuse)?;
    let bytes32 = |b: &[u8], what: &str| -> Result<[u8; 32], RpcError> {
        b.try_into()
            .map_err(|_| coded(SYNC, "invalid_request", format!("{what} is not 32 bytes")))
    };
    let statement = fauna_protocol::sync_writer_sig::SignedChange {
        set_nonce,
        actor_id: *actor_id,
        device_id: bytes32(&report.winner_device_id, "the winner's device_id")?,
        path_hash,
        manifest_hash: Some(bytes32(
            &report.winning_manifest_hash,
            "winning_manifest_hash",
        )?),
        change_type: "modify".into(),
        size_bytes: report.winning_size_bytes,
        content_key_version: report.winning_content_key_version,
        path_sealed,
        thumbnail_hash: None,
        derived_through: report.winning_derived_through,
        is_resolution: !report.winning_carries_novelty.unwrap_or(false),
        is_retention: false,
    };
    crate::change_signature::verify_carried(
        &state.db,
        &statement,
        carried,
        crate::change_signature::CertCarriage::ByReference,
    )
    .await
    .map(Some)
    .map_err(refuse)
}

/// Verify a resolved report's signature over the **retention row** of the
/// reporter's losing candidate (ruling (10)(d)): the statement rebuilt from
/// the row this report is about to mint — `loser`, the candidate
/// [`crate::db::ResolvedReport::retained_loser`] picks, the same pick the
/// insert makes — under the STORED nonce, the reporter as its actor, the
/// signer by reference under the winner's key (one signer per report). A
/// report that retains a loser and signs none is refused `signature_required`,
/// a mismatch `signature_invalid`. Birth-shape: the condition of ruling (4),
/// re-read for this build, held — no public release had shipped (ruling
/// (10)(i)).
#[allow(clippy::too_many_arguments)]
async fn verify_report_loser_signature(
    state: &AppState,
    actor_id: &[u8; 32],
    folder: &crate::db::FolderRow,
    report: &crate::db::ResolvedReport,
    loser: &ConflictCandidateRow,
    path_hash: [u8; 32],
    path_sealed: Option<Vec<u8>>,
    loser_signature: Option<&serde_bytes::ByteBuf>,
    signer_key: Option<&serde_bytes::ByteBuf>,
) -> Result<crate::change_signature::VerifiedSignature, RpcError> {
    let refuse = |r: crate::change_signature::IngestRefusal| r.into_rpc(SYNC);
    if loser_signature.is_none() {
        return Err(refuse(crate::change_signature::IngestRefusal::Required));
    }
    let set_nonce = crate::change_signature::stored_set_nonce(folder).map_err(refuse)?;
    let bytes32 = |b: &[u8], what: &str| -> Result<[u8; 32], RpcError> {
        b.try_into()
            .map_err(|_| coded(SYNC, "invalid_request", format!("{what} is not 32 bytes")))
    };
    let statement = fauna_protocol::sync_writer_sig::SignedChange {
        set_nonce,
        actor_id: *actor_id,
        device_id: bytes32(&loser.device_id, "the retained loser's device_id")?,
        path_hash,
        manifest_hash: Some(bytes32(
            &loser.manifest_hash,
            "the retained loser's manifest_hash",
        )?),
        change_type: "modify".into(),
        size_bytes: loser.size_bytes,
        content_key_version: loser.content_key_version,
        path_sealed,
        thumbnail_hash: None,
        derived_through: report.losing_derived_through,
        is_resolution: false,
        is_retention: true,
    };
    crate::change_signature::verify_carried(
        &state.db,
        &statement,
        crate::change_signature::CarriedSignature::new(loser_signature, signer_key),
        crate::change_signature::CertCarriage::ByReference,
    )
    .await
    .map_err(refuse)
}

fn conflict_resolve_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, SYNC, &actor_id, "fauna.sync.conflicts.resolve").await?;
            let req: ConflictResolveRequest = decode(&payload).map_err(malformed)?;

            // Resolution is owner-scoped: both DB paths gate on the conflict's
            // folder belonging to `actor_id` (F8). A caller who guesses another
            // user's enumerable conflict id gets NotFound / a no-op.
            match req.winning_manifest_hash {
                // Choose-winner: validate the hash against the conflict's
                // recorded candidates, record the winner, and propagate it via
                // a change record (see file-sync.md § Conflicts).
                Some(winner_hex) => {
                    let winner_bytes = hex::decode(&winner_hex).map_err(|_| {
                        coded(SYNC, "invalid_request", "invalid winning_manifest_hash hex")
                    })?;
                    let verified = verify_choose_winner_signature(
                        &state,
                        &actor_id,
                        req.id,
                        &winner_bytes,
                        crate::change_signature::CarriedSignature::new(
                            req.winner_signature.as_ref(),
                            req.winner_signer_key.as_ref(),
                        ),
                    )
                    .await?;
                    let outcome = state
                        .db
                        .resolve_conflict_choose_winner_signed(
                            &actor_id,
                            req.id,
                            &winner_bytes,
                            verified.as_ref().map(|v| v.as_row()),
                        )
                        .await
                        // The minted head row is metered (2026-09-28): a typed quota
                        // refusal leaves the conflict open.
                        .map_err(crate::sync_handlers::quota_err)?;
                    if let (Some(v), ResolveWinner::Resolved { .. }) = (&verified, &outcome) {
                        v.remember_cert(&state.db)
                            .await
                            .map_err(|e| internal(SYNC, e))?;
                    }
                    match outcome {
                        ResolveWinner::NotFound => Err(coded(
                            SYNC,
                            "not_found",
                            "conflict not found or already resolved",
                        )),
                        ResolveWinner::BadCandidate => Err(coded(
                            SYNC,
                            "bad_candidate",
                            "winning_manifest_hash is not one of the conflict's candidates",
                        )),
                        ResolveWinner::Resolved { .. } => encode_reply(&ConflictResolveReply {
                            resolved: true,
                            winning_manifest_hash: Some(winner_hex),
                            extra: Default::default(),
                        }),
                    }
                }
                // Candidate-free (mark-only): flip resolved_at, propagate nothing.
                None => {
                    let resolved = state
                        .db
                        .resolve_conflict(&actor_id, req.id)
                        .await
                        .map_err(|e| internal(SYNC, e))?;
                    if !resolved {
                        return Err(coded(
                            SYNC,
                            "not_found",
                            "conflict not found or already resolved",
                        ));
                    }
                    encode_reply(&ConflictResolveReply {
                        resolved: true,
                        winning_manifest_hash: None,
                        extra: Default::default(),
                    })
                }
            }
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

// ── fauna.folders.share (cross-user shared folders, Slice 2) ──────────────

/// Core of `fauna.folders.share` (testable without an `AppState` — the
/// `welcome_deliver_core` / `channel_send_core` pattern): bind an owner-only file
/// set to a **client-created** MLS group and register the owner on the derived
/// ChannelId roster. Wire shape + design: `fauna_protocol::folders` (the
/// `FolderShare*` section).
///
/// **Storage model (decision #2 — does NOT
/// repoint `actor_id`):** `folders.actor_id` is retained as the **owner**; the
/// shared flag is the non-NULL `mls_group_id` (the raw group id, the chunk_crypto
/// root source). A folder *transitions* from owner-owned — unlike conv's
/// born-shared `__conv/<hex>` reserved sets, which never had an owner and so put
/// the channel id in the `actor_id` slot. Keeping the owner here makes the bind
/// idempotent + owner-scoped and avoids overloading `actor_id` to mean
/// pubkey-or-ChannelId. The membership gate (S2-P3) derives
/// `ChannelId::from_group_id(mls_group_id)` and admits a member via
/// `is_actor_in_channel` *alongside* owner-equality — the same authz fn conv uses.
/// (Refines the spec / `key-material-hierarchy.md` "actor_id = group_id" mirror;
/// refutable downstream by the security review.)
///
/// **FS-BIND-3 (bind-write authz):**
/// (a) `owner` is the **authenticated caller** (the connection actor passed by the
/// RpcHandler framework), never a payload field; the bind is owner-scoped
/// (`set_folder_mls_group` UPDATEs `WHERE name=? AND actor_id=owner`), so a
/// non-owner changes 0 rows → `not_found` (existence oracle closed, the N1/ST-1
/// idiom). (b) The "caller is a current member of the MLS group it binds to"
/// requirement is **not nest-verifiable** — MLS groups are client-side; the nest
/// holds no group state, only the `actor_channels` roster (confirmed: no nest
/// handler introspects MLS membership). Binding to a group the caller isn't in is
/// owner self-DoS only (they can't `export_chunk_key` a group they're not in) —
/// The owner's own problem, re-shareable. Flagged for the S2 second-pass.
async fn share_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    req: &FolderShareRequest,
) -> Result<FolderShareReply, RpcError> {
    // Decode the raw MLS group id (variable length — openMLS group ids are not
    // fixed-32). Empty / non-hex is invalid.
    let raw_group_id = hex::decode(req.group_id.trim())
        .ok()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| coded(FS, "invalid_request", "group_id must be non-empty hex"))?;

    // Derive the 32-byte ChannelId server-side (never trust a client-asserted
    // channel id; FS-BIND-3) — the same fn the client's chunk_root +
    // `welcome.deliver` use.
    let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

    // Verify the caller owns the named set **read-only** first, so a non-owner /
    // absent set folds to one `not_found` (the existence oracle, ST-RES-1) BEFORE we
    // mutate anything (claim / register / bind). The owner-scoped bind below also
    // re-checks ownership, but doing the check up front keeps the security gate and
    // the namespace claim from running for a set the caller doesn't own.
    //
    // S5b: hash-first once — `existing.name` (not `req.name`, empty on a
    // hash-addressed request) is what the bind + reply below use.
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let existing = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(&h, owner).await,
        None => db.get_folder_for_actor(&req.name, owner).await,
    }
    .map_err(|e| internal(FS, e))?
    .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;

    // **(the security review):** a bound set never
    // moves to a different MLS group. The roster (`actor_channels`) and the read
    // gate (`folder_authz`) both project over the set's *current* derived
    // ChannelId, so re-pointing `mls_group_id` silently drops every earlier member
    // from a set nobody revoked — a `principles.md` § *User always controls their
    // data* breach. The ratified target is "never a fresh group, never a re-bind"
    // (`mls-group-key-material.md` § M2 *Admitting a member*); until now it was
    // enforced only client-side, so a **non-conforming client** — whose `share_set`
    // unconditionally mints a fresh group — could still perform the breach.
    // The nest is the authority
    // for the name→group binding, so it refuses here.
    //
    // Scope: only a *differing* id is refused. Re-sending the set's own group id is
    // the add path's idempotent claimant re-bind (`share_set_add` — the share wire
    // is how the 2nd..Nth member's access row lands), and a first share (`NULL`
    // binding) is the ordinary first-binder claim. There is deliberately no
    // re-bind-after-unshare gesture: nothing in production ever unbinds a set
    // (`set_folder_mls_group(.., None)` has no production caller), and if one is
    // ever wanted it should ride an explicit unbind step, not a silent UPDATE.
    if let Some(bound) = existing.mls_group_id.as_deref()
        && bound != raw_group_id.as_slice()
    {
        return Err(coded(
            FS,
            "already_bound",
            "folder is already bound to a different MLS group; \
             sharing with another member adds them to that group",
        ));
    }

    // First-binder-wins claim on the `group_id -> ChannelId` namespace. Atomically
    // claims a fresh channel (registering the owner on the roster), accepts an
    // idempotent re-bind by the existing claimant, and **rejects everything else —
    // every claim on a roster-populated channel included, member or not** (the
    // conv-reuse branch was closed 2026-07-29: a member's claim froze their own
    // group's commits, permanently) — so a removed member can no longer
    // `share`-rebind back onto the roster and an outsider can no longer
    // self-inject onto a conversation's roster. This replaces the old
    // unconditional `register_actor_channel` self-add.
    match db
        .claim_folder_channel(owner, &channel_id)
        .await
        .map_err(|e| internal(FS, e))?
    {
        crate::db::channels::ChannelClaimOutcome::Allowed => {}
        crate::db::channels::ChannelClaimOutcome::Denied => {
            // Either the channel is already claimed by a different owner (a
            // cross-owner clobber / a removed member's `share`-rebind), or it is a
            // roster-populated channel (a conversation) — refused for anyone,
            // member or not. One generic code for both — no claim-existence oracle.
            return Err(coded(
                FS,
                "already_claimed",
                "not permitted to bind a folder to this MLS group's channel",
            ));
        }
    }

    // Owner-scoped bind: `existing` was resolved owner-scoped above, and the
    // UPDATE keys on its row id. Re-binding the same set to the same group is
    // the same UPDATE → idempotent. (The claim above already registered the
    // owner on the roster for a fresh channel.)
    let bound = db
        .set_folder_mls_group_by_id(existing.id, Some(&raw_group_id))
        .await
        .map_err(|e| internal(FS, e))?;
    if !bound {
        return Err(coded(FS, "not_found", "folder not found"));
    }

    // Share-time access grant (multi-writer Phase 1, D5): record the invited
    // member's role row with the bind — before the Welcome is delivered — so a
    // writer's grant is never unset when they join. Absent fields = no row
    // (absent row = reader, the fail-safe default). Validated like
    // `set_access_core`; never an authz input (the roster still comes only from
    // `welcome.deliver`).
    if let (Some(member_hex), Some(access)) = (&req.member_actor_id, &req.access) {
        let member = fauna_core::hex32::decode(member_hex.trim())
            .map_err(|_| coded(FS, "invalid_request", "member_actor_id must be 32-byte hex"))?;
        if access != "reader" && access != "writer" {
            return Err(coded(
                FS,
                "invalid_request",
                "access must be \"reader\" or \"writer\"",
            ));
        }
        db.set_folder_member_access(&channel_id, &member, access, None)
            .await
            .map_err(|e| internal(FS, e))?;
    }

    Ok(FolderShareReply {
        ok: true,
        folder: existing.name.clone(),
        name_hash: existing
            .name_hash
            .clone()
            .map(fauna_protocol::ByteBuf::from),
        channel_id: hex::encode(channel_id),
        extra: Default::default(),
    })
}

fn share_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_SHARE).await?;
            let req: FolderShareRequest = decode(&payload).map_err(malformed)?;
            let reply = share_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.content_key.{put,get} (shared folders, Slice 3 — M2) ────
//
// Opaque storage for the M2 content-key envelope
// (`docs/goal/architecture/mls-group-key-material.md` § M2 content-key
// mechanism). The nest holds the sealed bytes opaquely — it never has the group
// secret; the seal/open live in `fauna-mls`. Both kinds address the set by `name`
// and derive the storage key (the 32-byte `ChannelId`) from the set's stored
// `mls_group_id`, reusing `folder_authz` for the read gate exactly as the
// snapshot reads do.

/// Resolve a shared (group-bound) folder's derived `ChannelId` from its raw
/// `mls_group_id`, or a coded error if the set isn't group-bound.
fn derive_channel_id(fs: &crate::db::FolderRow) -> Result<[u8; 32], RpcError> {
    let group_id = fs.mls_group_id.as_ref().ok_or_else(|| {
        coded(
            FS,
            "not_shared",
            "folder is not shared (bind it with fauna.folders.share first)",
        )
    })?;
    Ok(fauna_mls::types::ChannelId::from_group_id(group_id).0)
}

/// Require `caller` to be the first-binder claimant of `channel_id`'s folder
/// namespace (5d-SEC). The write surfaces that act on the *channel* rather than the
/// caller's own set — `content_key.put` (the per-group envelope slot) and
/// `members.evict` (the shared roster) — gate on this so owning *some* set bound to
/// the group is insufficient. `forbidden` (not `not_found`): the caller already
/// proved ownership of their named set, so no existence oracle is at stake.
async fn require_channel_claimant(
    db: &crate::db::CacheDb,
    channel_id: &[u8; 32],
    caller: &[u8; 32],
) -> Result<(), RpcError> {
    let claimed_by = db
        .folder_channel_claimed_by(channel_id)
        .await
        .map_err(|e| internal(FS, e))?;
    if claimed_by.as_ref() == Some(caller) {
        Ok(())
    } else {
        Err(coded(
            FS,
            "forbidden",
            "not the channel's authorized folder owner",
        ))
    }
}

/// Core of `fauna.folders.content_key.put` (testable without an `AppState`):
/// the **owner** publishes the sealed envelope for their shared set. Owner-scoped
/// (members are read-only in Slices 2–3); upserts keyed by the derived ChannelId.
async fn content_key_put_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    req: &fauna_protocol::folders::ContentKeyPutRequest,
) -> Result<fauna_protocol::folders::ContentKeyPutReply, RpcError> {
    // Owner-scoped lookup folds non-owner/absent to one `not_found` (ST-RES-1).
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let fs = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(&h, owner).await,
        None => db.get_folder_for_actor(&req.name, owner).await,
    }
    .map_err(|e| internal(FS, e))?
    .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
    let channel_id = derive_channel_id(&fs)?;
    // The envelope slot is keyed solely by `channel_id`, with no per-row publisher,
    // so owning *some* set bound to the group is not enough: the
    // publisher MUST be the channel's first-binder claimant, else a group-id-knower
    // could clobber a victim group's envelope. A bound set always carries a claim
    // (`share` records it), so a legit owner passes; a non-claimant is rejected.
    require_channel_claimant(db, &channel_id, owner).await?;
    let sealed = hex::decode(req.sealed.trim())
        .ok()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| coded(FS, "invalid_request", "sealed must be non-empty hex"))?;

    // The floor stamp is monotonic nest-side (a lower stamp never lowers) —
    // KMH § M2 version floor, D2/F4.
    let floor = i64::try_from(req.current_version)
        .map_err(|_| coded(FS, "invalid_request", "current_version out of range"))?;
    db.upsert_folder_content_key(&channel_id, req.epoch, &sealed, floor)
        .await
        .map_err(|e| internal(FS, e))?;

    Ok(fauna_protocol::folders::ContentKeyPutReply {
        ok: true,
        channel_id: hex::encode(channel_id),
        extra: Default::default(),
    })
}

/// Core of `fauna.folders.content_key.get` (testable without an `AppState`):
/// any **readable** member (owner or roster member — `folder_authz`) fetches
/// the latest sealed envelope.
async fn content_key_get_core(
    db: &crate::db::CacheDb,
    caller: &[u8; 32],
    req: &fauna_protocol::folders::ContentKeyGetRequest,
) -> Result<fauna_protocol::folders::ContentKeyGetReply, RpcError> {
    // Member-aware resolve: owner OR roster member; non-member/absent → None,
    // folding to `not_found` (the ST-RES-1 name-existence oracle stays closed).
    // `nest_url` (cross-nest relay) addresses by `channel_id`, never reaches
    // this same-nest core, so `name_hash` is same-nest-read-only here too.
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let fs =
        crate::folder_authz::resolve_readable_folder(db, &req.name, name_hash.as_ref(), caller)
            .await
            .map_err(|e| internal(FS, e))?
            .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
    let channel_id = derive_channel_id(&fs)?;

    let (epoch, sealed) = db
        .get_folder_content_key(&channel_id)
        .await
        .map_err(|e| internal(FS, e))?
        .ok_or_else(|| {
            // Readable, but the owner has not published an envelope yet — distinct
            // from "not readable" so the client knows to wait + retry.
            coded(FS, "not_published", "no content-key envelope published yet")
        })?;

    Ok(fauna_protocol::folders::ContentKeyGetReply {
        // Same-nest read: the caller's own nest already projects `access` /
        // cadence / floor onto the member `FolderSummary`, so there is nothing
        // to refresh here. These stamps exist only for the cross-nest relay arm.
        caller_access: None,
        residency: None,
        home_nest_actor_id: None,
        content_key_floor: None,
        // Same-nest: the caller's own nest names the owner on its
        // `FolderSummary`; the cross-nest owner label rides the relay only.
        owner_handle: None,
        owner_domain: None,
        epoch,
        sealed: hex::encode(&sealed),
        extra: Default::default(),
    })
}

fn content_key_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_CONTENT_KEY_PUT).await?;
            let req = decode(&payload).map_err(malformed)?;
            let reply = content_key_put_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

/// The cross-nest owner label this nest forwards on a relayed content-key
/// read — the relaying nest's half of *The cross-nest owner label*'s trust
/// rule (`federation.md` § Cross-nest shared folders + channel append): the
/// home nest's stamped pair rides on only when its domain is ALREADY bound to
/// the verified identity behind `peer_url` in the pool's domain cache. This is
/// the polled path, so the reply is never delayed: a cold domain forwards
/// nothing this poll and spawns the same binding the Welcome relay awaits
/// (under the same cap), so the next poll carries the pair; a mismatch
/// forwards nothing (logged by the binding). `None` ⇒ the member keeps what it
/// holds.
async fn relayed_owner_label(
    state: &std::sync::Arc<AppState>,
    peer_url: &str,
    handle: Option<&str>,
    domain: Option<&str>,
) -> (Option<String>, Option<String>) {
    const CONTEXT: &str = "federation content-key relay: owner label";
    // The fetch just succeeded, so `originate` resolved (and cached) the
    // peer's verified identity — this is a cache hit, never a fresh dial.
    let Ok(peer_nest_id) = state.federation_pool.resolve_peer_nest_id(peer_url).await else {
        return (None, None);
    };
    let Some((handle, domain)) = crate::federation_handlers::well_formed_asserted_name(
        handle,
        domain,
        &peer_nest_id,
        CONTEXT,
    ) else {
        return (None, None);
    };
    match state.federation_pool.cached_domain_nest_id(domain).await {
        Some(id) if id == peer_nest_id => (Some(handle.to_string()), Some(domain.to_string())),
        Some(_) => (None, None),
        None => {
            if let Some(in_flight) =
                crate::federation_handlers::start_domain_verification(state, &peer_nest_id, CONTEXT)
            {
                let domain = domain.to_string();
                let task_state = std::sync::Arc::clone(state);
                state.spawn_scoped(async move {
                    let _in_flight = in_flight;
                    crate::federation_handlers::bind_domain_to_origin(
                        &task_state,
                        &domain,
                        &peer_nest_id,
                        CONTEXT,
                    )
                    .await;
                });
            }
            (None, None)
        }
    }
}

fn content_key_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_CONTENT_KEY_GET).await?;
            let req: fauna_protocol::folders::ContentKeyGetRequest =
                decode(&payload).map_err(malformed)?;

            // Cross-nest relay (Phase 2): a foreign set's sealed envelope lives
            // on its home nest — relay the read there (channel-keyed; a foreign
            // set's `name` only resolves on the home nest). The envelope stays
            // opaque ciphertext on both hops.
            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                let channel_hex = req.channel_id.as_deref().ok_or_else(|| {
                    coded(
                        FS,
                        "invalid_request",
                        "channel_id is required with nest_url",
                    )
                })?;
                return match crate::federation_pool::originate_folder_content_key_fetch(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &hex::encode(actor_id),
                    channel_hex,
                )
                .await
                {
                    // The home nest's stamp of the caller's live grant rides
                    // back verbatim (advisory-for-UI on the client, never an
                    // authz input here) — this is the federated read a foreign
                    // member's client runs on every commit poll, so it is what
                    // keeps a promotion/demotion visible without a push kind.
                    Ok(Ok(fetched)) => {
                        let (owner_handle, owner_domain) = relayed_owner_label(
                            &state,
                            peer_url,
                            fetched.owner_handle.as_deref(),
                            fetched.owner_domain.as_deref(),
                        )
                        .await;
                        encode_reply(&fauna_protocol::folders::ContentKeyGetReply {
                            epoch: fetched.epoch,
                            sealed: fetched.sealed,
                            caller_access: fetched.caller_access,
                            // Home nest's identity, owner cadence, the set's
                            // content-key floor and the folder's residency,
                            // threaded back so the member refreshes them all
                            // alongside `caller_access`.
                            home_nest_actor_id: fetched.home_nest_actor_id,
                            content_key_floor: fetched.content_key_floor,
                            residency: fetched.residency,
                            owner_handle,
                            owner_domain,
                            extra: Default::default(),
                        })
                    }
                    Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                        peer_err,
                        "the cross-nest content-key read",
                    )),
                    Err(pool_err) => {
                        tracing::error!("federation content_key fetch relay: {pool_err}");
                        Err(internal(FS, "federation fetch failed"))
                    }
                };
            }

            let reply = content_key_get_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.write_token.get (Phase 3 — cross-nest writer byte token) ───

/// `fauna.folders.write_token.get` — a cross-nest **writer**'s own nest relays
/// a short-lived byte-plane write token from the set's home nest
/// (`fauna.federation.folder.write_token.mint`). Always a relay: a same-nest
/// writer uploads under its own session bearer, so a token only makes sense for
/// a foreign set. The home nest applies the foreign-member + `access == 'writer'`
/// gate before minting; S5 maps an old home nest's `unauthenticated` to the
/// shared typed `peer_nest_outdated` ("the home nest needs an update").
fn write_token_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_WRITE_TOKEN_GET).await?;
            let req: fauna_protocol::folders::WriteTokenGetRequest =
                decode(&payload).map_err(malformed)?;
            if req.nest_url.is_empty() {
                return Err(coded(FS, "invalid_request", "nest_url is required"));
            }
            if req.channel_id.is_empty() {
                return Err(coded(FS, "invalid_request", "channel_id is required"));
            }
            match crate::federation_pool::originate_folder_write_token_mint(
                &state.federation_pool,
                &state,
                &req.nest_url,
                &hex::encode(actor_id),
                &req.channel_id,
            )
            .await
            {
                Ok(Ok((token, expires_at))) => {
                    encode_reply(&fauna_protocol::folders::WriteTokenGetReply {
                        token,
                        expires_at,
                        extra: Default::default(),
                    })
                }
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest write-token mint",
                )),
                Err(pool_err) => {
                    tracing::error!("federation write_token mint relay: {pool_err}");
                    Err(internal(FS, "federation mint failed"))
                }
            }
        })
    })
}

/// `fauna.folders.read_token.get` — the read-scoped twin of
/// [`write_token_get_handler`]: a cross-nest **member**'s own nest relays a
/// short-lived byte-plane read token from the set's home nest
/// (`fauna.federation.folder.read_token.mint`), which mints behind the
/// foreign-member gate alone. Always a relay, with the same typed mapping of an
/// old home nest.
fn read_token_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_READ_TOKEN_GET).await?;
            let req: fauna_protocol::folders::ReadTokenGetRequest =
                decode(&payload).map_err(malformed)?;
            if req.nest_url.is_empty() {
                return Err(coded(FS, "invalid_request", "nest_url is required"));
            }
            if req.channel_id.is_empty() {
                return Err(coded(FS, "invalid_request", "channel_id is required"));
            }
            match crate::federation_pool::originate_folder_read_token_mint(
                &state.federation_pool,
                &state,
                &req.nest_url,
                &hex::encode(actor_id),
                &req.channel_id,
            )
            .await
            {
                Ok(Ok((token, expires_at))) => {
                    encode_reply(&fauna_protocol::folders::ReadTokenGetReply {
                        token,
                        expires_at,
                        extra: Default::default(),
                    })
                }
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest read-token mint",
                )),
                Err(pool_err) => {
                    tracing::error!("federation read_token mint relay: {pool_err}");
                    Err(internal(FS, "federation mint failed"))
                }
            }
        })
    })
}

// ── fauna.folders.members.evict (shared folders, Slice 3 — F1/OBS-1) ───────
//
// The metadata-confidentiality half of rotate-on-removal: the **owner** drops a
// cross-user actor from the set's `actor_channels` roster so the removed member
// loses discovery-metadata reads (snapshot lists / names / sizes). The content
// forward-secrecy half is the rotated content key (the crypto boundary OBS-1
// rests on); this closes F1, the un-evicted-roster metadata leak. Distinct from
// `members.remove`, which removes the owner's own sync *device* from
// `folder_members`; this removes another *user* (`ActorId`) from the shared
// roster. The nest can't verify MLS membership (client-side), so it trusts the
// owner's owner-scoped assertion — consistent with FS-SHARE-1 (the roster is
// discovery/metadata, not the confidentiality boundary).

/// Core of `fauna.folders.members.evict` (testable without an `AppState`): the
/// **owner** evicts a cross-user member from their shared set's roster. Owner-scoped
/// (folds non-owner/absent to one `not_found`, ST-RES-1); idempotent (an
/// already-absent member returns `evicted: false`).
async fn members_evict_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    req: &fauna_protocol::folders::MemberEvictRequest,
) -> Result<fauna_protocol::folders::MemberEvictReply, RpcError> {
    let member = fauna_core::hex32::decode(req.member.trim())
        .map_err(|_| coded(FS, "invalid_request", "member must be 32-byte hex"))?;

    // Owner-scoped lookup folds non-owner/absent to one `not_found` (ST-RES-1).
    let name_hash = parse_name_hash(FS, &req.name_hash)?;
    let fs = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(&h, owner).await,
        None => db.get_folder_for_actor(&req.name, owner).await,
    }
    .map_err(|e| internal(FS, e))?
    .ok_or_else(|| coded(FS, "not_found", "folder not found"))?;
    // `not_shared` for an owner-only (unbound) set — there is no roster to evict from.
    let channel_id = derive_channel_id(&fs)?;
    // Only the channel's first-binder claimant may evict from its roster — so a
    // different owner who bound their own set to the same group cannot kick the
    // victim's members.
    require_channel_claimant(db, &channel_id, owner).await?;

    let evicted = db
        .evict_actor_from_channel(&member, &channel_id)
        .await
        .map_err(|e| internal(FS, e))?;
    // A cross-nest member's membership is their `channel_foreign_members` row,
    // not an `actor_channels` row — delete it too so the federated fetch relay
    // dies with the membership (S8: the fetch authorization must not survive
    // eviction; Phase 2, `ui/folders.md` § Sharing → Cross-nest members).
    // Needs no federation call — the roster is home-nest-local. Idempotent.
    let foreign_evicted = db
        .remove_foreign_channel_member(&channel_id, &member)
        .await
        .map_err(|e| internal(FS, e))?;
    // The access grant dies with the membership (multi-writer Phase 1): a later
    // re-add starts from the fail-safe reader default, never a resurrected
    // `writer`. Idempotent, like the eviction itself.
    db.delete_folder_member_role(&channel_id, &member)
        .await
        .map_err(|e| internal(FS, e))?;

    Ok(fauna_protocol::folders::MemberEvictReply {
        ok: true,
        channel_id: hex::encode(channel_id),
        evicted: evicted || foreign_evicted,
        extra: Default::default(),
    })
}

fn members_evict_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_MEMBERS_EVICT).await?;
            let req = decode(&payload).map_err(malformed)?;
            let reply = members_evict_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.leave (shared folders, Slice 3 — recipient self-remove) ──
//
// The recipient counterpart to `members.evict`: a **member** voluntarily leaves a
// set shared *with* them. Where `members.evict` is owner-scoped and addressed by the
// set's `name` (which a member neither owns nor knows), `leave` is **self-scoped**
// and addressed by the raw `mls_group_id` the member holds in their B3 member-visible
// `FolderSummary`. It drops **only the authenticated caller's own** row from the
// derived channel roster — so there is no owner lookup and no first-binder-claimant
// check ("you can always remove yourself"; a caller can only ever remove their own
// `(actor, channel)` row, so an arbitrary group id is harmless). The leaver's client
// also locally forgets the MLS group (`MlsEngine::forget_group`), dropping the set
// from `has_group`-filtered list rendering; off the roster, their `content_key.get`
// folds to `not_found`, so they stop receiving content-key rotations. A voluntary
// leave does **not** rotate the owner's content key — the leaver keeps the
// generations they already held (`mls-group-key-material.md` § M2: "forward secrecy
// from yourself is not a threat"). Same-nest today; cross-nest leave (the roster
// lives on the sharer's home nest) is a follow-on alongside B4.

/// Core of `fauna.folders.leave` (testable without an `AppState`): drop the
/// **authenticated caller's own** `ActorId` from the roster of the channel derived
/// from `req.group_id`. Self-scoped + idempotent (`left: false` if the caller was
/// not a member).
async fn leave_core(
    db: &crate::db::CacheDb,
    caller: &[u8; 32],
    req: &fauna_protocol::folders::MemberLeaveRequest,
) -> Result<fauna_protocol::folders::MemberLeaveReply, RpcError> {
    // Decode the raw MLS group id (variable length — openMLS group ids are not
    // fixed-32), mirroring `share_core`. Empty / non-hex is invalid.
    let raw_group_id = hex::decode(req.group_id.trim())
        .ok()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| coded(FS, "invalid_request", "group_id must be non-empty hex"))?;
    let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

    // The only mutation: drop the caller's *own* roster row (the scoped
    // `(actor, channel)` DELETE). No owner/claimant gate — self-removal is always
    // permitted, and the caller can never affect another actor's row.
    let left = db
        .evict_actor_from_channel(caller, &channel_id)
        .await
        .map_err(|e| internal(FS, e))?;
    // The caller's own access grant leaves with them (multi-writer Phase 1) —
    // still self-scoped (only the caller's `(actor, channel)` row).
    db.delete_folder_member_role(&channel_id, caller)
        .await
        .map_err(|e| internal(FS, e))?;

    Ok(fauna_protocol::folders::MemberLeaveReply {
        ok: true,
        channel_id: hex::encode(channel_id),
        left,
        extra: Default::default(),
    })
}

fn leave_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_LEAVE).await?;
            let req: fauna_protocol::folders::MemberLeaveRequest =
                decode(&payload).map_err(malformed)?;

            // Cross-nest leave (Phase 2, `ui/folders.md` § Sharing → Leave):
            // a foreign set's roster row is the HOME nest's
            // `channel_foreign_members` entry — relay the self-scoped delete
            // there. This nest holds no row for the set (by design), so the
            // local `leave_core` mutations would all be no-ops; the relay IS
            // the leave. Generations already held are not revoked (parity with
            // same-nest voluntary leave — the client still forgets the group
            // locally via `forget_group`).
            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                let raw_group_id = hex::decode(req.group_id.trim())
                    .ok()
                    .filter(|b| !b.is_empty())
                    .ok_or_else(|| {
                        coded(FS, "invalid_request", "group_id must be non-empty hex")
                    })?;
                let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
                return match crate::federation_pool::originate_channel_leave(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &hex::encode(actor_id),
                    &hex::encode(channel_id),
                )
                .await
                {
                    Ok(Ok(removed)) => encode_reply(&fauna_protocol::folders::MemberLeaveReply {
                        ok: true,
                        channel_id: hex::encode(channel_id),
                        left: removed,
                        extra: Default::default(),
                    }),
                    Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                        peer_err,
                        "the cross-nest leave",
                    )),
                    Err(pool_err) => {
                        tracing::error!("federation channel leave relay: {pool_err}");
                        Err(internal(FS, "federation leave failed"))
                    }
                };
            }

            let reply = leave_core(&state.db, &actor_id, &req).await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.folders.public.fetch ─────────────────────────────────────────────

/// `fauna.folders.public.fetch` — a follower's read of a `public`-audience
/// folder (`folders.md` § Publicly-synced follow; `federation.md` § The public
/// folder read plane owns the kinds and gates). Phase 4 slice 4f-i.
///
/// Two arms, one core. With `nest_url` the caller's own nest **relays** to the
/// folder's home nest, exactly as every cross-nest client read relays; without
/// it the folder is homed here and
/// [`crate::folder_public::resolve_public_folder`] serves it locally — the
/// same-nest follow, where two users share a nest. Both arms authorize through
/// that one inverse-shaped gate, so neither can serve a sealed row.
///
/// **The connection actor is authenticated but never forwarded.** The bearer
/// permission check below is this nest's own "may this account use this kind"
/// question; the federation hop carries no requesting actor at all, so who
/// follows what stays on the follower's own nest
/// (`FedFolderPublicFetchRequest`'s doc comment).
///
/// Nothing is written on either nest: a follow's whole state is the follower's
/// own `fauna.state.follows` plane row.
fn public_fetch_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, FS, &actor_id, KIND_FOLDERS_PUBLIC_FETCH).await?;
            let req: FoldersPublicFetchRequest = decode(&payload).map_err(malformed)?;

            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                return match crate::federation_pool::originate_folder_public_fetch(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    req.owner_actor_id.as_deref(),
                    req.folder_name.as_deref(),
                    req.folder_id,
                    req.since,
                    req.limit,
                )
                .await
                {
                    Ok(Ok(reply)) => encode_reply(&FoldersPublicFetchReply {
                        folder_id: reply.folder_id,
                        name: reply.name,
                        home_nest_actor_id: reply.home_nest_actor_id,
                        changes: reply.changes,
                        extra: Default::default(),
                    }),
                    Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                        peer_err,
                        "the public folder read",
                    )),
                    Err(pool_err) => {
                        tracing::error!("federation folder public fetch relay: {pool_err}");
                        Err(internal(FS, "federation fetch failed"))
                    }
                };
            }

            let address = crate::federation_handlers::public_address_from_wire(
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
            encode_reply(&FoldersPublicFetchReply {
                folder_id: folder.id,
                name: folder.name,
                // The folder is homed HERE, so this deployment's identity is the
                // follower's byte-plane SPKI pin root — stamped on the local arm
                // too, so a follower's pinning code has one shape either way.
                home_nest_actor_id: Some(hex::encode(state.nest_identity.public_key_bytes())),
                changes,
                extra: Default::default(),
            })
        })
    })
}

/// Register the folder management surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_folders_kinds`.
/// All 22 are `forbid_replay = false` @5 s (reads + fast local DB mutations).
pub fn register_folders_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        (KIND_FOLDERS_CREATE, create_handler()),
        (KIND_FOLDERS_LIST, list_handler()),
        (KIND_FOLDERS_UPDATE, update_handler()),
        (KIND_FOLDERS_SET_WEB_PAYWALL, set_web_paywall_handler()),
        (KIND_FOLDERS_DELETE, delete_handler()),
        (KIND_FOLDERS_DEVICES, devices_handler()),
        (KIND_FOLDERS_MEMBERS_LIST, members_list_handler()),
        (
            KIND_FOLDERS_MEMBERS_SET_ACCESS,
            members_set_access_handler(),
        ),
        (
            KIND_FOLDERS_MEMBERS_LIST_ACTORS,
            actor_members_list_handler(),
        ),
        (KIND_FOLDERS_MEMBERS_REMOVE, members_remove_handler()),
        (KIND_FOLDERS_PLACES_SET, places_set_handler()),
        (KIND_FOLDERS_MEMBERS_EVICT, members_evict_handler()),
        (KIND_FOLDERS_LEAVE, leave_handler()),
        (KIND_FOLDERS_SHARE, share_handler()),
        (KIND_FOLDERS_CONTENT_KEY_PUT, content_key_put_handler()),
        (KIND_FOLDERS_CONTENT_KEY_GET, content_key_get_handler()),
        (KIND_FOLDERS_WRITE_TOKEN_GET, write_token_get_handler()),
        (KIND_FOLDERS_READ_TOKEN_GET, read_token_get_handler()),
        (KIND_FOLDERS_LEASE_ACQUIRE, lease_acquire_handler()),
        (KIND_FOLDERS_LEASE_RELEASE, lease_release_handler()),
        (KIND_FOLDERS_PUBLIC_FETCH, public_fetch_handler()),
        ("fauna.sync.conflicts.list", conflicts_list_handler()),
        ("fauna.sync.conflicts.report", conflict_report_handler()),
        ("fauna.sync.conflicts.resolve", conflict_resolve_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
    // The cross-nest roster relay — a pure read, on the federation hop's 30 s
    // deadline (the `channel.actors_remote` posture).
    b.add(
        KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: actor_members_list_remote_handler(),
        },
    );
    // The served-era adoption: idempotent (replay-safe), 30 s for a full
    // page's verifications in one transaction.
    b.add(
        KIND_FOLDERS_SERVED_ROWS_ADOPT,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: served_rows_adopt_handler(),
        },
    );
}

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;

    /// A `for_test` state with the web-content service wired, plus a rendered
    /// one-template website for `actor` — the transition tests' shared
    /// arrangement. Returns the state and the blob-store tempdir guard.
    async fn state_with_rendered_site(
        actor: &[u8; 32],
    ) -> (std::sync::Arc<AppState>, tempfile::TempDir) {
        use crate::blob_store::{BlobStoreBackend, DiskBlobStore};
        use crate::web_content::service::WebContentService;

        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: std::sync::Arc<dyn BlobStoreBackend> =
            std::sync::Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let mut state = AppState::for_test(db.clone());
        state.web_content_service = Some(std::sync::Arc::new(WebContentService::new(
            db.clone(),
            store.clone(),
        )));
        let state = std::sync::Arc::new(state);
        state
            .db
            .create_user(actor, "free", "site-owner")
            .await
            .unwrap();

        // A live website folder with one template, rendered.
        let folder_id = state
            .db
            .create_folder_with_options("site", actor, Default::default())
            .await
            .unwrap();
        state
            .db
            .update_folder_for_user(
                "site",
                actor,
                crate::db::FolderUpdate {
                    website_enabled: Some(true),
                    audience: Some(Some("public")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let manifest = crate::web_content::file_bytes::seed_synced_file(
            &store,
            None,
            b"<html><body>canary-door-marker</body></html>",
            None,
        )
        .await;
        state
            .db
            .upsert_web_file(
                actor,
                "index.html.hbs",
                &manifest,
                "text/plain",
                Some(folder_id),
                None,
            )
            .await
            .unwrap();
        let wcs = state.web_content_service.as_ref().unwrap();
        wcs.render_published_posts(actor).await.unwrap();
        assert!(
            state
                .db
                .get_web_rendered(actor, "index.html")
                .await
                .unwrap()
                .is_some(),
            "arrangement: the template must render while the site is live"
        );
        (state, dir)
    }

    /// Switching the website off through the production
    /// door must stop the RENDERED half too. `web_rendered` rows carry no
    /// `folder_id`, so they cannot be gated at the serve door (a folder-less
    /// default site is legitimate); the transition itself fires the idempotent
    /// re-render, whose folder-aware listing (part (a)) drops the disabled
    /// folder's pages.
    #[tokio::test]
    async fn website_toggle_off_stops_the_rendered_half() {
        let actor = [0x5e_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;

        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(false),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the toggle-off update must succeed");

        assert!(
            state
                .db
                .get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "the rendered half kept serving after the owner switched the website off"
        );
    }

    /// Row 182: `web_files` is a PROJECTION of the folder's live sync heads,
    /// and the toggle-ON transition must (re)build it. Before the backfill,
    /// enabling the website toggle on an already-synced folder served nothing
    /// until every file happened to re-record — for a static site that never
    /// changes, indefinitely — which broke the phase-4 no-migration ruling's
    /// own premise (legacy web-mode rows map to toggle-off, "owner re-enables
    /// by hand") and the followed-folder "turn it on" affordance.
    async fn state_with_synced_public_folder(
        actor: &[u8; 32],
    ) -> (std::sync::Arc<AppState>, tempfile::TempDir, i64) {
        use crate::blob_store::{BlobStoreBackend, DiskBlobStore};
        use crate::web_content::service::WebContentService;

        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: std::sync::Arc<dyn BlobStoreBackend> =
            std::sync::Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let mut state = AppState::for_test(db.clone());
        state.web_content_service = Some(std::sync::Arc::new(WebContentService::new(
            db.clone(),
            store.clone(),
        )));
        let state = std::sync::Arc::new(state);
        state
            .db
            .create_user(actor, "free", "site-owner")
            .await
            .unwrap();
        // A PUBLIC folder synced BEFORE the website toggle: its head rows rest
        // plaintext paths (the class that can be a URL at all), and nothing has
        // routed them into `web_files` because the toggle was off at arrival.
        let folder_id = state
            .db
            .create_folder_with_options("site", actor, Default::default())
            .await
            .unwrap();
        state
            .db
            .update_folder_for_user(
                "site",
                actor,
                crate::db::FolderUpdate {
                    audience: Some(Some("public")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        (state, dir, folder_id)
    }

    async fn seed_head(
        state: &AppState,
        dir: &tempfile::TempDir,
        actor: &[u8; 32],
        folder_id: i64,
        path: &str,
        bytes: &[u8],
    ) {
        use crate::blob_store::{BlobStoreBackend, DiskBlobStore};
        let store: std::sync::Arc<dyn BlobStoreBackend> =
            std::sync::Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let manifest =
            crate::web_content::file_bytes::seed_synced_file(&store, None, bytes, None).await;
        state
            .db
            .record_sync_change(
                actor,
                &fauna_core::sync::path_hash(path),
                Some(&manifest),
                bytes.len() as i64,
                "create",
                Some(folder_id),
                None,
                Some(path),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn website_toggle_on_backfills_already_synced_heads() {
        let actor = [0x6a_u8; 32];
        let (state, dir, folder_id) = state_with_synced_public_folder(&actor).await;
        seed_head(
            &state,
            &dir,
            &actor,
            folder_id,
            "index.html.hbs",
            b"<html><body>PROBE-ROW-182-BACKFILL</body></html>",
        )
        .await;
        seed_head(&state, &dir, &actor, folder_id, "logo.png", b"png-bytes").await;
        // A server-side-extension file must stay refused at backfill exactly as
        // it is at sync-time routing.
        seed_head(&state, &dir, &actor, folder_id, "hack.php", b"<?php ?>").await;

        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(true),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the toggle-on update must succeed");

        assert!(
            state
                .db
                .get_web_file(&actor, "logo.png")
                .await
                .unwrap()
                .is_some(),
            "an already-synced static asset must serve after the owner enables \
             the website toggle — the enable-time backfill did not engage"
        );
        assert!(
            state
                .db
                .get_web_file(&actor, "hack.php")
                .await
                .unwrap()
                .is_none(),
            "the backfill must refuse server-side extensions exactly like \
             sync-time routing"
        );
        assert!(
            state
                .db
                .get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_some(),
            "a backfilled template must feed the transition re-render — the \
             backfill must run BEFORE the transition render"
        );
    }

    #[tokio::test]
    async fn website_toggle_on_reconcile_drops_stale_rows_without_a_live_head() {
        let actor = [0x6b_u8; 32];
        let (state, dir, folder_id) = state_with_synced_public_folder(&actor).await;
        seed_head(
            &state,
            &dir,
            &actor,
            folder_id,
            "keep.html",
            b"<html>keep</html>",
        )
        .await;
        // A file created and then DELETED while the toggle was off: its
        // `web_files` row (from an earlier enabled window) survived, because
        // `route_web_file_change` only fires while the toggle is on at arrival.
        seed_head(
            &state,
            &dir,
            &actor,
            folder_id,
            "gone.html",
            b"<html>gone</html>",
        )
        .await;
        let stale = state.db.get_web_file(&actor, "gone.html").await.unwrap();
        assert!(stale.is_none(), "arrangement: nothing routed yet");
        let manifest = [0x42_u8; 32];
        state
            .db
            .upsert_web_file(
                &actor,
                "gone.html",
                &manifest,
                "text/html",
                Some(folder_id),
                None,
            )
            .await
            .unwrap();
        state
            .db
            .record_sync_change(
                &actor,
                &fauna_core::sync::path_hash("gone.html"),
                None,
                0,
                "delete",
                Some(folder_id),
                None,
                Some("gone.html"),
            )
            .await
            .unwrap();

        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(true),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the toggle-on update must succeed");

        assert!(
            state
                .db
                .get_web_file(&actor, "gone.html")
                .await
                .unwrap()
                .is_none(),
            "a row whose path has no live head must be dropped by the \
             toggle-ON reconcile — a delete recorded while the toggle was off \
             never fired delete_web_file, and re-serving it is resurrection"
        );
        assert!(
            state
                .db
                .get_web_file(&actor, "keep.html")
                .await
                .unwrap()
                .is_some(),
            "the live head beside it must backfill"
        );
    }

    /// Row 184 — the reconcile's drop pass may only judge the class it can
    /// actually observe. Its live set is built from the folder's **plaintext**
    /// heads (a sealed head rests `path = NULL`, S9), so a SEALED `web_files`
    /// row is never in it — and dropping on that absence deletes rows the nest
    /// has no evidence about, which is the whole sealed projection.
    ///
    /// Live today, and not only after row 184: a sealed folder accumulates
    /// sealed rows from ordinary sync-time routing the moment its toggle is on,
    /// and any later serving transition landing enabled — a bind flipping the
    /// audience `private` → `shared` while the site keeps serving — wipes them.
    /// The client's `converge_corpus_to_website` would not even re-drive it: its
    /// marker still reads `served`, because nothing about the toggle or the
    /// sealed-ness changed. The site simply goes dark.
    #[tokio::test]
    async fn website_toggle_on_reconcile_never_drops_a_sealed_row_it_cannot_see() {
        let actor = [0x8d_u8; 32];
        let (state, dir, folder_id) = state_with_synced_public_folder(&actor).await;
        // A plaintext head beside it, so the reconcile genuinely runs its fold
        // rather than short-circuiting on an empty one.
        seed_head(
            &state,
            &dir,
            &actor,
            folder_id,
            "keep.html",
            b"<html>keep</html>",
        )
        .await;
        // The sealed row a client's own re-record landed: `content_key_version`
        // set, and no plaintext head for the nest to match it against.
        let manifest = [0x8d_u8; 32];
        state
            .db
            .upsert_web_file(
                &actor,
                "chapter-one.pdf",
                &manifest,
                "application/pdf",
                Some(folder_id),
                Some(7),
            )
            .await
            .unwrap();

        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(true),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the toggle-on update must succeed");

        let sealed = state
            .db
            .get_web_file(&actor, "chapter-one.pdf")
            .await
            .unwrap()
            .expect(
                "a sealed row must survive the reconcile: its head rests no plaintext \
                 name, so its absence from the live set is ignorance, not evidence",
            );
        assert_eq!(
            sealed.content_key_version,
            Some(7),
            "and it must survive intact, seal generation included"
        );
        assert!(
            state
                .db
                .get_web_file(&actor, "keep.html")
                .await
                .unwrap()
                .is_some(),
            "the plaintext half of the reconcile is unchanged"
        );
    }

    /// A public folder flipped back must stop
    /// feeding the rendered half on the same transition — a prior fix
    /// revoked the `web_files` serve on the next request; this is the same
    /// revoke on the render plane.
    #[tokio::test]
    async fn audience_flip_back_stops_the_rendered_half() {
        let actor = [0x5f_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;

        let req = FolderUpdateRequest {
            name: "site".into(),
            audience: Some("private".into()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the audience flip-back must succeed");

        assert!(
            state
                .db
                .get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "the rendered half kept serving after the folder left the public audience"
        );
    }

    /// Deleting a website folder is the third
    /// transition that must stop the rendered half — and the folder's
    /// `web_files` projection rows die with the folder's sync rows instead of
    /// orphaning forever behind the serve gate.
    #[tokio::test]
    async fn deleting_a_website_folder_stops_the_rendered_half() {
        let actor = [0x60_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;

        let req = FolderDeleteRequest {
            name: "site".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        delete_handler()(state.clone(), actor, payload)
            .await
            .expect("the delete must succeed");

        assert!(
            state
                .db
                .get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "the rendered half kept serving after its website folder was deleted"
        );
        assert!(
            state
                .db
                .get_web_file(&actor, "index.html.hbs")
                .await
                .unwrap()
                .is_none(),
            "the folder's web_files projection rows must die with the folder"
        );
    }

    /// Make every render fail BEFORE `render_for_actor`'s clear with a real
    /// storage error: the render reads the declared region ahead of the clear,
    /// and no folder door touches that table.
    async fn break_the_render(state: &std::sync::Arc<AppState>) {
        state
            .db
            .conn()
            .await
            .execute_batch("ALTER TABLE nest_region RENAME TO nest_region_broken")
            .unwrap();
        let wcs = state.web_content_service.as_ref().unwrap();
        assert!(
            wcs.render_published_posts(&[0u8; 32]).await.is_err(),
            "precondition: the render now errors before its clear"
        );
    }

    async fn assert_site_dark_and_discharged(state: &AppState, actor: &[u8; 32], door: &str) {
        assert!(
            state
                .db
                .get_web_rendered(actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "{door}: a revoking door whose render errors must clear the site, not keep \
             serving the folder's pages"
        );
        assert!(
            state.db.list_web_render_owed().await.unwrap().is_empty(),
            "{door}: a fail-closed clear is a completed revoke and discharges the marker"
        );
    }

    /// A serving transition that takes a folder off the site is a revoke, so
    /// its door fails closed (`web-content-hosting.md` § Routing, render,
    /// serving → *A revoke is durable*).
    #[tokio::test]
    async fn website_toggle_off_fails_closed_when_its_render_errors() {
        let actor = [0x61_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;
        break_the_render(&state).await;

        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(false),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("a render failure never fails the update");

        assert_site_dark_and_discharged(&state, &actor, "toggle-off").await;
    }

    /// The delete leg of the same rule.
    #[tokio::test]
    async fn deleting_a_website_folder_fails_closed_when_its_render_errors() {
        let actor = [0x62_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;
        break_the_render(&state).await;

        let req = FolderDeleteRequest {
            name: "site".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        delete_handler()(state.clone(), actor, payload)
            .await
            .expect("a render failure never fails the delete");

        assert_site_dark_and_discharged(&state, &actor, "folder delete").await;
    }

    /// The nest stops between a serving transition's commit and its render
    /// (simulated by running the door's own transaction alone). The marker
    /// that transaction carried is what the boot drain — and the owner's
    /// retried update, which changes nothing the second time — renders from.
    #[tokio::test]
    async fn a_serving_transition_torn_before_its_render_stays_owed() {
        let actor = [0x63_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;

        state
            .db
            .update_folder_for_user(
                "site",
                &actor,
                crate::db::FolderUpdate {
                    website_enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            state.db.list_web_render_owed().await.unwrap(),
            vec![actor],
            "the transition's own transaction records the owed render"
        );

        // The retry: the stored state already says off, so nothing changes.
        let req = FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(false),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        update_handler()(state.clone(), actor, payload)
            .await
            .expect("the retried update must succeed");

        assert_site_dark_and_discharged(&state, &actor, "retried toggle-off").await;
    }

    /// An update that moves no serving state owes no render — the marker is
    /// not a side effect of every folder edit.
    #[tokio::test]
    async fn a_non_serving_folder_update_owes_no_render() {
        let actor = [0x64_u8; 32];
        let (state, _dir) = state_with_rendered_site(&actor).await;

        state
            .db
            .update_folder_for_user(
                "site",
                &actor,
                crate::db::FolderUpdate {
                    website_enabled: Some(true),
                    webdav_enabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(state.db.list_web_render_owed().await.unwrap().is_empty());
    }

    /// The delete guard keys on the RESOLVED row's name, never
    /// `req.name`. The boot hash-companion pass stamps `name_hash` on
    /// reserved rails too (`reconcile_path_sealing_companions`, every boot),
    /// so a rail is hash-addressable with an EMPTY
    /// `req.name` — a guard reading `req.name` would wave that delete
    /// through. In-crate because arranging the booted shape needs the
    /// test-only companion twin (integration tests build without `cfg(test)`).
    #[tokio::test]
    async fn delete_guard_keys_on_the_resolved_name_not_req_name() {
        let state = std::sync::Arc::new(AppState::for_test(std::sync::Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let actor = [0x7d_u8; 32];
        // The authority gate refuses an actor with no `users` row.
        state
            .db
            .create_user(&actor, "free", "rail-owner")
            .await
            .unwrap();

        // A genuine rail (production drafts-put path), then the booted-nest
        // shape: its name_hash stamped exactly as the companion pass does.
        state
            .db
            .record_drafts_blob_change(
                &actor,
                fauna_protocol::drafts::RAIL_CONVERSATIONS,
                &[0xAA_u8; 32],
                64,
            )
            .await
            .unwrap();
        state
            .db
            .stamp_folder_name_hash_like_the_backfill("__drafts", &actor)
            .await
            .unwrap();

        let req = FolderDeleteRequest {
            name: String::new(),
            name_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("__drafts").to_vec(),
            )),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = delete_handler()(state.clone(), actor, payload)
            .await
            .expect_err("a hash-addressed rail delete must refuse on the RESOLVED name");
        assert_eq!(err.code, "fauna.folders.invalid_request");

        assert!(
            state
                .db
                .get_folder_for_actor("__drafts", &actor)
                .await
                .unwrap()
                .is_some(),
            "the rail survives the hash-addressed delete attempt"
        );
    }

    /// S2-P2: the owner binds an owner-only set to a client-created MLS group.
    /// The raw group id (variable length — proving the `set_folder_mls_group`
    /// widening) is persisted verbatim, the nest-derived ChannelId is echoed, and
    /// the owner is registered on that ChannelId's roster. Re-share is idempotent.
    #[tokio::test]
    async fn share_core_binds_and_registers_owner_on_derived_channel() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        // A 20-byte raw MLS group id — NOT fixed-32, proving the widening.
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

        db.create_folder("shared", &owner).await.unwrap();

        let req = FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&raw_group_id),
            ..Default::default()
        };
        let reply = share_core(&db, &owner, &req).await.unwrap();

        assert!(reply.ok);
        assert_eq!(reply.folder, "shared");
        assert_eq!(
            reply.channel_id,
            hex::encode(channel_id),
            "reply echoes the nest-derived ChannelId (client never asserts it)"
        );
        // The raw (variable-length) group id is persisted verbatim — the
        // chunk_crypto root source; actor_id is NOT repointed (stays the owner).
        let fs = db
            .get_folder_for_actor("shared", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.mls_group_id, Some(raw_group_id.clone()));
        assert_eq!(fs.actor_id, owner.to_vec(), "actor_id stays the owner");
        // The owner is registered on the derived ChannelId roster (the gate scope).
        assert!(
            db.is_actor_in_channel(&owner, &channel_id).await.unwrap(),
            "the owner is registered on the group roster"
        );
        // Idempotent re-share (owner-scoped, same value) succeeds.
        assert!(
            share_core(&db, &owner, &req).await.is_ok(),
            "re-share is idempotent"
        );
    }

    /// A bound set never moves to a different MLS group. A
    /// `share` naming a fresh group id for an already-bound set is refused
    /// (`already_bound`) with nothing mutated — no re-bind, and no claim on the
    /// fresh channel — while the set's *own* id stays the idempotent re-bind the
    /// add path sends. The tier_3 twin (with a real member on the roster whose
    /// read survives) is `conformance_shared_folders.rs::
    /// share_refuses_to_rebind_a_bound_set_to_a_different_group_real_nest`.
    #[tokio::test]
    async fn share_core_refuses_a_rebind_to_a_different_group() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let bound = vec![0x7cu8; 20];
        let fresh = vec![0x5au8; 20];
        let fresh_channel = fauna_mls::types::ChannelId::from_group_id(&fresh).0;

        db.create_folder("shared", &owner).await.unwrap();
        let first = share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&bound),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let err = share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&fresh),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.folders.already_bound");

        assert_eq!(
            db.get_folder_for_actor("shared", &owner)
                .await
                .unwrap()
                .unwrap()
                .mls_group_id,
            Some(bound.clone()),
            "the refused share left the binding alone"
        );
        assert!(
            !db.is_actor_in_channel(&owner, &fresh_channel)
                .await
                .unwrap(),
            "the refusal lands before the namespace claim — no stray roster row"
        );

        // The add path's shape (the set's own id) is still accepted.
        let again = share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&bound),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(again.channel_id, first.channel_id);
    }

    /// Multi-writer Phase 1: `members.set_access` grants a member `writer` (+
    /// cap), and `members.list_actors` projects the grant on the member row —
    /// `Some("reader")`/0-bytes for an ungranted member, `None` fields on the
    /// owner row (the owner has no grant).
    #[tokio::test]
    async fn set_access_core_grants_and_list_actors_projects_it() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let writer = [0xb2u8; 32];
        let plain = [0xc3u8; 32];
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

        db.create_folder("shared", &owner).await.unwrap();
        share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Both members joined (welcome.deliver's roster registration).
        db.register_actor_channel(&writer, &channel_id)
            .await
            .unwrap();
        db.register_actor_channel(&plain, &channel_id)
            .await
            .unwrap();

        let reply = set_access_core(
            &db,
            &owner,
            &fauna_protocol::folders::MemberSetAccessRequest {
                name: "shared".into(),
                actor_id: hex::encode(writer),
                access: "writer".into(),
                byte_cap: Some(4096),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(reply.ok);

        let listed = actor_members_list_core(
            &db,
            &owner,
            &fauna_protocol::folders::ActorMembersListRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let row = |actor: &[u8; 32]| {
            listed
                .members
                .iter()
                .find(|m| m.actor_id == hex::encode(actor))
                .unwrap()
                .clone()
        };
        let w = row(&writer);
        assert_eq!(w.access.as_deref(), Some("writer"));
        assert_eq!(w.byte_cap, Some(4096));
        assert_eq!(w.bytes_used, Some(0));
        let p = row(&plain);
        assert_eq!(
            p.access.as_deref(),
            Some("reader"),
            "ungranted member projects the explicit reader default"
        );
        assert_eq!(p.byte_cap, None);
        assert_eq!(p.bytes_used, Some(0));
        let o = row(&owner);
        assert_eq!(o.access, None, "the owner row carries no grant fields");
        assert_eq!(o.byte_cap, None);
        assert_eq!(o.bytes_used, None);

        // Demotion is a plain edit (never rotates): writer → reader.
        set_access_core(
            &db,
            &owner,
            &fauna_protocol::folders::MemberSetAccessRequest {
                name: "shared".into(),
                actor_id: hex::encode(writer),
                access: "reader".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let role = db
            .get_folder_member_role(&channel_id, &writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role.access, "reader");
        assert_eq!(role.byte_cap, None, "cap follows the edit");
    }

    /// `set_access` gates: a stranger folds to `not_found` (ST-RES-1); an
    /// invalid access value is `invalid_request`; a same-group set owner who is
    /// NOT the channel claimant is `forbidden`.
    #[tokio::test]
    async fn set_access_core_rejects_stranger_invalid_and_non_claimant() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let attacker = [0xeeu8; 32];
        let raw_group_id = vec![0x7cu8; 20];

        db.create_folder("shared", &owner).await.unwrap();
        share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Stranger: no set of this name → not_found (no oracle).
        let err = set_access_core(
            &db,
            &attacker,
            &fauna_protocol::folders::MemberSetAccessRequest {
                name: "shared".into(),
                actor_id: hex::encode(member),
                access: "writer".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.folders.not_found");

        // Invalid access value.
        let err = set_access_core(
            &db,
            &owner,
            &fauna_protocol::folders::MemberSetAccessRequest {
                name: "shared".into(),
                actor_id: hex::encode(member),
                access: "admin".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.folders.invalid_request");

        // A different owner binds their OWN set to the same group directly (the
        // bypass share_core would refuse) — they own a bound set but are not the
        // channel claimant, so the grant write is forbidden.
        db.create_folder("their-set", &attacker).await.unwrap();
        db.set_folder_mls_group("their-set", &attacker, Some(&raw_group_id))
            .await
            .unwrap();
        let err = set_access_core(
            &db,
            &attacker,
            &fauna_protocol::folders::MemberSetAccessRequest {
                name: "their-set".into(),
                actor_id: hex::encode(member),
                access: "writer".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.folders.forbidden");
    }

    /// D5 share-time grant: a share request carrying `member_actor_id` +
    /// `access: writer` records the role row with the bind — before any join —
    /// and a request without them records nothing (absent row = reader).
    #[tokio::test]
    async fn share_core_records_share_time_writer_grant() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

        db.create_folder("shared", &owner).await.unwrap();
        share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&raw_group_id),
                member_actor_id: Some(hex::encode(member)),
                access: Some("writer".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let role = db
            .get_folder_member_role(&channel_id, &member)
            .await
            .unwrap()
            .expect("share-time grant recorded with the bind");
        assert_eq!(role.access, "writer");
        assert_eq!(role.byte_cap, None, "share wire carries no cap — uncapped");

        // A plain share (no access fields) records nothing.
        let owner2 = [0xa2u8; 32];
        let member2 = [0xb3u8; 32];
        let raw2 = vec![0x7du8; 20];
        let channel2 = fauna_mls::types::ChannelId::from_group_id(&raw2).0;
        db.create_folder("plain", &owner2).await.unwrap();
        share_core(
            &db,
            &owner2,
            &FolderShareRequest {
                name: "plain".into(),
                group_id: hex::encode(&raw2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_folder_member_role(&channel2, &member2)
                .await
                .unwrap(),
            None,
            "no share-time fields → no row (absent = reader)"
        );
    }

    /// The grant dies with the membership: `members.evict` and `leave` both
    /// drop the role row, so a later re-add starts from the fail-safe reader
    /// default instead of a resurrected `writer`.
    #[tokio::test]
    async fn evict_and_leave_drop_the_role_row() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

        db.create_folder("shared", &owner).await.unwrap();
        share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.register_actor_channel(&member, &channel_id)
            .await
            .unwrap();
        db.set_folder_member_access(&channel_id, &member, "writer", None)
            .await
            .unwrap();

        // Owner evicts → roster row AND role row both gone.
        members_evict_core(
            &db,
            &owner,
            &fauna_protocol::folders::MemberEvictRequest {
                name: "shared".into(),
                member: hex::encode(member),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_folder_member_role(&channel_id, &member)
                .await
                .unwrap(),
            None,
            "evict drops the grant with the membership"
        );

        // Re-add + re-grant, then the member leaves voluntarily → same outcome.
        db.register_actor_channel(&member, &channel_id)
            .await
            .unwrap();
        db.set_folder_member_access(&channel_id, &member, "writer", None)
            .await
            .unwrap();
        leave_core(
            &db,
            &member,
            &fauna_protocol::folders::MemberLeaveRequest {
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_folder_member_role(&channel_id, &member)
                .await
                .unwrap(),
            None,
            "leave drops the caller's own grant"
        );
    }

    /// 5d(c): `list` projects a bound set's `mls_group_id` (hex) so the owner's
    /// sync daemon can detect the binding and load its M2 content keys; an
    /// owner-only set lists `None`. `list` is owner-scoped, so only the owner
    /// ever sees the projection.
    #[tokio::test]
    async fn list_core_projects_mls_group_id_for_bound_sets() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let raw_group_id = vec![0x7cu8; 20];

        db.create_folder("private", &owner).await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        share_core(
            &db,
            &owner,
            &FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Owner-scoped (no flag): the daemon contract, unchanged.
        let reply = list_core(&db, &owner, false).await.unwrap();
        let by_name = |n: &str| {
            reply
                .folders
                .iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("{n} missing from list"))
        };
        assert_eq!(
            by_name("shared").mls_group_id,
            Some(hex::encode(&raw_group_id)),
            "a bound set projects its raw group id (hex) for the daemon"
        );
        assert_eq!(
            by_name("private").mls_group_id,
            None,
            "an owner-only set has no binding"
        );
        assert_eq!(
            by_name("shared").role.as_deref(),
            Some("owner"),
            "the caller's own set is tagged role=owner"
        );
        assert_eq!(
            by_name("shared").owner_handle,
            None,
            "the caller's own set carries no owner handle"
        );
        assert_eq!(
            by_name("shared").owner_actor_id,
            None,
            "the caller's own set carries no owner actor id"
        );
    }

    /// B3 member-list-visibility: with `include_shared_with_me`, `list` unions
    /// the caller's owned sets with sets they are a **roster member** of —
    /// each a `role == "member"` row carrying the owner's handle, with the
    /// owner's local paths withheld. Without the flag the enumeration stays
    /// owner-scoped (the daemon contract). An outsider (not on the roster) never
    /// sees the set, and a reserved `__conv/*` roster channel never surfaces.
    #[tokio::test]
    async fn list_core_unions_member_visible_sets_only_when_requested() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        db.create_user_with_handle(&owner, "free", "alice", None)
            .await
            .unwrap();

        // Owner shares "family-photos" (bound) and registers `member` on the roster.
        let channel_id = share_and_add_member(&db, "family-photos", &owner, &member).await;
        // The member also owns a private set of their own.
        db.create_folder("my-notes", &member).await.unwrap();

        // A reserved conversation the member is (also) a roster member of — must
        // NOT surface as a folder.
        let conv_name = format!("__conv/{}", hex::encode([0x11u8; 32]));
        db.create_folder(&conv_name, &owner).await.unwrap();
        db.set_folder_mls_group(&conv_name, &owner, Some(&[0x22u8; 20]))
            .await
            .unwrap();
        let conv_channel = fauna_mls::types::ChannelId::from_group_id(&[0x22u8; 20]).0;
        db.register_actor_channel(&member, &conv_channel)
            .await
            .unwrap();

        // Without the flag: owner-scoped — the member sees only their own set.
        let owner_scoped = list_core(&db, &member, false).await.unwrap();
        assert!(
            owner_scoped
                .folders
                .iter()
                .all(|s| s.name != "family-photos"),
            "owner-scoped list must NOT include a set shared with the caller"
        );

        // With the flag: the member sees their own set + the shared set.
        let reply = list_core(&db, &member, true).await.unwrap();
        let find = |n: &str| reply.folders.iter().find(|s| s.name == n);

        let mine = find("my-notes").expect("the member's own set is listed");
        assert_eq!(mine.role.as_deref(), Some("owner"), "own set is role=owner");

        let shared = find("family-photos").expect("the shared set is member-visible");
        assert_eq!(
            shared.role.as_deref(),
            Some("member"),
            "shared set is role=member"
        );
        assert_eq!(
            shared.owner_handle.as_deref(),
            Some("alice"),
            "the owner handle is resolved nest-side for the 'Shared by' badge"
        );
        assert_eq!(
            shared.owner_actor_id.as_deref(),
            Some(hex::encode(owner).as_str()),
            "the member row carries the owner actor id — the badge's display fallback"
        );
        assert_eq!(
            mine.owner_actor_id, None,
            "the caller's own row carries no owner actor id (no 'Shared by' badge)"
        );
        assert_eq!(
            shared.mls_group_id,
            Some(hex::encode(vec![0x7cu8; 20])),
            "the member row carries the group id so the client can filter to joined"
        );
        assert_eq!(
            shared.include_paths, None,
            "the owner's local paths are withheld from a member"
        );
        assert!(
            reply.folders.iter().all(|s| !s.name.starts_with("__")),
            "a reserved __conv/* roster channel never surfaces as a folder"
        );
        let _ = channel_id;

        // An outsider (not on the roster) never sees the set, flag or not.
        let outsider_view = list_core(&db, &outsider, true).await.unwrap();
        assert!(
            outsider_view
                .folders
                .iter()
                .all(|s| s.name != "family-photos"),
            "a non-member never sees the shared set"
        );
    }

    /// **Path-sealing S6-e: the RETENTION seal REACHES a roster member — the
    /// opposite disposition to its neighbour, and the reason is the audience.**
    ///
    /// This is the sibling of `the_sealed_selective_sync_pair_is_withheld_from_a_
    /// roster_member` below, and the pair is deliberately adjacent: the two fields
    /// sit lines apart on one struct and pull opposite ways. `include_paths` is
    /// withheld from a member because the nest already withholds its plaintext;
    /// `retention_policy`'s plaintext is shipped to a member **unmodified**, so
    /// withholding its seal — or sealing it under the owner-only root — would blank
    /// a member's retention display at the flip. That is a NARROWING, and it fails
    /// in the flattering direction: it reads as hardening, which is exactly why it
    /// needs a test that reddens on it.
    ///
    /// The negative control is the selective-sync pair asserted in the same test:
    /// a change that shipped *every* seal to members would pass a lone positive
    /// assertion and look like a fix.
    #[tokio::test]
    async fn the_sealed_retention_policy_reaches_a_roster_member_unlike_the_path_pair() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        db.create_user_with_handle(&owner, "free", "alice", None)
            .await
            .unwrap();
        let _ = share_and_add_member(&db, "family-photos", &owner, &member).await;

        db.update_folder_for_user(
            "family-photos",
            &owner,
            crate::db::FolderUpdate {
                retention_policy: Some(Some(r#"{"max_snapshots":7}"#)),
                retention_policy_sealed: Some(Some(&[0x4au8, 0x4b][..])),
                // The contrast control: stamped so this test reddens on a change
                // that widens the *path* pair, not only on one that narrows
                // retention. Both dispositions are asserted from one fixture.
                include_paths: Some(Some(r#"["/home/alice/photos"]"#)),
                include_paths_sealed: Some(Some(&[0x1au8, 0x1b][..])),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // The member arm — the positive pin, and the one a "tightening" breaks.
        let member_view = list_core(&db, &member, true).await.unwrap();
        let shared = member_view
            .folders
            .iter()
            .find(|s| s.name == "family-photos")
            .expect("the shared set is member-visible");
        // Prove the subject reached the code under test before grading it — a
        // filtered list can drop the row and leave every assertion below vacuous
        // (the S6-c lesson-4 datum the reviewer ratified into the method ledger).
        assert_eq!(shared.role.as_deref(), Some("member"));
        assert_eq!(
            shared
                .retention_policy_sealed
                .as_deref()
                .map(|b| b.to_vec()),
            Some(vec![0x4au8, 0x4b]),
            "a roster member IS the retention seal's audience — they receive the \
             plaintext today, so withholding the seal would blank them at the flip"
        );
        assert!(
            shared.retention_policy.is_some(),
            "the plaintext still rests during expand — the seal rides beside it, \
             it does not replace it yet"
        );
        assert!(
            shared.name_hash.is_some(),
            "the seal's SALT must ride with it, or the row is unopenable once the \
             plaintext name scrubs (the S2b/S4 hole)"
        );
        assert_eq!(
            shared.include_paths_sealed, None,
            "the selective-sync seal stays withheld — this test must redden on a \
             change that widens the path pair, not only on one that narrows retention"
        );

        // The owner arm — the twin, so a fix that withholds from *everyone* cannot
        // pass wearing this test's colours.
        let own_view = list_core(&db, &owner, false).await.unwrap();
        let own = own_view
            .folders
            .iter()
            .find(|s| s.name == "family-photos")
            .expect("the owner sees their own set");
        assert_eq!(
            own.retention_policy_sealed.as_deref().map(|b| b.to_vec()),
            Some(vec![0x4au8, 0x4b])
        );
    }

    /// **Path-sealing S6-e: the retention pair moves together — a keyless save
    /// CLEARS the seal rather than retaining it.**
    ///
    /// The failure this forbids is silent and therefore the worst kind: a retained
    /// seal opens to the policy it *replaced*, so post-flip the user is shown a
    /// retention rule that is no longer theirs, with nothing erroring. Same rule
    /// and same reasoning as `label_sealed` (S6-b) and the selective-sync pair
    /// (S6-c), now applied consistently to the third field.
    #[tokio::test]
    async fn a_keyless_retention_save_clears_the_stale_seal() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa3u8; 32];
        db.create_user_with_handle(&owner, "free", "carol", None)
            .await
            .unwrap();
        db.create_folder("keyed-set", &owner).await.unwrap();

        // A keyed writer stamps the pair.
        db.update_folder_for_user(
            "keyed-set",
            &owner,
            crate::db::FolderUpdate {
                retention_policy: Some(Some(r#"{"max_snapshots":7}"#)),
                retention_policy_sealed: Some(Some(&[0x4au8][..])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fs = db
            .get_folder_for_actor("keyed-set", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fs.retention_policy_sealed.as_deref(),
            Some(&[0x4au8][..]),
            "precondition: the seal is stamped, so the clear below is observable"
        );

        // A keyless writer then saves a DIFFERENT policy carrying no seal.
        db.update_folder_for_user(
            "keyed-set",
            &owner,
            crate::db::FolderUpdate {
                retention_policy: Some(Some(r#"{"max_snapshots":30}"#)),
                retention_policy_sealed: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fs = db
            .get_folder_for_actor("keyed-set", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fs.retention_policy.as_deref(),
            Some(r#"{"max_snapshots":30}"#)
        );
        assert_eq!(
            fs.retention_policy_sealed, None,
            "the stale seal is CLEARED, not COALESCE-retained — a retained one \
             would open to the policy it replaced and show it silently"
        );
    }

    /// **Path-sealing S6-c: the selective-sync SEAL is
    /// withheld from a roster member, exactly as its plaintext already is.**
    ///
    /// The trap this pins is that `member_summary` ships `name_sealed`/`name_hash`
    /// to a member three lines away, so "apply the `FolderName` precedent
    /// directly" reads as ship-it — and shipping it would turn a sealing slice
    /// into a disclosure widening. `include_paths`/`exclude_paths` is
    /// **owner-only**: the owner's absolute local filesystem layout, which a
    /// member neither syncs nor manages.
    ///
    /// Both halves are asserted, because withholding one is not withholding the
    /// pair — and the owner arm is asserted in the same test, because a fix that
    /// withholds from *everyone* is a regression wearing this test's colours.
    #[tokio::test]
    async fn the_sealed_selective_sync_pair_is_withheld_from_a_roster_member() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        db.create_user_with_handle(&owner, "free", "alice", None)
            .await
            .unwrap();
        let _ = share_and_add_member(&db, "family-photos", &owner, &member).await;

        // Stamp both seals as the owner's keyed save would. Opaque blobs: this
        // projection never opens them, so distinct sentinels are enough to tell
        // "shipped" from "withheld" and from "crossed over".
        db.update_folder_for_user(
            "family-photos",
            &owner,
            crate::db::FolderUpdate {
                include_paths: Some(Some(r#"["/home/alice/photos"]"#)),
                exclude_paths: Some(Some(r#"["/home/alice/photos/raw"]"#)),
                include_paths_sealed: Some(Some(&[0x1au8, 0x1b][..])),
                exclude_paths_sealed: Some(Some(&[0x2au8, 0x2b][..])),
                // Stamped alongside on purpose: it is the CONTROL for the member
                // arm below. Without it, a change that stopped projecting every
                // seal to a member would pass the two withheld-assertions.
                name_sealed: Some(&[0x3au8, 0x3b][..]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // The member arm — the negative pin. The stamped name seal rests the
        // set's name NULL (schema 114), so both views find it by its hash.
        let photos_hash = fauna_core::path_crypto::set_name_hash("family-photos");
        let member_view = list_core(&db, &member, true).await.unwrap();
        let shared = member_view
            .folders
            .iter()
            .find(|s| s.name_hash.as_deref().map(|h| &h[..]) == Some(&photos_hash[..]))
            .expect("the shared set is member-visible");
        assert_eq!(shared.role.as_deref(), Some("member"));
        assert_eq!(
            shared.include_paths_sealed, None,
            "a member is not the audience for the owner's filesystem layout"
        );
        assert_eq!(
            shared.exclude_paths_sealed, None,
            "the exclude half is withheld too — withholding one is not withholding the pair"
        );
        assert!(
            shared.name_sealed.is_some(),
            "the SET NAME seal still ships to a member — this test must fail when the \
             selective-sync pair leaks, not when the name pair stops flowing"
        );

        // The owner arm — the positive twin. Withholding from everyone is a
        // regression, not a fix.
        let owner_view = list_core(&db, &owner, false).await.unwrap();
        let own = owner_view
            .folders
            .iter()
            .find(|s| s.name_hash.as_deref().map(|h| &h[..]) == Some(&photos_hash[..]))
            .expect("the owner lists their own set");
        assert_eq!(own.role.as_deref(), Some("owner"));
        assert_eq!(
            own.include_paths_sealed.as_deref().map(|b| b.to_vec()),
            Some(vec![0x1au8, 0x1b]),
            "the owner receives the seal they minted"
        );
        assert_eq!(
            own.exclude_paths_sealed.as_deref().map(|b| b.to_vec()),
            Some(vec![0x2au8, 0x2b]),
        );
    }

    /// **The pair moves together (path-sealing S6-c, the S6-b `label_sealed`
    /// rule).** A keyless writer saving new paths must CLEAR the seal it cannot
    /// re-mint: keeping it would leave a row whose plaintext says one thing and
    /// whose seal opens to the list it replaced, and post-flip the user would be
    /// shown the *stale* filesystem layout with nothing failing.
    #[tokio::test]
    async fn a_keyless_paths_save_clears_the_seal_it_cannot_re_mint() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        db.create_folder("docs", &owner).await.unwrap();
        db.update_folder_for_user(
            "docs",
            &owner,
            crate::db::FolderUpdate {
                include_paths: Some(Some(r#"["/old"]"#)),
                include_paths_sealed: Some(Some(&[0x1au8, 0x1b][..])),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // A keyless app saves new paths: plaintext rides, no seal does.
        db.update_folder_for_user(
            "docs",
            &owner,
            crate::db::FolderUpdate {
                include_paths: Some(Some(r#"["/new"]"#)),
                include_paths_sealed: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let fs = db
            .get_folder_for_actor("docs", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.include_paths.as_deref(), Some(r#"["/new"]"#));
        assert_eq!(
            fs.include_paths_sealed, None,
            "the stale seal is dropped for S8 to re-stamp, never left to open to the old list"
        );

        // A seal-only write (the S8 backfill shape) stamps without touching the
        // plaintext — the other half of the same rule.
        db.update_folder_for_user(
            "docs",
            &owner,
            crate::db::FolderUpdate {
                include_paths_sealed: Some(Some(&[0x3au8][..])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fs = db
            .get_folder_for_actor("docs", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.include_paths.as_deref(), Some(r#"["/new"]"#));
        assert_eq!(fs.include_paths_sealed.as_deref(), Some(&[0x3au8][..]));
    }

    /// FS-BIND-3(a): a non-owner caller cannot bind another user's set. The
    /// owner-scoped UPDATE changes 0 rows → `not_found` (existence oracle closed —
    /// indistinguishable from "absent"), and the set stays unbound.
    #[tokio::test]
    async fn share_core_non_owner_cannot_bind() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let attacker = [0xeeu8; 32];
        let raw_group_id = vec![0x7cu8; 32];

        db.create_folder("shared", &owner).await.unwrap();
        let req = FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&raw_group_id),
            ..Default::default()
        };
        let err = share_core(&db, &attacker, &req).await.unwrap_err();
        assert!(
            err.code.contains("not_found"),
            "non-owner bind closes the existence oracle (not_found), got {}",
            err.code
        );
        assert_eq!(
            db.get_folder_for_actor("shared", &owner)
                .await
                .unwrap()
                .unwrap()
                .mls_group_id,
            None,
            "the non-owner attempt left the set unbound"
        );
    }

    use fauna_protocol::folders::{ContentKeyGetRequest, ContentKeyPutRequest};

    /// Bind `name` (owned by `owner`) to a group and register `member` on the
    /// derived roster — the post-`share` + `welcome.deliver` state the content-key
    /// kinds gate against. Returns the derived 32-byte ChannelId.
    async fn share_and_add_member(
        db: &crate::db::CacheDb,
        name: &str,
        owner: &[u8; 32],
        member: &[u8; 32],
    ) -> [u8; 32] {
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        db.create_folder(name, owner).await.unwrap();
        share_core(
            db,
            owner,
            &FolderShareRequest {
                name: name.into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // welcome.deliver registers the member on the roster.
        db.register_actor_channel(member, &channel_id)
            .await
            .unwrap();
        channel_id
    }

    /// Piece 2 happy path: the owner publishes the envelope; both the owner and a
    /// roster member fetch the identical opaque bytes; an outsider gets
    /// `not_found` (the gate is `folder_authz`, the same as the snapshot reads).
    #[tokio::test]
    async fn content_key_put_then_get_for_owner_member_and_outsider() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        let channel_id = share_and_add_member(&db, "shared", &owner, &member).await;

        let sealed_hex = "ab".repeat(40);
        let put = content_key_put_core(
            &db,
            &owner,
            &ContentKeyPutRequest {
                name: "shared".into(),
                epoch: 3,
                sealed: sealed_hex.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(put.ok);
        assert_eq!(put.channel_id, hex::encode(channel_id));

        // Owner reads it back.
        let got = content_key_get_core(
            &db,
            &owner,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(got.epoch, 3);
        assert_eq!(got.sealed, sealed_hex);

        // Roster member reads the identical envelope.
        let member_got = content_key_get_core(
            &db,
            &member,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(member_got.sealed, sealed_hex);

        // Outsider: not_found (the resolver folds non-member → None; oracle closed).
        let err = content_key_get_core(
            &db,
            &outsider,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);
    }

    /// The actor-roster member-list read (`fauna.folders.members.list_actors`) —
    /// the owner-side "Shared with" list. The owner and every roster member read
    /// the full roster (the owner marked `role == "owner"`, everyone else
    /// `"member"`, each with its nest-resolved handle); an outsider gets
    /// `not_found` (oracle closed) and an owner-only (unshared) set gets
    /// `not_shared`.
    #[tokio::test]
    async fn actor_members_list_projects_roster_with_handles_and_roles() {
        use fauna_protocol::folders::ActorMembersListRequest;
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        share_and_add_member(&db, "shared", &owner, &member).await;

        // Resolvable handles for owner + member.
        db.create_user(&owner, "free", "test").await.unwrap();
        db.set_handle(&owner, "alice").await.unwrap();
        db.create_user(&member, "free", "test").await.unwrap();
        db.set_handle(&member, "bob").await.unwrap();

        let req = ActorMembersListRequest {
            name: "shared".into(),
            ..Default::default()
        };

        // Owner sees the full roster: itself (owner) + the shared-with member.
        let reply = actor_members_list_core(&db, &owner, &req).await.unwrap();
        assert_eq!(reply.members.len(), 2);
        let owner_row = reply
            .members
            .iter()
            .find(|m| m.actor_id == hex::encode(owner))
            .expect("owner in roster");
        assert_eq!(owner_row.role, "owner");
        assert_eq!(owner_row.handle, "alice");
        let member_row = reply
            .members
            .iter()
            .find(|m| m.actor_id == hex::encode(member))
            .expect("member in roster");
        assert_eq!(member_row.role, "member");
        assert_eq!(member_row.handle, "bob");

        // A roster member also reads the list (the read is owner/member-gated).
        let member_view = actor_members_list_core(&db, &member, &req).await.unwrap();
        assert_eq!(member_view.members.len(), 2);

        // Outsider: not_found (the resolver folds non-member → None; oracle closed).
        let err = actor_members_list_core(&db, &outsider, &req)
            .await
            .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);

        // An owner-only (unshared) set has no actor roster → not_shared.
        db.create_folder("private", &owner).await.unwrap();
        let err = actor_members_list_core(
            &db,
            &owner,
            &ActorMembersListRequest {
                name: "private".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_shared"), "got {}", err.code);
    }

    /// Re-publish upserts (one envelope per group, the latest wins).
    #[tokio::test]
    async fn content_key_put_upserts_latest() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        share_and_add_member(&db, "shared", &owner, &member).await;

        for (epoch, sealed) in [(1, "11".repeat(40)), (2, "22".repeat(40))] {
            content_key_put_core(
                &db,
                &owner,
                &ContentKeyPutRequest {
                    name: "shared".into(),
                    epoch,
                    sealed,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let got = content_key_get_core(
            &db,
            &member,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(got.epoch, 2, "the latest publish wins");
        assert_eq!(got.sealed, "22".repeat(40));
    }

    /// A non-owner cannot publish — the owner-scoped lookup folds to `not_found`.
    #[tokio::test]
    async fn content_key_put_non_owner_cannot_publish() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        share_and_add_member(&db, "shared", &owner, &member).await;

        // A roster *member* is read-only — they cannot put.
        let err = content_key_put_core(
            &db,
            &member,
            &ContentKeyPutRequest {
                name: "shared".into(),
                epoch: 1,
                sealed: "ab".repeat(40),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);
    }

    /// Publishing to an owner-only (unshared) set is `not_shared`, not a silent put.
    #[tokio::test]
    async fn content_key_put_unshared_set_rejected() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        db.create_folder("private", &owner).await.unwrap();
        let err = content_key_put_core(
            &db,
            &owner,
            &ContentKeyPutRequest {
                name: "private".into(),
                epoch: 1,
                sealed: "ab".repeat(40),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_shared"), "got {}", err.code);
    }

    /// A readable member who fetches before any publish gets `not_published`
    /// (distinct from `not_found` — wait + retry, access is fine).
    #[tokio::test]
    async fn content_key_get_before_publish_is_not_published() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        share_and_add_member(&db, "shared", &owner, &member).await;
        let err = content_key_get_core(
            &db,
            &member,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_published"), "got {}", err.code);
    }

    /// An empty / non-hex group id is `invalid_request`, not a silent bind.
    #[tokio::test]
    async fn share_core_rejects_empty_group_id() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        db.create_folder("shared", &owner).await.unwrap();
        let req = FolderShareRequest {
            name: "shared".into(),
            group_id: String::new(),
            ..Default::default()
        };
        let err = share_core(&db, &owner, &req).await.unwrap_err();
        assert!(err.code.contains("invalid_request"), "got {}", err.code);
    }

    use fauna_protocol::folders::MemberEvictRequest;

    fn evict_req(name: &str, member: &[u8; 32]) -> MemberEvictRequest {
        MemberEvictRequest {
            name: name.into(),
            member: hex::encode(member),
            ..Default::default()
        }
    }

    /// F1/OBS-1 happy path: the owner evicts a rostered member, the scoped DELETE
    /// drops exactly that `(actor, channel)` row, and the evicted member's
    /// metadata reads now fold to `not_found` — while the owner stays rostered.
    #[tokio::test]
    async fn members_evict_drops_member_from_roster_and_closes_metadata() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let channel_id = share_and_add_member(&db, "shared", &owner, &member).await;
        assert!(db.is_actor_in_channel(&member, &channel_id).await.unwrap());

        let reply = members_evict_core(&db, &owner, &evict_req("shared", &member))
            .await
            .unwrap();
        assert!(reply.ok && reply.evicted, "a rostered member is evicted");
        assert_eq!(reply.channel_id, hex::encode(channel_id));

        // The roster row is gone — the metadata-confidentiality fix.
        assert!(
            !db.is_actor_in_channel(&member, &channel_id).await.unwrap(),
            "evicted member is off the roster"
        );
        // The owner's own row is untouched (scoped DELETE, never blanket).
        assert!(
            db.is_actor_in_channel(&owner, &channel_id).await.unwrap(),
            "the owner stays rostered"
        );
        // The evicted member's discovery-metadata read now folds to not_found.
        let err = content_key_get_core(
            &db,
            &member,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);
    }

    /// Idempotent: evicting an already-absent member is `ok: true, evicted: false`
    /// (the crash-resumed eviction path — no error).
    #[tokio::test]
    async fn members_evict_is_idempotent() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        share_and_add_member(&db, "shared", &owner, &member).await;

        assert!(
            members_evict_core(&db, &owner, &evict_req("shared", &member))
                .await
                .unwrap()
                .evicted
        );
        let again = members_evict_core(&db, &owner, &evict_req("shared", &member))
            .await
            .unwrap();
        assert!(again.ok && !again.evicted, "re-evict is a no-op success");
    }

    /// A non-owner cannot evict — the owner-scoped lookup folds to `not_found`,
    /// and the target member stays rostered.
    #[tokio::test]
    async fn members_evict_non_owner_cannot_evict() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let attacker = [0xeeu8; 32];
        let channel_id = share_and_add_member(&db, "shared", &owner, &member).await;

        let err = members_evict_core(&db, &attacker, &evict_req("shared", &member))
            .await
            .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);
        assert!(
            db.is_actor_in_channel(&member, &channel_id).await.unwrap(),
            "a non-owner eviction left the roster intact"
        );
    }

    /// Evicting from an owner-only (unshared) set is `not_shared` — there is no
    /// roster.
    #[tokio::test]
    async fn members_evict_unshared_set_rejected() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        db.create_folder("private", &owner).await.unwrap();
        let err = members_evict_core(&db, &owner, &evict_req("private", &[0xb2u8; 32]))
            .await
            .unwrap_err();
        assert!(err.code.contains("not_shared"), "got {}", err.code);
    }

    /// A non-hex / wrong-length member is `invalid_request`.
    #[tokio::test]
    async fn members_evict_rejects_bad_member_hex() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        share_and_add_member(&db, "shared", &owner, &[0xb2u8; 32]).await;
        let err = members_evict_core(
            &db,
            &owner,
            &MemberEvictRequest {
                name: "shared".into(),
                member: "nothex".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("invalid_request"), "got {}", err.code);
    }

    // ── fauna.folders.leave: member self-remove (recipient side) ───────────────

    use fauna_protocol::folders::MemberLeaveRequest;

    fn leave_req(raw_group_id: &[u8]) -> MemberLeaveRequest {
        MemberLeaveRequest {
            group_id: hex::encode(raw_group_id),
            ..Default::default()
        }
    }

    /// Happy path: a rostered member leaves *by group id*, the scoped DELETE drops
    /// exactly their own `(actor, channel)` row (the owner stays rostered), and the
    /// leaver's metadata read now folds to `not_found` — they stop receiving
    /// content-key rotations. Self-scoped: no owner secret, no claimant check.
    #[tokio::test]
    async fn leave_drops_caller_from_roster_and_closes_metadata() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = share_and_add_member(&db, "shared", &owner, &member).await;
        assert!(db.is_actor_in_channel(&member, &channel_id).await.unwrap());

        // The *member* (not the owner) drives the leave, addressed by group id.
        let reply = leave_core(&db, &member, &leave_req(&raw_group_id))
            .await
            .unwrap();
        assert!(reply.ok && reply.left, "a rostered member leaves");
        assert_eq!(reply.channel_id, hex::encode(channel_id));

        // Only the caller's row is dropped (scoped DELETE, never blanket).
        assert!(
            !db.is_actor_in_channel(&member, &channel_id).await.unwrap(),
            "the leaver is off the roster"
        );
        assert!(
            db.is_actor_in_channel(&owner, &channel_id).await.unwrap(),
            "the owner stays rostered"
        );
        // Off the roster, the leaver's discovery-metadata read folds to not_found —
        // they no longer see content-key rotations.
        let err = content_key_get_core(
            &db,
            &member,
            &ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("not_found"), "got {}", err.code);
    }

    /// Idempotent: a caller not on the roster (already left, or never joined) is
    /// `ok: true, left: false` — a harmless self-scoped no-op, never an error.
    #[tokio::test]
    async fn leave_of_unjoined_channel_is_idempotent_noop() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 20];
        share_and_add_member(&db, "shared", &owner, &member).await;

        // First leave removes the member.
        assert!(
            leave_core(&db, &member, &leave_req(&raw_group_id))
                .await
                .unwrap()
                .left
        );
        // Second leave is a no-op success.
        let again = leave_core(&db, &member, &leave_req(&raw_group_id))
            .await
            .unwrap();
        assert!(again.ok && !again.left, "re-leave is a no-op success");

        // A stranger who never joined leaves harmlessly (only ever drops their own
        // row — self-scoped) and cannot touch the owner's roster.
        let stranger = [0xeeu8; 32];
        let none = leave_core(&db, &stranger, &leave_req(&raw_group_id))
            .await
            .unwrap();
        assert!(none.ok && !none.left, "a non-member leave is a no-op");
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        assert!(
            db.is_actor_in_channel(&owner, &channel_id).await.unwrap(),
            "a stranger's leave left the owner rostered"
        );
    }

    /// A non-hex / empty group id is `invalid_request`.
    #[tokio::test]
    async fn leave_rejects_bad_group_hex() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let member = [0xb2u8; 32];
        for bad in ["nothex", ""] {
            let err = leave_core(
                &db,
                &member,
                &MemberLeaveRequest {
                    group_id: bad.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
            assert!(err.code.contains("invalid_request"), "got {}", err.code);
        }
    }

    // ── 5d-SEC: first-binder-wins namespace claim ────────────────────────────────
    //
    // The
    // `group_id -> ChannelId` namespace is owned by its first folder binder; a
    // second owner, a removed member's `share`-rebind, and a non-claimant publisher
    // are all rejected, while the legit conversation-reuse case (an existing roster
    // member binding a folder to the same group) still works.

    /// primary defense: a second owner cannot bind their own set to a
    /// group already claimed by the first binder, and the rejected attempt does not
    /// register them on the victim group's roster.
    #[tokio::test]
    async fn share_core_second_owner_cannot_claim_bound_group() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let a = [0xa1u8; 32];
        let b = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 24];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        let share_req = |name: &str| FolderShareRequest {
            name: name.into(),
            group_id: hex::encode(&raw_group_id),
            ..Default::default()
        };

        // A binds set "x" to group G first → A claims the channel.
        db.create_folder("x", &a).await.unwrap();
        share_core(&db, &a, &share_req("x")).await.unwrap();
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            Some(a)
        );

        // B owns "y" and tries to bind it to the same group → already_claimed.
        db.create_folder("y", &b).await.unwrap();
        let err = share_core(&db, &b, &share_req("y")).await.unwrap_err();
        assert!(err.code.contains("already_claimed"), "got {}", err.code);
        assert!(
            !db.is_actor_in_channel(&b, &channel_id).await.unwrap(),
            "the rejected second binder is NOT on the victim group's roster"
        );
        // B's set "y" stays unbound (the claim gate runs before the bind).
        assert_eq!(
            db.get_folder_for_actor("y", &b)
                .await
                .unwrap()
                .unwrap()
                .mls_group_id,
            None
        );
    }

    /// The one-time `members.evict` is no longer defeated by a
    /// `share`-rebind — a removed member cannot re-run `share` to re-add themselves
    /// to the roster.
    #[tokio::test]
    async fn members_evict_then_share_rebind_is_blocked() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let removed = [0xb2u8; 32];
        // A shares "x" to group G (claims it) and B joins via welcome.deliver.
        let channel_id = share_and_add_member(&db, "x", &owner, &removed).await;
        let raw_group_id = vec![0x7cu8; 20]; // the id `share_and_add_member` uses
        assert_eq!(
            channel_id,
            fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0
        );

        // A evicts B (F1 metadata eviction).
        members_evict_core(&db, &owner, &evict_req("x", &removed))
            .await
            .unwrap();
        assert!(!db.is_actor_in_channel(&removed, &channel_id).await.unwrap());

        // B tries to re-add via a share-rebind of their own set "y" to group G.
        db.create_folder("y", &removed).await.unwrap();
        let err = share_core(
            &db,
            &removed,
            &FolderShareRequest {
                name: "y".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("already_claimed"), "got {}", err.code);
        assert!(
            !db.is_actor_in_channel(&removed, &channel_id).await.unwrap(),
            "the removed member stays off the roster — F1 eviction holds"
        );
    }

    /// on the put surface (direct): even a set that is somehow bound to a
    /// group it does not own cannot publish to the envelope slot — the publisher
    /// must be the channel's claimant, not merely the owner of *some* bound set.
    #[tokio::test]
    async fn content_key_put_non_claimant_forbidden() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let a = [0xa1u8; 32];
        let b = [0xb2u8; 32];
        let raw_group_id = vec![0x7cu8; 24];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;

        // A claims the channel via share.
        db.create_folder("x", &a).await.unwrap();
        share_core(
            &db,
            &a,
            &FolderShareRequest {
                name: "x".into(),
                group_id: hex::encode(&raw_group_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // B's set "y" is force-bound to the same group (simulating a pre-guard or
        // out-of-band bind) — B owns "y" but is NOT the channel claimant.
        db.create_folder("y", &b).await.unwrap();
        db.set_folder_mls_group("y", &b, Some(&raw_group_id))
            .await
            .unwrap();

        let err = content_key_put_core(
            &db,
            &b,
            &ContentKeyPutRequest {
                name: "y".into(),
                epoch: 1,
                sealed: "ab".repeat(40),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.code.contains("forbidden"), "got {}", err.code);
        // A's claim still holds; the victim slot was never written by B.
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            Some(a)
        );
    }

    /// A channel pre-populated by a conversation (members on the roster via
    /// `welcome.deliver`) is bindable by **nobody** — the outsider who merely
    /// knows the raw group id, and the conversation's own member alike.
    ///
    /// The member arm formerly asserted `Allowed` ("legit conv-reuse"). It is now
    /// refused because the resulting claim is permanent and unreleasable, and a
    /// held claim froze the group under the claimant-only commit gate of the
    /// time (since re-ratified to roster-membership admission, 2026-08-24 —
    /// `federation.md` § Cross-nest shared folders + channel append); the
    /// refusal stands regardless, on the claim's permanence and the
    /// claim-read gate's owner suppression. See
    /// `db::channels::claim_folder_channel` for the full reasoning and why
    /// refusing costs no shipped flow.
    #[tokio::test]
    async fn share_core_refuses_binding_to_a_populated_conversation_channel() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let conv_member = [0xa1u8; 32];
        let outsider = [0xeeu8; 32];
        let raw_group_id = vec![0x5au8; 16];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        let share_req = |name: &str| FolderShareRequest {
            name: name.into(),
            group_id: hex::encode(&raw_group_id),
            ..Default::default()
        };

        // A conversation already registered `conv_member` on this channel.
        db.register_actor_channel(&conv_member, &channel_id)
            .await
            .unwrap();

        // An outsider owns "y" and tries to bind it to the conv group → denied,
        // and is NOT injected onto the conversation roster.
        db.create_folder("y", &outsider).await.unwrap();
        let err = share_core(&db, &outsider, &share_req("y"))
            .await
            .unwrap_err();
        assert!(err.code.contains("already_claimed"), "got {}", err.code);
        assert!(
            !db.is_actor_in_channel(&outsider, &channel_id)
                .await
                .unwrap(),
            "an outsider cannot self-inject onto a conversation's roster"
        );

        // …and so is the conversation's own member. Binding here would hand them a
        // permanent claim on their own group's channel, after which the group's
        // owner could no longer post a membership Commit and no client could
        // release it.
        db.create_folder("x", &conv_member).await.unwrap();
        let err = share_core(&db, &conv_member, &share_req("x"))
            .await
            .unwrap_err();
        assert!(err.code.contains("already_claimed"), "got {}", err.code);
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            None,
            "the conversation's channel stays unclaimed, so its members keep \
             posting membership commits"
        );
    }

    /// The relaying nest's half of the cross-nest owner label's trust rule
    /// (`federation.md` § Cross-nest shared folders + channel append → *The
    /// cross-nest owner label*): the read reply forwards the home nest's pair
    /// only on a WARM binding of its domain to the peer's verified identity; a
    /// mismatch forwards nothing; a cold domain forwards nothing this poll and
    /// spawns the binding (never delaying the reply). Loopback-only fixtures —
    /// the cold domain is a refusing `127.0.0.1:1`, no outbound DNS.
    #[tokio::test]
    async fn the_content_key_relay_forwards_the_owner_label_only_on_a_warm_binding() {
        let state = std::sync::Arc::new(AppState::for_test(std::sync::Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let home = [0x11_u8; 32];
        let other = [0x22_u8; 32];
        let peer_url = "https://home.example:8443";

        // Warm, bound to the peer: forwarded whole.
        state
            .federation_pool
            .seed_bindings_for_test(peer_url, home, Some(("home.example", home)))
            .await;
        assert_eq!(
            super::relayed_owner_label(&state, peer_url, Some("alice"), Some("home.example")).await,
            (Some("alice".into()), Some("home.example".into()))
        );
        // A malformed handle or a half pair: nothing.
        assert_eq!(
            super::relayed_owner_label(&state, peer_url, Some("al ice"), Some("home.example"))
                .await,
            (None, None)
        );
        assert_eq!(
            super::relayed_owner_label(&state, peer_url, Some("alice"), None).await,
            (None, None)
        );
        // Warm, bound to ANOTHER nest: the spoof forwards nothing.
        state
            .federation_pool
            .seed_bindings_for_test(peer_url, home, Some(("elsewhere.example", other)))
            .await;
        assert_eq!(
            super::relayed_owner_label(&state, peer_url, Some("alice"), Some("elsewhere.example"))
                .await,
            (None, None)
        );
        // Cold: nothing this poll, and the binding is spawned off the reply.
        let attempts = state.federation_pool.domain_resolve_attempt_count();
        assert_eq!(
            super::relayed_owner_label(&state, peer_url, Some("alice"), Some("127.0.0.1:1")).await,
            (None, None)
        );
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while state.federation_pool.domain_resolve_attempt_count() == attempts {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a cold domain spawns its binding for the next poll");
    }
}
