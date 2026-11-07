//! Device-sync control-plane WS-RPC handlers (bearer connection) — part of
//! the WS-RPC-everywhere migration (tracked internally). A
//! behavior-preserving transport migration of the bearer-authed device-sync routes (`sync_routes`):
//!
//! - `fauna.sync.register`
//! - `fauna.sync.changes.{list,record}`
//! - `fauna.sync.status`
//! - `fauna.sync.files`
//! - `fauna.sync.backup_status`
//! - `fauna.sync.devices.{list,delete}`
//!
//! Each handler reuses the same `CacheDb` method the HTTP twin calls and scopes
//! on the connection `actor_id` (the twins did `bearer.0.0`). Gate `User |
//! Admin` (the twins were plain `BearerAuth`; an admin owns sync devices too) —
//! enforced in `bridge_method_allowlist::is_permitted`. Wire types: `libs/
//! fauna-protocol/src/sync.rs`. The sibling `fauna.sync.conflicts.*` kinds live
//! in `folder_handlers.rs`; the bulk byte routes stay HTTP residue.
//!
//! One latent-authorization fix lands here: the `sync_status` twin took
//! `_bearer` and **never checked folder ownership** (every other device-sync
//! route did), so any authed actor could read any folder's device/online
//! status. The `fauna.sync.status` kind adds the ownership check the siblings
//! have (the B14 / B12b "fix the latent twin bug in the WS-RPC kind" precedent).

use std::time::Duration;

use fauna_protocol::sync::{
    BackupStatusEntry, DeviceFolderRole, DeviceGrantRegisterReply, DeviceGrantRegisterRequest,
    DeviceGrantRevokeReply, DeviceGrantRevokeRequest, SyncBackupStatusReply,
    SyncBackupStatusRequest, SyncChange, SyncChangeRecordReply, SyncChangeRecordRequest,
    SyncChangesListReply, SyncChangesListRequest, SyncChangesSupersedeReply,
    SyncChangesSupersedeRequest, SyncDevice, SyncDeviceDeleteReply, SyncDeviceDeleteRequest,
    SyncDeviceP2pParticipationSetReply, SyncDeviceP2pParticipationSetRequest,
    SyncDeviceRenameReply, SyncDeviceRenameRequest, SyncDevicesListReply, SyncDevicesListRequest,
    SyncFile, SyncFilesReply, SyncFilesRequest, SyncRegisterReply, SyncRegisterRequest,
    SyncStatusReply, SyncStatusRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use crate::db::sync_storage::{DeviceQuotaError, StorageQuotaError};
use crate::routes::{AppState, SetGate, parse_32_bytes as parse_device_id};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use fauna_protocol::folders::SetAddressed;

/// Error namespace for the `fauna.sync.*` kinds.
const SYNC: &str = "sync";

// ── error / encode helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn coded(code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns(SYNC, code, detail)
}

/// [`coded`] for the account-data plane, whose kinds live under
/// `fauna.account.state.*` rather than `fauna.sync.*`. A separate helper because
/// `coded` hard-codes the [`SYNC`] namespace, and an error whose code namespace
/// disagrees with its kind's is the kind of thing a client's error routing gets
/// wrong once and never revisits.
fn state_coded(code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns("account.state", code, detail)
}

/// Map a `CacheDb` / storage error onto the twin's `internal` status. We format
/// the **full** anyhow chain (`{e:#}`) rather than `to_string()` (top context
/// only) so any wrapped marker survives into logs/details — the events B12b /
/// folders B14 `.context()` error-chain gotcha (no UNIQUE-conflict path here:
/// `register` is `INSERT OR REPLACE`, `changes.record` a plain `INSERT`).
fn internal(e: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(SYNC, e)
}

/// Map a storage-quota outcome onto the wire: an `Exceeded` rejection becomes the
/// typed, client-renderable `fauna.sync.storage_quota_exceeded` error (the
/// sync-domain twin of the mail path's `over_quota("storage")`); a DB error folds
/// into `internal`.
/// Map a device-quota outcome onto the wire: a `LimitExceeded` rejection
/// becomes the typed, client-renderable `fauna.sync.device_limit_exceeded`
/// error (the device-plane twin of [`quota_err`]'s `storage_quota_exceeded`);
/// a DB error folds into `internal`.
fn device_quota_err(e: DeviceQuotaError) -> RpcError {
    match e {
        e @ DeviceQuotaError::LimitExceeded { .. } => coded("device_limit_exceeded", e),
        DeviceQuotaError::Db(inner) => internal(inner),
    }
}

/// A refusal that is neither malformed input nor an internal fault — the
/// tier lookup answering for an actor with no `users` row.
fn forbidden(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::forbidden_ns(SYNC, reason)
}

pub(crate) fn quota_err(e: StorageQuotaError) -> RpcError {
    match e {
        e @ StorageQuotaError::Exceeded { .. } => coded("storage_quota_exceeded", e),
        // Multi-writer Phase 1: a member recorder past their owner-set byte
        // cap — client-renderable + retry-after-owner-raises-the-cap.
        e @ StorageQuotaError::MemberCapExceeded { .. } => coded("member_cap_exceeded", e),
        // A malformed declaration, not a quota verdict: the recorder named a
        // negative size. Refused in the metering core so every record door
        // answers alike (`db/sync_storage.rs` `StorageQuotaError::NegativeSize`).
        e @ StorageQuotaError::NegativeSize { .. } => coded("invalid_size", e),
        StorageQuotaError::Db(inner) => internal(inner),
    }
}

/// Parse a wire `name_hash` into a fixed 32-byte array — see
/// `crate::routes::parse_name_hash`.
fn parse_name_hash(
    name_hash: &Option<fauna_protocol::ByteBuf>,
) -> Result<Option<[u8; 32]>, RpcError> {
    crate::routes::parse_name_hash(name_hash, |msg| coded("invalid_request", msg))
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Load the connection actor's **own** folder by name — the owner-scoped read
/// used by the write / owner-infra device-sync handlers (`changes.record`,
/// `sync.status`). Replaces the former name-only `get_folder` + post-fetch
/// owner-equality with the `(name, actor)`-scoped lookup, so a non-owner gets
/// `not_found` rather than `permission_denied` — closing the folder-name
/// existence oracle (ST-RES-1, the N1/ST-1 idiom).
///
/// This is **owner-only** by design — the surfaces still gated by it
/// (`changes.supersede`, `sync.status`) stay owner-scoped under multi-writer
/// Phase 1 (`file-sync.md` § Multi-writer shared sets). Member-readable
/// surfaces (`changes.list`, `sync.files`) use
/// `crate::folder_authz::resolve_readable_folder`; the three write-plane
/// kinds a `writer` member may reach resolve through
/// [`crate::folder_authz::resolve_writable_folder`].
///
/// The request's address is read through the trait — its row when carried,
/// else its name/hash (`set-addressing.md` § The row form).
async fn owned_folder(
    state: &AppState,
    actor_id: &[u8; 32],
    req: &impl SetAddressed,
) -> Result<crate::db::FolderRow, RpcError> {
    crate::routes::resolve_set(&state.db, SYNC, req, SetGate::Owner, actor_id).await
}

/// The write gate — the owner, or a roster member with an explicit `writer`
/// grant (multi-writer Phase 1;
/// [`crate::folder_authz::resolve_writable_folder`] is the one resolver, and
/// absent/not-writable folds to the same `not_found` as [`owned_folder`],
/// ST-RES-1: a reader probing the write plane learns nothing a stranger
/// wouldn't) — plus the one class of set this door mints on demand: the
/// connection actor's **own reserved segment-backup destination set**.
///
/// This is the federation relay's lazy-provision rule
/// (`federation_handlers::resolve_backup_custody_set`) applied to the
/// owner-authed arm, ratified by `docs/goal/architecture/message-segment-store.md`
/// § Client-device custodian (pull) → *Restore* ("the reserved sets get-or-create
/// idempotently server-side"). Its consumer is the **re-seed delivery leg**
/// (`fauna_sync_engine::reseed`): a nest freshly enrolled to receive a
/// custodian's corpus holds no set for the owner yet, and the ceremony
/// deliberately has no pre-enrollment write surface to make one through — so
/// either the first custody record provisions it, or re-seed cannot start.
///
/// Five things keep this narrow, and each is load-bearing:
///
/// 1. **Only a name the one shared derivation can produce**
///    ([`fauna_sync_engine::segment_backup::parse_reserved_backup_set_name`]) —
///    not any `__` name. A reserved rail that is not a backup destination
///    (`__index`, `__mls`) is *not* mintable here, which is what keeps
///    `changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it`
///    true: those rails are non-custody-copy and `record_change_core` refuses
///    them, and this door will not hand a client a custody-copy one instead.
/// 2. **The three reserved axes, and only those** — the segment axis
///    (`parse_reserved_backup_set_name`) and, since the re-seed leg landed, the
///    folder-mirror axis (`parse_folder_backup_set_name`). A folder set name
///    embeds a source nest id, which the **federated** arm must always re-derive
///    from its verified handshake rather than trust a writer for; that refusal
///    is untouched and still lives in `parse_reserved_backup_set_name`'s doc.
///    On *this* door the id is writer-declared and safe, because the writer is
///    the owner and the set is minted under their own actor id: declaring a
///    wrong id mis-files their own corpus for their own later materialize and
///    reaches nobody else (`message-segment-store.md` § Client-device custodian
///    → *Restore*, ratified 2026-08-22 — a custodian re-seeding a fresh nest
///    pushes its held mirrors under exactly these names).
///    **And the landing axis** (`parse_landing_set_name`, ruled 2026-10-21):
///    a delivered copy of the owner's own segment corpus — the re-seed
///    delivery, the nest-held pull-back, the lived-in recovery — lands under
///    `__landing/<kind>`, never the rail's `__<kind>`, because the nest mints
///    that rail within a minute of holding mail and a record into it is the
///    rail's own, seal-gated (`reserved-folders.md` § __mail Sync → *The
///    landing set*).
/// 3. **Only under the connection actor**, whose own quota pays for it — the
///    caller is the owner writing into their own custody, never a foreign writer.
/// 4. **Never a co-mingle**: an existing set of that name in a non-backup mode is
///    the owner's own live set on their own nest, and is refused rather than
///    written into — the same typed refusal, for the same reason, as the
///    federated arm's.
///
/// 5. **Never by row**: a request carrying `folder_id` names an existing row
///    by construction, so an unresolved row is `not_found` and never mints
///    (`set-addressing.md` § The row form, *The row wins*).
async fn writable_or_provisioned_backup_set(
    state: &AppState,
    actor_id: &[u8; 32],
    req: &impl SetAddressed,
) -> Result<crate::db::FolderRow, RpcError> {
    let address = crate::routes::parse_set_address(req, |msg| coded("invalid_request", msg))?;
    if let Some(fs) = crate::folder_authz::resolve_writable_folder(&state.db, address, actor_id)
        .await
        .map_err(internal)?
    {
        return Ok(fs);
    }

    // Nothing resolved. Mint only if the *declared* name is one the shared
    // derivation produces — a hash-addressed request cannot be, since a hash
    // names no set this nest has ever seen, and a row-addressed one never is.
    let crate::folder_authz::FolderAddress::Name { name, .. } = address else {
        return Err(coded("not_found", "folder not found"));
    };
    let segment_axis =
        fauna_sync_engine::segment_backup::parse_reserved_backup_set_name(name).is_some();
    let folder_axis = fauna_core::data::parse_folder_backup_set_name(name).is_some();
    let landing_axis = fauna_sync_engine::segment_backup::parse_landing_set_name(name).is_some();
    if !segment_axis && !folder_axis && !landing_axis {
        return Err(coded("not_found", "folder not found"));
    }

    // A concurrent first record may win the `UNIQUE(name, actor_id)` race; the
    // re-read below is the arbiter either way, so the create's own error is not
    // fatal — mirrored from the federated arm.
    let _ = state
        .db
        .create_folder_with_options(
            name,
            actor_id,
            crate::db::FolderOptions {
                // This provisioner is one of the two writers of the flag
                // (`reserved-folders.md` § Destination capability).
                custody_copy: true,
                ..Default::default()
            },
        )
        .await;

    let Some(fs) = state
        .db
        .get_folder_for_actor(name, actor_id)
        .await
        .map_err(internal)?
    else {
        // The create lost to the owner's own set created by hash alone on
        // this name's address: the coded, recoverable refusal.
        state
            .db
            .refuse_a_squatted_rail_address(name, actor_id)
            .await
            .map_err(internal)?;
        return Err(internal(
            "backup custody set missing immediately after create",
        ));
    };
    // A live rail of this name already held the key: the role holding live
    // state wins, the newcomer is refused (`reserved-folders.md` § Destination
    // capability).
    if !crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
        return Err(coded(
            "permission_denied",
            "reserved set of this name exists on this nest as a live rail, not a backup \
             custody copy",
        ));
    }
    tracing::info!(
        actor = %hex::encode(actor_id),
        set = name,
        "provisioned the owner's reserved backup destination set on first custody record",
    );
    Ok(fs)
}

/// Load a folder by name that the connection actor may **read** — their own
/// set, or a group-bound shared set of that name they're a roster member of
/// (S2-P3). Used by the read handlers (`changes.list`, `sync.files`); a
/// not-found / not-a-member both fold to `not_found` (ST-RES-1).
async fn readable_folder(
    state: &AppState,
    actor_id: &[u8; 32],
    req: &impl SetAddressed,
) -> Result<crate::db::FolderRow, RpcError> {
    crate::routes::resolve_set(&state.db, SYNC, req, SetGate::Reader, actor_id).await
}

/// Map a stored `SyncChangeRow` into the wire `SyncChange` (hex-encoding the
/// path / manifest / device hashes, as the twin's JSON did). `pub(crate)`: the
/// federated read relay (`federation_handlers::folder_changes_fetch_handler`)
/// serves the same wire rows a same-nest `changes.list` does.
pub(crate) fn change_to_wire(c: &crate::db::SyncChangeRow) -> SyncChange {
    SyncChange {
        seq: c.seq,
        path_hash: hex::encode(&c.path_hash),
        manifest_hash: c.manifest_hash.as_ref().map(hex::encode),
        size_bytes: c.size_bytes,
        change_type: c.change_type.clone(),
        created_at: c.created_at,
        path: c.path.clone(),
        device_id: c.device_id.as_ref().map(hex::encode),
        content_key_version: c.content_key_version.map(|v| v as u64),
        content_generation: None,
        thumbnail_hash: c.thumbnail_hash.clone(),
        // Nest-stamped attribution (multi-writer Phase 1): the row's actor_id
        // IS the recorder — for every pre-multi-writer row that was the owner,
        // the only possible recorder, so stamping is correct there too.
        author_actor_id: Some(hex::encode(&c.actor_id)),
        // Opaque to this nest by construction — echoed byte-for-byte so the
        // reader opens it under the generation the envelope itself names
        // (`file-sync.md` § Sealed names & paths).
        path_sealed: c.path_sealed.clone().map(fauna_protocol::ByteBuf::from),
        // Causal stamps — stored opaque, echoed verbatim (the ruling's
        // pass-through: the nest never interprets causality, clients do).
        derived_through: c.derived_through,
        is_resolution: c.is_resolution,
        is_retention: c.is_retention,
        // The depositor of an adopted deposit — a covered field of the owner's
        // statement, echoed verbatim (`folder-deposit.md` ruling 3).
        deposited_by: c.deposited_by.as_ref().map(hex::encode),
        // W2.3 (account-data-plane.md § Workstreams) generalized-feed coordinates. All four are `None` on every file
        // row, which is exactly the shipped semantics — the generalized row
        // EXTENDS this one (`account-sync-plane.md` § Feeds and cursors →
        // *Feed row + wire evolution*).
        item_class: c.item_class.clone(),
        origin_writer: c.origin_writer.as_ref().map(hex::encode),
        origin_seq: c.origin_seq,
        // Opaque by construction: the sealed T14 envelope, echoed byte-for-byte
        // like `path_sealed` beside it. The nest holds no key that opens it.
        entry: c.entry_sealed.clone().map(fauna_protocol::ByteBuf::from),
        extra: Default::default(),
        // The writer's signature + key, echoed verbatim — every reader
        // verifies them itself (writer-signed change records (3)); the cert a
        // delegated signer needs rides the reply's `signer_certs` side table
        // ([`signer_certs_for`]).
        signature: c.signature.clone().map(fauna_protocol::ByteBuf::from),
        signer_key: c.signer_key.clone().map(fauna_protocol::ByteBuf::from),
    }
}

/// Cut a page of change rows at the serve frame budget
/// ([`crate::segments::take_page_within_budget`]), each row costed at the
/// exact canonical-CBOR length of the wire row [`change_to_wire`] makes of it.
///
/// Measured, never estimated: the wire row hex-encodes every hash and id
/// (doubling it), carries `origin_writer` and the field names of a CBOR map,
/// so a sum of the stored fields' raw lengths under-counts each row by a few
/// hundred bytes. On a feed paged by bytes alone (the class-2 account-state
/// feed has no row cap) a page of thousands of small entries then encoded
/// past the 2 MiB WS message cap: the app's socket refused the frame, the app
/// dropped its own connection, and every reconnect walked into the same page,
/// so the scope was never listed. `pub(crate)`: the public-folder and
/// federated folder reads page the same rows.
pub(crate) fn take_change_page(
    rows: Vec<crate::db::SyncChangeRow>,
) -> (Vec<crate::db::SyncChangeRow>, Vec<crate::db::SyncChangeRow>) {
    crate::segments::take_page_within_budget(rows, change_wire_len)
}

/// The exact encoded length of the wire row `c` becomes. An encode failure
/// (unreachable for this plain struct) costs the row the whole budget, so the
/// page closes before it and the caller's frozen-head log names it.
fn change_wire_len(c: &crate::db::SyncChangeRow) -> usize {
    encode_canonical(&change_to_wire(c))
        .map_or(crate::segments::SERVE_PAGE_BUDGET_BYTES, |b| b.len())
}

/// The largest wire encoding a change row may have and still be served: a row
/// costs [`change_wire_len`] + `RECORD_WIRE_OVERHEAD` on a page, and a page holds
/// [`crate::segments::SERVE_PAGE_BUDGET_BYTES`].
pub(crate) const MAX_SERVABLE_CHANGE_WIRE_BYTES: usize =
    crate::segments::SERVE_PAGE_BUDGET_BYTES - crate::segments::RECORD_WIRE_OVERHEAD;

/// Refuse a change row no page could ever serve (`transport.md` § Max frame,
/// *count-paged serve replies are byte-budgeted*).
///
/// A row over [`MAX_SERVABLE_CHANGE_WIRE_BYTES`] still fits the 2 MiB inbound
/// frame, but when it heads a reader's page [`take_change_page`] returns an
/// empty page, and every reader takes an empty page for a drained log: the
/// walk of every co-member and every other device stops at that seq for good,
/// silently, deletes and supersedes after it included. So every door that
/// lands a `sync_changes` row measures the row it is about to store, exactly
/// as the readers will be served it, and refuses it at the door — the shape of
/// `fauna.conversations.channel.send`'s envelope cap.
///
/// `row` is the row as the door will store it; its nest-stamped integers
/// (`seq`, `created_at`) and the retention marker are measured at their widest
/// encodings, so the measure bounds every row the door can mint from it.
/// Returns the measured length when the row is refused.
pub(crate) fn unservable_change_len(row: crate::db::SyncChangeRow) -> Option<usize> {
    let len = minted_change_wire_len(row);
    (len > MAX_SERVABLE_CHANGE_WIRE_BYTES).then_some(len)
}

/// [`change_wire_len`] bound over every row a door can mint from `row`: the
/// nest-stamped integers and the retention marker at their widest encodings.
fn minted_change_wire_len(mut row: crate::db::SyncChangeRow) -> usize {
    row.seq = i64::MIN;
    row.created_at = i64::MIN;
    row.is_retention = Some(true);
    change_wire_len(&row)
}

/// The detail every door's `change_too_large` refusal carries.
pub(crate) fn change_too_large_detail(len: usize) -> String {
    format!(
        "the change row encodes to {len} bytes, over the {MAX_SERVABLE_CHANGE_WIRE_BYTES}-byte \
         serve-page limit (it would end every reader's walk of the folder's change log — \
         transport.md § Max frame)"
    )
}

/// The `signer_certs` side table for a page of rows (`mls-group-key-material.md`
/// § M2 → *Writer-signed change records*, ruling (2)): one embed-as-bytes
/// `DeviceAuthorization` per distinct DELEGATED signer in the page — a direct
/// signer (`signer_key == actor`) needs none. The certs are the ones each
/// signer verified under at ingest (`sync_signer_certs`), so a row recorded
/// before its device was deleted stays verifiable to every reader.
/// `pub(crate)`: the federated read relay serves the same side table.
pub(crate) async fn signer_certs_for(
    db: &crate::db::CacheDb,
    rows: &[crate::db::SyncChangeRow],
) -> Result<Vec<fauna_core::encoding::EmbedAsBytes>, RpcError> {
    let signers = rows.iter().filter_map(|row| {
        Some((
            <[u8; 32]>::try_from(row.actor_id.as_slice()).ok()?,
            <[u8; 32]>::try_from(row.signer_key.as_deref()?).ok()?,
        ))
    });
    signer_certs_for_signers(db, signers)
        .await
        .map_err(internal)
}

/// Room a change-log reply keeps, beside its rows and its `signer_certs`, for
/// everything else it carries: the field names, the cross-nest access and
/// residency stamps, the watermark echoes, the replica id.
const CHANGE_REPLY_FIXED_ROOM: usize = 8 * 1024;

/// [`take_change_page`] for a reply that also carries the page's
/// `signer_certs` side table (`fauna.sync.changes.list`'s folder arm,
/// `fauna.federation.folder.changes.fetch`): the page, the rows left behind,
/// and the side table — which together fit the WS message cap.
///
/// The row cut leaves the frame's 64 KiB headroom for the rest of the reply,
/// and the side table rides in it: one cert per distinct DELEGATED signer in
/// the page, a few hundred bytes each. A page whose signers' certs would pass
/// that headroom (a set written by hundreds of delegated devices) closes
/// earlier still — rows are given back from the tail until the certs of the
/// page that remains fit — so the page stays a contiguous prefix: close early,
/// never skip (`transport.md` § Max frame). A single row is always served,
/// whatever its certs weigh: its one cert is far under the headroom.
pub(crate) async fn take_signed_change_page(
    db: &crate::db::CacheDb,
    rows: Vec<crate::db::SyncChangeRow>,
) -> Result<
    (
        Vec<crate::db::SyncChangeRow>,
        Vec<crate::db::SyncChangeRow>,
        Vec<fauna_core::encoding::EmbedAsBytes>,
    ),
    RpcError,
> {
    use crate::segments::RECORD_WIRE_OVERHEAD;
    let cost = |row: &crate::db::SyncChangeRow| change_wire_len(row) + RECORD_WIRE_OVERHEAD;
    let (mut page, mut rest) = take_change_page(rows);
    loop {
        let certs = signer_certs_for(db, &page).await?;
        let certs_len: usize = certs
            .iter()
            .map(|c| {
                encode_canonical(c).map_or(crate::segments::SERVE_PAGE_BUDGET_BYTES, |b| b.len())
                    + RECORD_WIRE_OVERHEAD
            })
            .sum();
        let rows_len: usize = page.iter().map(cost).sum();
        let total = rows_len + certs_len + CHANGE_REPLY_FIXED_ROOM;
        if total <= fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE || page.len() <= 1 {
            return Ok((page, rest, certs));
        }
        let mut excess = total - fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE;
        let mut given_back = Vec::new();
        while excess > 0 && page.len() > 1 {
            let row = page.pop().expect("page holds more than one row");
            excess = excess.saturating_sub(cost(&row));
            given_back.push(row);
        }
        given_back.reverse();
        given_back.append(&mut rest);
        rest = given_back;
    }
}

/// The most rows a folder change-log page ever loads: one page's worth plus
/// one. [`crate::segments::take_page_within_budget`] costs every row at least
/// [`crate::segments::RECORD_WIRE_OVERHEAD`] beside its own bytes, so no page
/// holds more than `SERVE_PAGE_BUDGET_BYTES / RECORD_WIRE_OVERHEAD` rows; the
/// one extra row lets the cut see that the log goes on. A cap at the query
/// bounds the load under `CacheDb`'s single connection lock — without it a
/// walk of N rows in P pages loaded O(N·P) rows, and any reader of a set
/// could repeat `since = 0` at will.
pub(crate) const FOLDER_CHANGE_PAGE_MAX_ROWS: i64 =
    (crate::segments::SERVE_PAGE_BUDGET_BYTES / crate::segments::RECORD_WIRE_OVERHEAD) as i64 + 1;

/// One signed page of a set's change log above `since`: at most `max_rows`
/// rows loaded (clamped to [`FOLDER_CHANGE_PAGE_MAX_ROWS`]), then cut to the
/// frame by [`take_signed_change_page`]. Shared by `fauna.sync.changes.list`'s
/// folder arm and `fauna.federation.folder.changes.fetch`. Returns the page,
/// the loaded rows the cut left behind (the next page's by the `since`
/// cursor — close early, never skip) and the page's `signer_certs`.
pub(crate) async fn folder_change_page(
    db: &crate::db::CacheDb,
    folder_id: i64,
    since: i64,
    exclude_device_id: Option<&[u8; 32]>,
    max_rows: i64,
) -> Result<
    (
        Vec<crate::db::SyncChangeRow>,
        Vec<crate::db::SyncChangeRow>,
        Vec<fauna_core::encoding::EmbedAsBytes>,
    ),
    RpcError,
> {
    let max_rows = max_rows.clamp(1, FOLDER_CHANGE_PAGE_MAX_ROWS);
    let rows = db
        .get_sync_changes_for_folder_page(folder_id, since, exclude_device_id, max_rows)
        .await
        .map_err(internal)?;
    take_signed_change_page(db, rows).await
}

/// The side table for a set of `(signed actor, signer key)` pairs — the one
/// builder every projection that carries `signer_certs` shares
/// (`changes.list`, the federation fetch, `fauna.media.list`): direct signers
/// (`key == actor`) and duplicates dropped, the stored certs decoded.
pub(crate) async fn signer_certs_for_signers(
    db: &crate::db::CacheDb,
    signers: impl IntoIterator<Item = ([u8; 32], [u8; 32])>,
) -> anyhow::Result<Vec<fauna_core::encoding::EmbedAsBytes>> {
    let mut distinct: Vec<([u8; 32], [u8; 32])> = Vec::new();
    for (actor, key) in signers {
        if actor != key && !distinct.contains(&(actor, key)) {
            distinct.push((actor, key));
        }
    }
    let wires = db.sync_signer_certs(&distinct).await?;
    Ok(wires
        .iter()
        .filter_map(|w| fauna_cbor::decode_strict::<fauna_core::encoding::EmbedAsBytes>(w).ok())
        .collect())
}

// ── fauna.sync.register (≡ POST /api/v1/sync/register) ───────────────────────

fn register_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.register").await?;
            let req: SyncRegisterRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;

            // The nest's own WebDAV pseudo-device id is a *derived*, not a
            // *reserved*, value — nothing stops a client computing it and
            // naming it here, which would register a real device under the
            // one id the device-quota count excludes (`sync_storage.rs`'s
            // `register_sync_device_capped` doc comment), minting an
            // uncounted extra slot. The id is nest-authored, so a client
            // naming it is malformed input by definition — refused
            // unconditionally, not gated on `enforce_tier_quotas`: the
            // desktop nest owes the same guarantee even with no cap to dodge
            // (`docs/goal/behavior/devices.md` § Step 4).
            if device_id == fauna_core::label_custody::webdav_pseudo_device_id(&actor_id) {
                return Err(coded(
                    "invalid_request",
                    "device_id is reserved for the nest's own WebDAV pseudo-device",
                ));
            }
            // The re-seed pseudo-device the same: a label re-homed rows carry
            // (`writer-signed-change-records.md` ruling (7)(a)(ii)), never a
            // registered device.
            if device_id == fauna_core::label_custody::reseed_pseudo_device_id(&actor_id) {
                return Err(coded(
                    "invalid_request",
                    "device_id is reserved for the re-seed ceremony's pseudo-device",
                ));
            }

            // Tier-based device cap — off on the single-user desktop nest,
            // exactly like the feed cap (`feed_routes.rs`, the shape this
            // mirrors). The tier *is* the quota (`admin.md` § 2 Users), and
            // until this landed `max_devices` was read at five sites and
            // compared at none: the number every app shows the user as
            // `AccountGetQuota.devices.max` was enforced by nothing, and the
            // frontier ceiling that sizes itself against it
            // (`fauna_sync_engine::MAX_FRONTIER_WRITERS`) rested on an
            // unbounded term.
            let max_devices = if *state.enforce_tier_quotas.read().await {
                Some(
                    state
                        .db
                        .get_user_tier_max_devices(&actor_id)
                        .await
                        .map_err(|e| {
                            tracing::error!("get_user_tier_max_devices error: {e}");
                            forbidden("user not registered or tier lookup failed")
                        })?,
                )
            } else {
                None
            };

            // `label_sealed` is stored opaquely — the nest holds no key to open
            // or to mint one, so it is neither validated nor derived here
            // (`file-sync.md` § Sealed names & paths).
            state
                .db
                .register_sync_device_capped(
                    &actor_id,
                    &device_id,
                    &req.label,
                    req.label_sealed.as_ref().map(|b| &b[..]),
                    &req.capabilities,
                    max_devices,
                )
                .await
                .map_err(device_quota_err)?;

            encode_reply(&SyncRegisterReply {
                device_id: req.device_id,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.device_grant.register (renewal grant; additive 2026-07-19) ────

/// Store a root-key-signed, `RenewBearer`-scoped `DeviceAuthorization` on the
/// connection actor's `sync_devices` row — the registration half of the sync
/// agent's app-dead bearer renewal (`sync-agent.md` § Credential model; the
/// mint half is `fauna.auth.device_handshake`, `auth_core::device_auth_core`).
/// The verification checks mirror `fauna.subscriptions.delegate.upload`:
///
/// 1. The authorization's `actor_id` is the bearer (only the actor registers
///    their own grant).
/// 2. `capabilities` contains `RenewBearer` (or `All`) — anything else cannot
///    renew.
/// 3. The embedded root-key signature verifies.
/// 4. The named device row exists for this actor (`fauna.sync.register`
///    first) — a grant with no device row would be invisible to the devices
///    UI, i.e. unrevocable.
///
/// **No guardian-marker check, by ruling.** A supervised ward's session can
/// replace the key on the row its guardian marked, and the nest takes it: it
/// cannot tell that write from the guardian's own device registering its next
/// principal, and a rule protecting the key the row carries would protect
/// whichever device wrote there first. The displaced key is not tombstoned
/// and registers again in one call (`family-safety.md` § Full visibility for
/// young children → *The device marker*, the replacement bound; pinned by
/// `conformance_family.rs::a_replaced_credential_on_the_marked_row_tombstones_nothing_and_registers_again`).
fn device_grant_register_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.device_grant.register").await?;
            let req: DeviceGrantRegisterRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;

            let (auth_bytes, auth_env) = req
                .authorization
                .clone()
                .into_signed()
                .map_err(|e| malformed(format!("authorization envelope: {e}")))?;
            let auth: fauna_core::data::DeviceAuthorization =
                fauna_core::encoding::decode_signed_bytes(&auth_bytes)
                    .map_err(|e| malformed(format!("authorization: {e}")))?;

            if auth.actor_id.0 != actor_id {
                return Err(coded(
                    "permission_denied",
                    "can only register your own renewal grant",
                ));
            }
            let has_renew = auth.capabilities.iter().any(|c| {
                matches!(
                    c,
                    fauna_core::data::Capability::RenewBearer | fauna_core::data::Capability::All
                )
            });
            if !has_renew {
                return Err(coded(
                    "invalid_grant",
                    "RenewBearer capability required on a renewal grant",
                ));
            }
            if fauna_core::encoding::verify_envelope(&auth, &auth_bytes, &auth_env).is_err() {
                return Err(coded("invalid_grant", "grant signature invalid"));
            }

            // Store the verified wire blob; the mint path re-verifies at use.
            let wire = encode_canonical(&req.authorization)
                .map_err(|e| internal(format!("re-encode grant wire: {e}")))?;
            match state
                .db
                .set_sync_device_grant(&actor_id, &device_id, &auth.device_key, &wire)
                .await
                .map_err(internal)?
            {
                crate::db::GrantStoreOutcome::Stored => {}
                // Check 5: the nest's revocation memory. The grant itself
                // passes checks 1–4 — it is genuinely root-signed — but its
                // device key was tombstoned by a device deletion, and a
                // replayed credential must not heal the revocation
                // (sync-agent.md § Credential model). A fresh provision mints
                // a fresh keypair, so this never blocks re-enrollment.
                //
                crate::db::GrantStoreOutcome::GrantRevoked => {
                    // A code of its own, unlike the checks above: this is the
                    // one refusal on this kind that no retry can ever clear,
                    // and the caller must be able to tell it apart from the
                    // three retryable `invalid_grant` malformations to raise
                    // the loud removed-from-account state rather than latch
                    // silently (`sync-agent.md` § Credential model → RULED
                    // 2026-08-15, decision 4). It is ALSO the
                    // evidence gate of principal succession: a
                    // ceremony-capable sign-in seeing exactly this code mints
                    // a successor principal (decision 1), which is why
                    // it must never fire on a merely malformed grant — and
                    // why the anonymous handshake path stays opaque while
                    // this authenticated path speaks. An older nest answers
                    // the generic code, which a newer client simply reads as
                    // an ordinary retryable error — today's behaviour, and
                    // the safe direction.
                    return Err(coded(
                        "device_grant_revoked",
                        "this renewal grant was revoked by device deletion — \
                         provision a fresh device keypair and grant",
                    ));
                }
                crate::db::GrantStoreOutcome::NoDevice => {
                    return Err(coded(
                        "not_found",
                        "device not registered — call fauna.sync.register first",
                    ));
                }
            }

            encode_reply(&DeviceGrantRegisterReply {
                registered: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.device_grant.revoke (grant retirement; additive 2026-08-15) ───

/// The proof-of-possession triple gate shared by the device-key self arms (the
/// grant revoke's and the p2p participation report's): all three of
/// `timestamp_ms`/`nonce`/`signature` present, or all three absent — a
/// partial triple is a caller bug, and accepting it would silently downgrade a
/// proof-of-possession call to a session-authorized one. Returns the triple
/// for the caller to verify, or `None` when the session arm (no PoP offered,
/// session-authorized) applies.
fn require_pop_triple_or_none<'a>(
    timestamp_ms: Option<u64>,
    nonce: Option<&'a str>,
    signature: Option<&'a str>,
) -> Result<Option<(u64, &'a str, &'a str)>, RpcError> {
    match (timestamp_ms, nonce, signature) {
        (None, None, None) => Ok(None),
        (Some(timestamp_ms), Some(nonce), Some(signature)) => {
            Ok(Some((timestamp_ms, nonce, signature)))
        }
        _ => Err(coded(
            "invalid_request",
            "the proof-of-possession arm needs timestamp_ms, nonce and signature together",
        )),
    }
}

/// Retire one renewal grant, named by its device public key: clear the grant
/// columns, tombstone the key, revoke the sessions it minted — the 2026-08-15
/// hardening's triple, factored out of `fauna.sync.devices.delete` so a
/// credential can be retired without deleting the device that carries it
/// (`sync-agent.md` § Credential model → the RULED 2026-08-15 block).
///
/// **Authorization — two arms, either sufficient, per the ruling's decision 2.**
///
/// * *App/user arm.* The connection is an authenticated session of the owning
///   account, and the grant is looked up under that actor id, so a caller can
///   only ever reach their own account's grants. This is the arm the signed-out
///   reconcile's nest-side revoke (§ Credential model, *Not in scope,
///   deliberately*) has been waiting for, and the arm a runtime revokes a
///   removed member's grant with at every nest it completes
///   (`fleet-scope-reclamation.md` § Fleet-scope reclamation, clause (4) → *The
///   nest half follows merged state*). It refuses the key a guardian-marked
///   row carries while the account is supervised, as that row's deletion is
///   refused.
/// * *Self arm.* A proof-of-possession signature by the very key being retired.
///   Supplied, it is **verified, never merely noted**: a present-but-invalid
///   triple refuses rather than falling through to the session arm.
///
/// ⚠ **Build-time refinement of the ruling, recorded rather than diverged from
/// silently** (`sync-agent.md` § Credential model carries the same note). The
/// ruling words the first arm as *an **identity**-authenticated session*. The
/// nest cannot express that distinction today: `RpcHandler` receives the
/// connection's actor id and no token metadata, and a device-grant-minted
/// bearer is by design "an ordinary session token" (§ Credential model — the
/// sync-scoped tier is named there as a deliberate non-goal). The narrowing
/// would also buy nothing: any bearer of the account may already call
/// `fauna.sync.devices.delete`, which revokes this grant *and* deletes its
/// device row. The distinction becomes expressible — and worth making here —
/// when that scoped token tier lands; until then this handler is honest about
/// being account-scoped rather than pretending to a check it does not make.
fn device_grant_revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.device_grant.revoke").await?;
            let req: DeviceGrantRevokeRequest = decode(&payload).map_err(malformed)?;

            let device_key = parse_device_id(&req.device_key)
                .ok_or_else(|| coded("invalid_request", "invalid device_key hex"))?;

            // The self arm — see `require_pop_triple_or_none`.
            if let Some((timestamp_ms, nonce_hex, signature_hex)) = require_pop_triple_or_none(
                req.timestamp_ms,
                req.nonce.as_deref(),
                req.signature.as_deref(),
            )? {
                verify_grant_revoke_pop(
                    &state,
                    &actor_id,
                    &device_key,
                    timestamp_ms,
                    nonce_hex,
                    signature_hex,
                )
                .await?;
            } else if state
                .db
                .list_devices_for_actor(&actor_id)
                .await
                .map_err(internal)?
                .iter()
                .any(|d| d.guardian_marked && d.principal.as_deref() == Some(&device_key[..]))
                && state
                    .db
                    .get_guardian_of(&actor_id)
                    .await
                    .map_err(internal)?
                    .is_some()
            {
                // The app/user arm, naming the key a guardian-marked row
                // carries: refused exactly as that row's deletion is
                // (`devices_delete_handler`; family-safety.md § Full
                // visibility → *The device marker*, "the refusal covers the
                // credential"). The marked device authenticates as this very
                // account, so retiring its grant ends the supervision the
                // deletion's refusal protects. Same predicate — `marked AND
                // currently supervised`, never the flag alone — and same
                // code. The self arm above is untouched: a proof of possession
                // is the guardian's device retiring itself.
                return Err(coded(
                    "guardian_marked",
                    "this device was enrolled by your guardian and its sign-in cannot be revoked",
                ));
            }

            let outcome = state
                .db
                .revoke_device_grant(&actor_id, &device_key)
                .await
                .map_err(internal)?;
            let revoked = outcome.cleared;

            // Both halves, identical to the device delete's: every session
            // this key minted dies with the grant rather than living out its
            // own ≤1 h expiry, and every socket those sessions opened closes
            // 4401. Unconditional — a `revoked: false` answer can still mean
            // "the grant was cleared by an earlier attempt whose reply was
            // lost", and a live bearer or socket from it would outlast the
            // retirement the caller believes finished. When the caller IS the
            // retiring device, its own socket closes only after this Reply is
            // out (`AppState::revoke_device_authority`).
            let sessions_revoked = state.revoke_device_authority(&actor_id, &device_key).await;

            encode_reply(&DeviceGrantRevokeReply {
                revoked,
                sessions_revoked: sessions_revoked as u32,
                extra: Default::default(),
            })
        })
    })
}

/// The self arm of `fauna.sync.device_grant.revoke`: drift window, signature by
/// the named key over the domain-tagged payload, single-use replay guard — the
/// `fauna.auth.device_handshake` gates, on a payload that is not a mint.
///
/// The actor-level gates that path also runs (supersession, active, lockout)
/// are deliberately **not** repeated: `device_handshake` is a *pre-identity*
/// kind on an anonymous connection and must establish the actor itself, while
/// this kind runs on a connection whose bearer already passed them at mint. A
/// second consult here would additionally mean a locked-out account cannot
/// retire a credential — refusing a de-escalation is the wrong direction to
/// fail in.
async fn verify_grant_revoke_pop(
    state: &AppState,
    actor_id: &[u8; 32],
    device_key: &[u8; 32],
    timestamp_ms: u64,
    nonce_hex: &str,
    signature_hex: &str,
) -> Result<(), RpcError> {
    verify_device_key_pop(
        state,
        device_key,
        timestamp_ms,
        nonce_hex,
        signature_hex,
        &|nonce| {
            fauna_protocol::auth::device_grant_revoke_signed_message(
                actor_id,
                device_key,
                timestamp_ms,
                nonce,
            )
        },
    )
    .await
}

/// The gates every device-key self arm shares — drift window, signature by the
/// named key, single-use replay guard — over whichever domain-tagged payload
/// the caller builds (the grant revoke's and the p2p participation report's).
/// One body rather than one per arm, so a future gate cannot be added to one
/// arm and forgotten on another; the payload builder is the only difference,
/// and it is exactly the domain separation that keeps the arms apart.
async fn verify_device_key_pop(
    state: &AppState,
    device_key: &[u8; 32],
    timestamp_ms: u64,
    nonce_hex: &str,
    signature_hex: &str,
    signed_message: &(dyn Fn(&[u8]) -> Vec<u8> + Send + Sync),
) -> Result<(), RpcError> {
    use ed25519_dalek::Signature;

    let now_ms = fauna_core::data::Timestamp::now_millis();
    if timestamp_ms.abs_diff(now_ms) > crate::auth_core::MAX_TIMESTAMP_DRIFT_MS {
        return Err(coded("permission_denied", "timestamp drift"));
    }

    let nonce =
        hex::decode(nonce_hex).map_err(|_| coded("invalid_request", "invalid nonce hex"))?;
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Err(coded("invalid_request", "invalid signature")),
    };
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|_| coded("invalid_request", "invalid signature"))?;
    let msg = signed_message(&nonce);
    if !fauna_core::identity::verify_detached(device_key, &msg, &sig_bytes) {
        return Err(coded("permission_denied", "signature verification failed"));
    }

    // Single-use, sharing the mint path's guard: the map keys on signature
    // bytes and the two payloads are domain-separated, so a handshake
    // signature can never present as a revoke (or the reverse).
    if !state
        .auth
        .replay_guard
        .check_and_record(
            &signature.to_bytes(),
            now_ms,
            crate::auth_core::MAX_TIMESTAMP_DRIFT_MS,
        )
        .await
    {
        return Err(coded("permission_denied", "signature already used"));
    }
    Ok(())
}

// ── fauna.sync.changes.list (≡ GET /api/v1/sync/changes) ─────────────────────

fn changes_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                "fauna.sync.changes.list",
                crate::rpc_errors::internal,
            )
            .await?;
            let req: SyncChangesListRequest = decode(&payload).map_err(malformed)?;

            // The custodian-NEST serve door (`account-replica-posture.md` §
            // the custodian-NEST serve door, ruling 1): an owner-fleet device
            // reads the copy this nest HOLDS for its owner, answered from the
            // custodied store — before every branch below, so a fleet bearer
            // never reaches the nest's own tables or the federation relay.
            if class == crate::bridge_method_allowlist::CallerClass::CustodiedFleet {
                return crate::custody_fleet::serve_changes_list(&state, &actor_id, &req).await;
            }

            // Parsed unconditionally, like every other `name_hash`-accepting kind
            // — a malformed hash refuses here rather than being skipped on the
            // branch that does not read `folder` (the
            // contract is `folder_authz`'s own doc comment).
            let name_hash = parse_name_hash(&req.name_hash)?;

            // Cross-nest relay (Phase 2): a foreign shared set's change log
            // lives on its home nest — relay the read there. The home nest
            // authorizes by the caller's recorded foreign membership bound to
            // THIS nest's id; the local branches below are same-nest only.
            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                let channel_hex = req.channel_id.as_deref().ok_or_else(|| {
                    coded("invalid_request", "channel_id is required with nest_url")
                })?;
                return match crate::federation_pool::originate_folder_changes_fetch(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &hex::encode(actor_id),
                    channel_hex,
                    req.since,
                    0,
                )
                .await
                {
                    // The home nest's stamps of the caller's live grant and the
                    // folder's residency ride back verbatim — this relay never
                    // interprets them (each refreshes the member's custody
                    // record, never an authz input here).
                    Ok(Ok(crate::federation_handlers::FedFolderChangesFetchReply {
                        changes,
                        caller_access,
                        residency,
                        signer_certs,
                    })) => {
                        encode_reply(&SyncChangesListReply {
                            changes,
                            caller_access,
                            residency,
                            // The watermark echo is the class-2 arm's alone.
                            complete_through_seq: None,
                            retirable_through_seq: None,
                            replica_id: None,
                            extra: Default::default(),
                            // The home nest's side table, relayed verbatim —
                            // this nest vouches for none of it; the reader
                            // verifies every cert against the row's signer.
                            signer_certs,
                        })
                    }
                    Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                        peer_err,
                        "the cross-nest change-log read",
                    )),
                    Err(pool_err) => {
                        tracing::error!("federation folder changes fetch relay: {pool_err}");
                        Err(internal("federation fetch failed"))
                    }
                };
            }

            // W2.3 item-class routing. Naming `state-entry` asks for the
            // generalized plane's class-2 feed — the request shape that
            // supersedes the reserved-set refusal for a feed-served scope
            // (`account-sync-plane.md` § Feeds and cursors → *Feed row + wire
            // evolution*) — and is the ONLY way to reach
            // one, which is what keeps the supersession opt-in: no shipped
            // client sends this field, and the branches below are byte-for-byte
            // the shipped behavior without it.
            if let Some(class) = req.item_class.as_deref() {
                use fauna_protocol::account_state::ItemClass;
                return match ItemClass::from_wire(class) {
                    Some(ItemClass::RecordCid) => {
                        serve_content_scope_feed(&state, &actor_id, &req).await
                    }
                    _ => serve_account_state_feed(&state, &actor_id, class, &req).await,
                };
            }

            // `name_hash` is a set selector in its own right, not a modifier on
            // the plaintext one: post-flip it is the ONLY selector, since clients
            // stop sending the cleartext name. Reading it only inside the
            // `folder.is_some()` arm answered a one-set question with every set
            // the caller owns.
            let (changes, signer_certs) =
                if req.folder.is_some() || name_hash.is_some() || req.folder_id.is_some() {
                    // Read (S2-P3): the owner OR a member of a group-bound shared set.
                    // The row when carried, else hash-first, so the empty name is
                    // never read.
                    let fs = readable_folder(&state, &actor_id, &req).await?;
                    let exclude = req.device_id.as_deref().and_then(parse_device_id);
                    // A bounded load (`FOLDER_CHANGE_PAGE_MAX_ROWS`), then
                    // frame-budgeted like every other change-log read — close
                    // early, never skip: each consumer walks this log by its
                    // `since` cursor until a page comes back empty
                    // (`devices.md` § fetch-paging completeness), so the rows
                    // left behind are the next page's.
                    let (page, rest, signer_certs) = folder_change_page(
                        &state.db,
                        fs.id,
                        req.since,
                        exclude.as_ref(),
                        FOLDER_CHANGE_PAGE_MAX_ROWS,
                    )
                    .await?;
                    if page.is_empty()
                        && let Some(head) = rest.first()
                    {
                        tracing::error!(
                            folder_id = fs.id,
                            seq = head.seq,
                            "folder change log: a single change row exceeds the WS \
                         frame budget — a reader's walk cannot advance"
                        );
                    }
                    (page, signer_certs)
                } else {
                    // No selector at all. There is no actor-wide feed: every
                    // caller names its folder (by name or by hash), its item
                    // class, or its cross-nest route.
                    return Err(coded(
                        "invalid_request",
                        "a change-log read names its folder (`folder` or `name_hash`)",
                    ));
                };

            encode_reply(&SyncChangesListReply {
                // Same-nest: `access` already rides the member `FolderSummary`
                // this caller lists from; the stamp is the cross-nest relay's.
                caller_access: None,
                residency: None,
                changes: changes.iter().map(change_to_wire).collect(),
                // One writer, one scalar `since` — which already IS this
                // arm's watermark; the echo is the class-2 arm's alone.
                complete_through_seq: None,
                retirable_through_seq: None,
                replica_id: None,
                extra: Default::default(),
                signer_certs,
            })
        })
    })
}

/// `fauna.sync.changes.list` for a principal session — a *scope-dispatched*
/// kind (`apps/bridges.md` § Capability-allowlist enforcement, rule (c)'s
/// first refinement): the request's scope names the arm. An `ext:<kind>`
/// scope is the `records` arm's read door (`third-party-kinds.md` § The
/// record doors) — served when one of the session's `records` qualifiers
/// covers the kind; the scope `state` is the `fauna:personalization:rw` arm's
/// read-only list of the delegable cluster (`authorization-server.md` § Scope
/// grammar → *The fifth arm*) — served when the session holds that arm. Every
/// other spelling refuses, so a principal lists `state-fleet` or a content
/// scope by none. On the account's own feed, refusing every other selector
/// the actor door takes: a folder, a cross-nest route, a custody `of_owner`,
/// another item class.
pub(crate) fn principal_changes_list_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            use fauna_protocol::account_state::ItemClass;
            let mut req: SyncChangesListRequest = decode(&payload).map_err(malformed)?;
            principal_list_scope(&caller, req.scope.as_deref().unwrap_or_default())?;
            let state_feed = ItemClass::StateEntry.as_wire();
            if req.item_class.as_deref() != Some(state_feed)
                || req.folder.is_some()
                || req.name_hash.is_some()
                || req.nest_url.is_some()
                || req.of_owner.is_some()
            {
                return Err(coded(
                    "invalid_request",
                    format!("a connected app lists only a granted scope's {state_feed:?} feed"),
                ));
            }
            // A principal is not one of the account's replicas: it marks no
            // walk the retention gate would wait on before compacting.
            req.walker_id = None;
            serve_account_state_feed(&state, &caller.account, state_feed, &req).await
        })
    })
}

/// Parse the `of_owner` addressing field — the custodied account a CUSTODY
/// session names (W8.6 pin N3). Shared by both feed arms so one spelling of
/// the refusal exists.
fn parse_of_owner(owner_hex: &str) -> Result<[u8; 32], RpcError> {
    hex::decode(owner_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| coded("invalid_request", "of_owner is not 64 hex chars"))
}

// The custody door's authorization predicate — `custody_admits_scope` and
// `admit_content_scope_for_custody` — used to live here, when this file held
// its only two callers. Row 148 slice 2 gave the bulk plane its own custody
// doors (`fauna.segments.list` and the segment byte route), so the predicate
// moved to `crate::custody_admission` with its doc contract intact. Four doors
// now share one verdict; do not re-derive it here.

/// The W2.3 class-2 arm of `fauna.sync.changes.list`: serve a scope's sealed
/// state entries, filtered by the caller's frontier and serve-order watermark,
/// and echo how far the page is complete (`account-sync-plane.md` § Feeds and
/// cursors → *Compaction is a serve-order watermark*).
///
/// **Authorization is the connection actor, full stop.** The account-state scope
/// is per-account by definition (`account-sync-plane.md` § Feeds and cursors →
/// *Scope partition — three scope families, one feed contract*) and resolved from
/// `actor_id`, so — exactly like `fauna.drafts.*` — the
/// request carries no actor and a caller can only ever reach their own scope.
/// That is also why this arm does not go through `readable_folder`: there is
/// no name for a caller to supply and therefore nothing to authorize against.
pub(crate) async fn serve_account_state_feed(
    state: &AppState,
    actor_id: &[u8; 32],
    item_class: &str,
    req: &SyncChangesListRequest,
) -> Result<bytes::Bytes, RpcError> {
    use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};

    if ItemClass::from_wire(item_class) != Some(ItemClass::StateEntry) {
        // Deliberately a refusal, not an empty page: the other three classes are
        // real vocabulary this route does not serve yet, and answering "no rows"
        // would read to a caller as "the scope is empty".
        return Err(coded(
            "invalid_request",
            format!(
                "item_class {item_class:?} is not served by this feed; \
                 only {:?} is",
                ItemClass::StateEntry.as_wire()
            ),
        ));
    }
    let scope = req.scope.as_deref().unwrap_or(ACCOUNT_STATE_SCOPE);
    // A scope no account serves refuses before the custody row is read, so
    // a malformed one costs nothing; an `ext:<kind>` is served per account
    // and is checked again once the account is known.
    if !fauna_protocol::account_state::is_served_scope(scope)
        && fauna_protocol::scope::ext_scope_kind(scope).is_none()
    {
        return Err(coded("invalid_request", format!("unknown scope {scope:?}")));
    }

    // The custody arm (W8.6 — `account-data-plane.md` § Replica posture →
    // *The custody grant + ceremony*): `of_owner` names the custodied
    // account whose feed a CUSTODY-session caller pulls. Authorization is
    // the LIVE capability row, re-derived RIGHT HERE on every request (the
    // "per-request row re-check" — `fauna.capabilities.revoke` deletes the
    // row and the very next request on a live session refuses), and the
    // verdict is the row's own scope set with the shared-audience carve-out
    // (`AdmittedScopes` — the one vocabulary the peer seam evaluates).
    // A caller with no live custody row for the named owner is refused,
    // never silently self-served.
    let account: [u8; 32] = match req.of_owner.as_deref() {
        None => *actor_id,
        Some(owner_hex) => {
            let owner = parse_of_owner(owner_hex)?;
            crate::custody_admission::custody_admits_scope(state, actor_id, &owner, scope).await?;
            owner
        }
    };
    if !serves_state_scope(state, &account, scope).await? {
        return Err(coded("invalid_request", format!("unknown scope {scope:?}")));
    }

    let Some(fs_id) = state
        .db
        .find_state_scope(&account, scope)
        .await
        .map_err(internal)?
    else {
        // Never written to — an empty feed, not an error. Still named: a
        // rebuilt box's empty scope is exactly what a bound device must tell
        // apart from the replica it settled on (`account-sync-plane.md` § The
        // bind leg, ruling 2).
        return encode_reply(&SyncChangesListReply {
            replica_id: Some(fauna_protocol::ByteBuf::from(
                state.db.nest_replica_id().await.map_err(internal)?.to_vec(),
            )),
            ..SyncChangesListReply::default()
        });
    };

    // The retention gate's mark (`fleet-scope-reclamation.md`
    // § *Fleet-scope reclamation*, clause (1)): a walker that names
    // itself beside the watermark it banked is recording "I hold every row at
    // or below this seq" — the claim `fauna.account.state.retire` consults.
    // Only the actor's OWN scope is marked: a custody-session pull of another
    // owner's feed is a key-less relay, never a replica whose staleness
    // could resurrect anything there.
    if req.of_owner.is_none()
        && let (Some(walker_hex), Some(held)) = (req.walker_id.as_deref(), req.held_through_seq)
    {
        let walker = parse_device_id(walker_hex)
            .ok_or_else(|| coded("invalid_request", "invalid walker_id hex"))?;
        state
            .db
            .record_state_walk_mark(fs_id, &walker, held)
            .await
            .map_err(internal)?;
    }
    // The gate's watermark (the same clause → *the gate's watermark*), read
    // AFTER this request's own mark landed so a converged walk's last page
    // never reads its own lag as the fleet's; the actor's own scope only, for
    // the same reason only that scope is marked.
    let retirable_through_seq = if req.of_owner.is_none() {
        state
            .db
            .retirable_through_seq(fs_id, actor_id)
            .await
            .map_err(internal)?
    } else {
        None
    };
    let sealed_under: Option<[u8; 32]> = match req.sealed_under.as_ref() {
        None => None,
        Some(bytes) => Some(
            bytes
                .as_ref()
                .try_into()
                .map_err(|_| coded("invalid_request", "sealed_under must be 32 bytes"))?,
        ),
    };

    let item_key: Option<[u8; 32]> = match req.item_key.as_ref() {
        None => None,
        Some(bytes) => Some(
            bytes
                .as_ref()
                .try_into()
                .map_err(|_| coded("invalid_request", "item_key must be 32 bytes"))?,
        ),
    };

    let frontier = req.frontier.clone().unwrap_or_default();
    // A page read loads a page of rows: the read stops at
    // the first served row that overflows the frame budget, measured exactly
    // as the cut below measures it — every row costs at least
    // `RECORD_WIRE_OVERHEAD`, so this also bounds the row count.
    let mut page_bytes = 0usize;
    let mut page_full = |row: &crate::db::SyncChangeRow| {
        page_bytes += change_wire_len(row) + crate::segments::RECORD_WIRE_OVERHEAD;
        page_bytes > crate::segments::SERVE_PAGE_BUDGET_BYTES
    };
    let read = state
        .db
        .get_account_state_page(
            fs_id,
            &crate::db::account_state::StateFeedFilter {
                since: req.since,
                frontier: &frontier,
                held_through_seq: req.held_through_seq,
                sealed_under: sealed_under.as_ref(),
                item_key: item_key.as_ref(),
            },
            &mut page_full,
        )
        .await
        .map_err(internal)?;

    // Frame-budget the page the way every other feed does — close early, never
    // skip, so the caller's frontier walk stays a contiguous prefix per writer.
    let (rows, rest) = take_change_page(read.rows);
    if rows.is_empty()
        && let Some(head) = rest.first()
    {
        // Unreachable while MAX_STATE_ENTRY_BYTES stays far under the frame
        // budget, which is why this is an error log and not a refusal: if it
        // ever fires, the cap and the budget have drifted apart and the caller's
        // walk cannot advance.
        tracing::error!(
            seq = head.seq,
            "account-state feed: a single entry exceeds the WS frame budget"
        );
    }

    // The echo (`account-sync-plane.md` § Feeds and cursors → *Compaction is a
    // serve-order watermark*), stamped from the page this reply actually cut,
    // never from the one the read computed: complete through its last row when
    // the frame budget closed it early, and through the read's tip when it
    // left nothing behind — an empty page included, which is how a converged
    // walk banks the whole log. A cut that carried no row claims nothing, and
    // a read that stopped on a full page claims only what it loaded.
    let complete_through_seq = if rest.is_empty() && !read.more {
        read.tip
    } else {
        rows.last().map(|row| row.seq)
    };

    encode_reply(&SyncChangesListReply {
        caller_access: None,
        residency: None,
        changes: rows.iter().map(change_to_wire).collect(),
        complete_through_seq,
        retirable_through_seq,
        replica_id: Some(fauna_protocol::ByteBuf::from(
            state.db.nest_replica_id().await.map_err(internal)?.to_vec(),
        )),
        extra: Default::default(),
        // Exempt by class: a state-entry carries its own in-seal writer
        // signature (writer-signed change records (2)), so no cert rides here.
        signer_certs: Vec::new(),
    })
}

/// The class-1 arm of `fauna.sync.changes.list`: serve one **content scope's**
/// records — arrivals and tombstones — filtered by the caller's cursor.
///
/// This is the half of the bootstrap contract that follows the bulk one
/// (`account-data-plane.md` § Store logical schema → *the bootstrap contract*):
/// a replica adopts a scope's segments verbatim, then "walks the scope's feed
/// from a zero frontier to materialize state entries and tombstones". Segments
/// are a snapshot; this is how the replica learns what happened after it.
///
/// **A content scope is one-writer: this nest.** Record order on a content
/// scope is nest-assigned (charter § Feeds and cursors → *Multi-writer fit*),
/// so the cursor is the shipped scalar `since` — the nest-writer slot — and the
/// `frontier` map that carries *device* writers on the class-2 arm has nothing
/// to say here. That is why this arm reads `since` and ignores `frontier`
/// rather than merging the two.
///
/// **Authorization is the connection actor, dispatched per scope-id family**
/// ([`admit_content_scope`]): the own-actor kinds reduce to "the scope id is
/// the caller" (`message-segment-store.md` § Layout), and `conv` — whose scope
/// id is an MLS channel — admits on channel membership, the `actor_channels`
/// roster (`account-sync-plane.md` § Implementation status today → *Built —
/// conv on the content-scope feed…*).
async fn serve_content_scope_feed(
    state: &AppState,
    actor_id: &[u8; 32],
    req: &SyncChangesListRequest,
) -> Result<bytes::Bytes, RpcError> {
    use crate::segments::records_db::FEED_SERVED_KINDS;
    use fauna_protocol::account_state::{ItemClass, OP_RECORD_ADDED, OP_TOMBSTONE};
    use fauna_protocol::scope::Scope;

    let Some(text) = req.scope.as_deref() else {
        return Err(coded(
            "invalid_request",
            "a record-cid feed request names its content scope",
        ));
    };
    // Strict, never repairing (`fauna_protocol::scope` ruling 3): two spellings
    // of one scope would defeat every string-equality comparison downstream.
    let scope: Scope = text
        .parse()
        .map_err(|e| coded("invalid_request", format!("unknown scope {text:?}: {e}")))?;
    let Scope::Content(content) = scope else {
        return Err(coded(
            "invalid_request",
            format!("scope {text:?} is not a content scope; this arm serves content scopes only"),
        ));
    };

    // Nest-gated per door, and a refusal is NOT an empty page (charter ruling
    // 4): to a bootstrapping replica this means *this nest cannot serve this
    // scope yet* — version skew — which it records as unserved and retries,
    // where an empty success would be recorded as converged-empty forever.
    if !FEED_SERVED_KINDS.contains(&content.kind()) {
        return Err(coded(
            "invalid_request",
            format!(
                "this nest does not serve kind {:?} on the content feed (serves: {})",
                content.kind(),
                FEED_SERVED_KINDS.join(", ")
            ),
        ));
    }
    // Two doors, one feed. Without `of_owner` the caller is pulling its own
    // plane and admission is the connection actor ([`admit_content_scope`]).
    // With it, the caller is a CUSTODIAN naming the account it holds, and
    // admission is the live custody row (the record-cid arm of the
    // nest custody door; `account-data-plane.md` § The custody grant + ceremony
    // → *Coverage enumeration*, whose nest-door corollary this implements).
    match req.of_owner.as_deref() {
        None => admit_content_scope(state, actor_id, &content).await?,
        Some(owner_hex) => {
            let owner = parse_of_owner(owner_hex)?;
            crate::custody_admission::admit_content_scope_for_custody(
                state, actor_id, &owner, text, &content,
            )
            .await?;
        }
    }

    let rows = state
        .db
        .content_scope_feed(
            content.scope_id(),
            content.kind(),
            req.since,
            CONTENT_FEED_PAGE_ROWS,
        )
        .await
        .map_err(internal)?;

    let changes: Vec<_> = rows
        .iter()
        .map(|row| SyncChange {
            seq: row.changed_seq,
            // The record CID's **digest** in the shipped `path_hash` slot
            // (charter § Feed row: "for class-1 items the record CID's
            // digest"). The full CID is reconstructible — every record on this
            // plane is dag-cbor-coded — and the replica does exactly that.
            path_hash: hex::encode(row.record_cid.digest()),
            manifest_hash: None,
            size_bytes: 0,
            change_type: if row.tombstoned {
                OP_TOMBSTONE.to_string()
            } else {
                OP_RECORD_ADDED.to_string()
            },
            created_at: row.created_at,
            item_class: Some(ItemClass::RecordCid.as_wire().to_string()),
            ..Default::default()
        })
        .collect();

    encode_reply(&SyncChangesListReply {
        caller_access: None,
        residency: None,
        changes,
        // One writer, one scalar `since` — which already IS this arm's
        // watermark; the echo is the class-2 arm's alone.
        complete_through_seq: None,
        retirable_through_seq: None,
        replica_id: None,
        extra: Default::default(),
        // Exempt by class: a record-cid row's provenance is the record's own
        // envelope (writer-signed change records (2)).
        signer_certs: Vec::new(),
    })
}

/// Per-kind admission for the content feed's class-1 arm — the authorization
/// rule [`FEED_SERVED_KINDS`]'s doc contract requires before a kind may join
/// the served set (`account-sync-plane.md` § Implementation status today →
/// *Built — conv on the content-scope feed…* ruling (d) owns the rule; this
/// is its code).
///
/// Two scope-id families, two rules:
///
/// - **Own-actor kinds** (`post`/`calendar`/`card`/`mail`): the scope id *is*
///   the caller (`message-segment-store.md` § Layout).
/// - **`conv`**: the scope id is an MLS channel, and admission is channel
///   membership read off the `actor_channels` roster
///   ([`crate::db::CacheDb::is_actor_in_channel`]) — the Welcome-delivery
///   projection, the same fact `channel.actors` gates on. MLS leaf state
///   cannot be the fact: it is E2EE client state this nest cannot read. Three
///   deliberate properties: **(1)** this door never writes the fact it checks
///   — no auto-register, unlike `channel.fetch`'s routing-roster parity
///   write. ⚠ Read that as scoped to *this* door, never as "the self-admit is
///   closed": the sibling doors write the fact FOR the caller. `channel.send`
///   and `channel.fetch` each auto-register the caller on any **unclaimed**
///   channel — which is every conversation, since the claim-read gate gates
///   only *claimed* folder channels — and a `channel.send` refused at ingest
///   still leaves the row, the register running before ingest and never
///   rolled back. So "any authenticated actor self-admits to any unclaimed
///   channel's feed by asking" is the posture today, one door over. That does
///   not widen the bound the owner doc sets — `channel.fetch` already serves
///   the same actor strictly more on the same precondition, knowing the
///   channel id — but it is what this gate's reach actually is: real against a
///   passive never-admitted actor, ~zero against a deliberate one. Measured by
///   `feed_admission_tests::the_conv_feed_gate_is_lifted_by_the_callers_own_refused_send`.
///   **Design-accepted, not debt — do not re-file it**: the security review
///   graded it a residual and re-affirmed that, having
///   enumerated this door and `channel_actors_handler` together. Both turns
///   reached it by tracing this call graph, as did the turn that added the
///   test — which is why the caveat now lives here, at the door, instead of
///   only in a point-in-time review;
///   **(2)** a lapsed or absent
///   membership is a flat `forbidden`, identical for "never admitted",
///   "evicted" and "no such channel" (no existence oracle) — the walking
///   replica records the scope unserved (charter ruling 4), and dropping its
///   local data stays the departure seam's decision off an affirmative
///   membership answer, never this refusal's; **(3)** membership is checked
///   *before* the Plan-9 pure-backup gate, so a non-member learns nothing
///   about this nest's backup role. The pure-backup refusal itself mirrors
///   `channel.fetch`'s: on a pure-backup destination the record mirror is
///   empty, and a member-admitted empty page would read as converged-empty
///   forever — exactly the misread ruling 4 forbids.
///
/// **Fail-closed and exhaustive**: a kind added to [`FEED_SERVED_KINDS`]
/// without an arm here is refused, never silently served — pinned by
/// `tests::every_feed_served_kind_has_a_ruled_admission`, so growing the
/// served set without ruling admission reds a test instead of shipping.
///
/// [`FEED_SERVED_KINDS`]: crate::segments::records_db::FEED_SERVED_KINDS
async fn admit_content_scope(
    state: &AppState,
    actor_id: &[u8; 32],
    content: &fauna_protocol::scope::ContentScope,
) -> Result<(), RpcError> {
    match content.kind() {
        "post" | "calendar" | "card" | "mail" => {
            if content.scope_id() != actor_id {
                return Err(coded(
                    "forbidden",
                    "a content scope is served only to the actor it belongs to",
                ));
            }
            Ok(())
        }
        "conv" => {
            let is_member = state
                .db
                .is_actor_in_channel(actor_id, content.scope_id())
                .await
                .map_err(internal)?;
            if !is_member {
                return Err(coded(
                    "forbidden",
                    "a conv scope is served only to the channel's members",
                ));
            }
            crate::bridge_routing_handlers::refuse_if_pure_backup(
                state,
                "conv",
                content.scope_id(),
                || {
                    crate::rpc_errors::pure_backup_destination_ns(
                        "sync",
                        "this nest holds this channel's segments as an opaque backup \
                         and cannot serve its feed",
                    )
                },
            )
            .await
        }
        // Unreachable while this match and FEED_SERVED_KINDS agree — and
        // load-bearing the moment they don't: a served kind with no admission
        // rule must refuse, never fall through to an accidental serve.
        other => Err(coded(
            "invalid_request",
            format!("kind {other:?} has no admission rule on the content feed"),
        )),
    }
}

/// Rows per content-feed page. A class-1 row is a fixed handful of small fields
/// (no inline envelope, unlike the class-2 arm's sealed entry), so the page is
/// bounded by row count rather than by the frame budget those entries need.
const CONTENT_FEED_PAGE_ROWS: i64 = 512;

// ── fauna.account.state.put ──────────────────────────────────────────────────

/// Append one sealed class-2 entry to the calling actor's scope feed (W2.3).
///
/// The nest validates only the plane's own coordinates — it cannot read the
/// entry, and deliberately does not try: every semantic check that matters
/// (which kind, which logical key, which merge stamp) lives inside the seal and
/// is the reader's, per § The class-2 entry form.
fn account_state_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            use fauna_protocol::account_state::{AccountStatePutRequest, KIND_STATE_PUT};
            require_permission(&state, &actor_id, KIND_STATE_PUT).await?;
            let req: AccountStatePutRequest = decode(&payload).map_err(malformed)?;
            put_state_entry(&state, &actor_id, &req).await
        })
    })
}

/// `fauna.account.state.put` for a principal session — the `records` arm's
/// write door (`third-party-kinds.md` § The record doors). The three checks
/// run first, before anything is parsed further: the `scope` is an
/// `ext:<kind>` one of the session's `records` qualifiers covers, and the
/// `writer_id` is the principal's own attested writer key — a principal
/// writes only as itself. Then the same put the account's own devices make,
/// on the account's feed.
pub(crate) fn principal_account_state_put_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            use fauna_protocol::account_state::AccountStatePutRequest;
            let req: AccountStatePutRequest = decode(&payload).map_err(malformed)?;
            // The gate's namespace for this kind (`class_refusal_namespace`),
            // so a principal meets one refusal code whichever half refused.
            principal_ext_scope(&caller, &req.scope, "account")?;
            let own = caller.writer_ed25519.map(hex::encode);
            if own.as_deref() != Some(req.writer_id.as_str()) {
                return Err(crate::rpc_errors::permission_denied_ns(
                    "account",
                    "a connected app writes only as the writer key it attested at consent",
                ));
            }
            // The floor a key-less door can check on a principal's row
            // (`third-party-kinds.md` § Kind namespacing → *Two doors onto the
            // same plane*: "a merge-policy-compatible envelope"): an `ext.*`
            // kind is `latest-wins` at gen 0 by construction, so its row is
            // the v1 form. The seal itself is the replicas' to open.
            if !fauna_core::account_entry_crypto::has_v1_envelope_shape(&req.entry) {
                return Err(state_coded(
                    "invalid_request",
                    "entry is not a sealed gen-0 (v1) envelope",
                ));
            }
            put_state_entry(&state, &caller.account, &req).await
        })
    })
}

/// The list's scope dispatch (rule (c)'s first refinement): `state` is the
/// fifth arm's, read only — refused unless the session holds
/// `fauna:personalization:rw`; every other spelling is the `records` arm's
/// per-kind check below.
fn principal_list_scope(
    caller: &crate::principal_handlers::PrincipalCaller,
    scope: &str,
) -> Result<(), RpcError> {
    use fauna_bridge_atproto::fauna_scope::{FaunaScopeArm, arm_of};
    if scope == fauna_protocol::account_state::ACCOUNT_STATE_SCOPE {
        if caller
            .scopes
            .iter()
            .any(|s| arm_of(s) == Some(FaunaScopeArm::PersonalizationRw))
        {
            return Ok(());
        }
        return Err(crate::rpc_errors::permission_denied_ns(
            SYNC,
            "this connection's grant does not cover the state scope",
        ));
    }
    principal_ext_scope(caller, scope, SYNC).map(|_| ())
}

/// The `records` arm's per-kind reach check (`third-party-kinds.md` § The
/// record doors): `scope` must spell `ext:<kind>` and one of the session's
/// `records` qualifiers must cover the kind — structurally, by the parsed
/// publisher, never a string prefix. Every other scope string — `state`,
/// `state-fleet`, a content scope, another publisher's kind — refuses, by any
/// spelling.
fn principal_ext_scope(
    caller: &crate::principal_handlers::PrincipalCaller,
    scope: &str,
    ns: &str,
) -> Result<fauna_protocol::ext_kind::ExtKind, RpcError> {
    let kind = fauna_protocol::scope::ext_scope_kind(scope).ok_or_else(|| {
        crate::rpc_errors::permission_denied_ns(
            ns,
            format!("a connected app reaches only its own ext:<kind> scopes, not {scope:?}"),
        )
    })?;
    let covered = caller.scopes.iter().any(|s| {
        fauna_bridge_atproto::fauna_scope::records_qualifier(s).is_some_and(|q| q.covers(&kind))
    });
    if !covered {
        return Err(crate::rpc_errors::permission_denied_ns(
            ns,
            format!("this connection's grant does not cover {kind}"),
        ));
    }
    Ok(kind)
}

/// Does this nest serve `scope` on `account`'s class-2 doors? The two
/// first-party scopes always; an `ext:<kind>` scope when one of the account's
/// live principal rows declares the kind **or** the scope already exists here
/// — so revoking the principal never makes the user's own rows unreadable to
/// the user's own replicas (`third-party-kinds.md` § The `ext` sub-scope).
/// Anything else is "unknown scope", which a replica reads as *not served
/// yet*, never as empty.
async fn serves_state_scope(
    state: &AppState,
    account: &[u8; 32],
    scope: &str,
) -> Result<bool, RpcError> {
    if fauna_protocol::account_state::is_served_scope(scope) {
        return Ok(true);
    }
    let Some(kind) = fauna_protocol::scope::ext_scope_kind(scope) else {
        return Ok(false);
    };
    if state
        .db
        .principal_declares_kind(account, &kind.to_string())
        .await
        .map_err(internal)?
    {
        return Ok(true);
    }
    Ok(state
        .db
        .find_state_scope(account, scope)
        .await
        .map_err(internal)?
        .is_some())
}

/// Validate and record one sealed class-2 entry on `account`'s scope feed —
/// the put every door shares once its caller is authorized: the account's own
/// devices (the connection actor) and a principal writing its own kinds.
pub(crate) async fn put_state_entry(
    state: &AppState,
    account: &[u8; 32],
    req: &fauna_protocol::account_state::AccountStatePutRequest,
) -> Result<bytes::Bytes, RpcError> {
    use fauna_protocol::account_state::{
        ACCOUNT_STATE_SCOPE, AccountStatePutReply, MAX_REPLACED_ROWS_PER_PUT,
        MAX_STATE_ENTRY_BYTES, is_state_op,
    };
    if !serves_state_scope(state, account, &req.scope).await? {
        return Err(state_coded(
            "invalid_request",
            format!("unknown scope {:?}", req.scope),
        ));
    }
    if !is_state_op(&req.op) {
        return Err(state_coded(
            "invalid_request",
            format!("unknown op {:?}", req.op),
        ));
    }
    let writer_id = parse_device_id(&req.writer_id)
        .ok_or_else(|| state_coded("invalid_request", "invalid writer_id hex"))?;
    let item_key: [u8; 32] = req
        .item_key
        .as_ref()
        .try_into()
        .map_err(|_| state_coded("invalid_request", "item_key must be 32 bytes"))?;
    if req.writer_seq < 0 {
        return Err(state_coded(
            "invalid_request",
            "writer_seq must not be negative",
        ));
    }
    if req.entry.is_empty() {
        return Err(state_coded("invalid_request", "entry must not be empty"));
    }
    if req.entry.len() > MAX_STATE_ENTRY_BYTES {
        return Err(state_coded(
            "invalid_request",
            format!(
                "sealed entry is {} bytes; the maximum is {MAX_STATE_ENTRY_BYTES}",
                req.entry.len()
            ),
        ));
    }
    // The rows this put covers (`delegable-scope-reclamation.md`
    // § Delegable-scope reclamation, part (2)), each validated as the retire
    // validates its one. The delegable scope only: the fleet scope's retires
    // carry orderings its retention gate enforces, and a replacement would
    // walk round them; an `ext:` scope's rows are a principal's, and a put
    // that superseded another writer's there with no gate is no part of the
    // rule.
    if !req.replaces.is_empty() && req.scope != ACCOUNT_STATE_SCOPE {
        return Err(state_coded(
            "invalid_request",
            "replaces is accepted on the delegable scope only",
        ));
    }
    if req.replaces.len() > MAX_REPLACED_ROWS_PER_PUT {
        return Err(state_coded(
            "invalid_request",
            format!(
                "replaces names {} rows; the maximum is {MAX_REPLACED_ROWS_PER_PUT}",
                req.replaces.len()
            ),
        ));
    }
    let mut replaces = Vec::with_capacity(req.replaces.len());
    for row in &req.replaces {
        let writer = parse_device_id(&row.writer_id)
            .ok_or_else(|| state_coded("invalid_request", "invalid replaces writer_id hex"))?;
        let row_item_key: [u8; 32] =
            row.item_key.as_ref().try_into().map_err(|_| {
                state_coded("invalid_request", "replaces item_key must be 32 bytes")
            })?;
        // No own-item check: a carry's put names the predecessor's row at the
        // predecessor schedule's item key (part (2)), and keys are blinded, so
        // the nest cannot tell it from any other item's. The no-gate supersede
        // rests on the covering row landing in the same transaction.
        if row.writer_seq < 0 {
            return Err(state_coded(
                "invalid_request",
                "replaces writer_seq must not be negative",
            ));
        }
        replaces.push(crate::db::account_state::ReplacedCoordinate {
            item_key: row_item_key,
            writer,
            writer_seq: row.writer_seq,
        });
    }

    let fs_id = state
        .db
        .get_or_create_state_scope(account, &req.scope)
        .await
        .map_err(internal)?;
    let outcome = state
        .db
        .record_account_state_entry(
            account,
            fs_id,
            &item_key,
            &writer_id,
            req.writer_seq,
            &req.op,
            req.entry.as_ref(),
            req.cas_base,
            &replaces,
        )
        .await
        .map_err(internal)?;

    let recorded = match outcome {
        Ok(recorded) => recorded,
        Err(e @ crate::db::account_state::StateEntryError::CasMismatch { .. }) => {
            // Its own code: a losing CAS writer re-reads the head and
            // retries, where a stale replay must simply be dropped.
            return Err(state_coded("cas_mismatch", e));
        }
        Err(
            e @ (crate::db::account_state::StateEntryError::SeqNotAdvancing { .. }
            | crate::db::account_state::StateEntryError::SeqReused { .. }),
        ) => {
            return Err(state_coded("stale_writer_seq", e));
        }
        Err(e @ crate::db::account_state::StateEntryError::ScopeFull) => {
            return Err(state_coded("scope_full", e));
        }
    };

    // Every path that records a feed row owes the best-effort nudge
    // (`account-sync-plane.md` § Feeds and cursors → *Feed row + wire
    // evolution*), scope-tagged.
    if let Ok(Some(fs)) = state.db.get_folder_by_id(fs_id).await {
        notify_sync_changed_scoped(state, &fs, Some(&req.scope)).await;
    }

    // `replaced` is present whenever the request named a row — its presence
    // is how the device learns this nest read the list.
    encode_reply(&AccountStatePutReply {
        seq: recorded.seq,
        replaced: (!req.replaces.is_empty()).then_some(recorded.replaced),
        extra: Default::default(),
    })
}

// ── fauna.account.state.retire ───────────────────────────────────────────────

/// The class-2 feed's own compaction — mark one live row superseded without
/// inserting anything, behind the retention gate and the generation belt
/// (`CacheDb::retire_account_state_entry` owns both; `fleet-scope-reclamation.md`
/// § *Fleet-scope reclamation*, clause (1)).
/// Authorization is the connection actor, exactly like the put: the request
/// carries no actor id and a caller can only ever compact their own scope.
fn account_state_retire_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            use fauna_protocol::account_state::{
                ACCOUNT_STATE_FLEET_SCOPE, AccountStateRetireReply, AccountStateRetireRequest,
                KIND_STATE_RETIRE,
            };
            require_permission(&state, &actor_id, KIND_STATE_RETIRE).await?;
            let req: AccountStateRetireRequest = decode(&payload).map_err(malformed)?;
            if !serves_state_scope(&state, &actor_id, &req.scope).await? {
                return Err(state_coded(
                    "invalid_request",
                    format!("unknown scope {:?}", req.scope),
                ));
            }
            let writer_id = parse_device_id(&req.writer_id)
                .ok_or_else(|| state_coded("invalid_request", "invalid writer_id hex"))?;
            let item_key: [u8; 32] = req
                .item_key
                .as_ref()
                .try_into()
                .map_err(|_| state_coded("invalid_request", "item_key must be 32 bytes"))?;
            if req.writer_seq < 0 {
                return Err(state_coded(
                    "invalid_request",
                    "writer_seq must not be negative",
                ));
            }
            let no_rows_sealed_under: Option<[u8; 32]> = match req.no_rows_sealed_under.as_ref() {
                None => None,
                Some(bytes) => Some(bytes.as_ref().try_into().map_err(|_| {
                    state_coded("invalid_request", "no_rows_sealed_under must be 32 bytes")
                })?),
            };
            // The escrow sweep rides the belt and nothing else: a deletion
            // that named no belted generation would be one the belt never
            // checked (clause (3e)).
            if req.delete_escrow_wraps && no_rows_sealed_under.is_none() {
                return Err(state_coded(
                    "invalid_request",
                    "delete_escrow_wraps needs no_rows_sealed_under",
                ));
            }
            // The generation belt's completeness claim holds only for the
            // fleet scope — every generation-sealed kind seals there — so a
            // belt named on the delegable `state` scope would scan a folder
            // no generation-sealed row can ever live in and pass vacuously
            // .
            if no_rows_sealed_under.is_some() && req.scope != ACCOUNT_STATE_FLEET_SCOPE {
                return Err(state_coded(
                    "invalid_request",
                    "no_rows_sealed_under is only valid for the fleet scope",
                ));
            }

            // Non-creating: a scope never written to holds nothing to retire.
            let Some(fs_id) = state
                .db
                .find_state_scope(&actor_id, &req.scope)
                .await
                .map_err(internal)?
            else {
                return encode_reply(&AccountStateRetireReply {
                    retired: false,
                    extra: Default::default(),
                });
            };
            let outcome = state
                .db
                .retire_account_state_entry(
                    &actor_id,
                    &req.scope,
                    fs_id,
                    &item_key,
                    &writer_id,
                    req.writer_seq,
                    no_rows_sealed_under.as_ref(),
                    req.delete_escrow_wraps,
                )
                .await
                .map_err(internal)?;
            let retired = match outcome {
                Ok(retired) => retired,
                Err(e @ crate::db::account_state::RetireError::NotYetStable { .. }) => {
                    return Err(state_coded("not_yet_stable", e));
                }
                Err(e @ crate::db::account_state::RetireError::GenerationInUse { .. }) => {
                    return Err(state_coded("generation_in_use", e));
                }
            };
            encode_reply(&AccountStateRetireReply {
                retired,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.changes.record (≡ POST /api/v1/sync/changes) ──────────────────

/// The cert a relayed signed record carries inline to the set's home nest
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*: the cert
/// travels by reference within a home nest and inline across a trust
/// boundary). Resolved HERE, by reference over the writer's own live grants —
/// this is the nest that enforces the writer's revocation, since the set-home
/// nest holds no device row for a foreign writer. A cert the client put on the
/// request is never forwarded in its place: forwarding it would let a deleted
/// device's cert outlive the tombstone. `None` for an unsigned record, a direct
/// signature, or a key with no live `SyncWrite` grant (the home nest then
/// refuses the record, which is the right outcome).
async fn relay_signer_cert(
    state: &AppState,
    actor_id: &[u8; 32],
    req: &SyncChangeRecordRequest,
) -> Result<Option<fauna_core::encoding::EmbedAsBytes>, RpcError> {
    let Some(key) = req
        .signer_key
        .as_ref()
        .and_then(|k| <[u8; 32]>::try_from(&k[..]).ok())
    else {
        return Ok(None);
    };
    if key == *actor_id {
        return Ok(None);
    }
    for wire in state
        .db
        .live_sync_device_grants(actor_id, &key)
        .await
        .map_err(internal)?
    {
        let Ok(cert) = fauna_cbor::decode_strict::<fauna_core::encoding::EmbedAsBytes>(&wire)
        else {
            continue;
        };
        let grants_sync_write = fauna_core::encoding::decode_signed_bytes::<
            fauna_core::data::DeviceAuthorization,
        >(&cert.bytes)
        .is_ok_and(|auth| {
            auth.capabilities
                .iter()
                .any(|c| c.grants(&fauna_core::data::Capability::SyncWrite))
        });
        if grants_sync_write {
            return Ok(Some(cert));
        }
    }
    Ok(None)
}

fn changes_record_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.changes.record").await?;
            let req: SyncChangeRecordRequest = decode(&payload).map_err(malformed)?;

            // Cross-nest relay (Phase 3): a `writer` member records into a set
            // homed elsewhere. When nest_url is set, relay to the home nest's
            // federated changes.record instead of the local path (which would
            // fail not_found — this nest holds no row for a foreign set). The
            // home nest applies its own foreign-member + writer gate + metering;
            // fail-loud per the ratified wire rule. Runs before the local device
            // gate (a foreign set has no local set row to gate on here).
            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                let channel_hex = req.channel_id.as_deref().ok_or_else(|| {
                    coded("invalid_request", "channel_id is required with nest_url")
                })?;
                // The writer's cert crosses the trust boundary INLINE, resolved
                // here — the writer's own home nest, the one that holds its
                // grants and tombstones (writer-signed change records (1)).
                let signer_cert = relay_signer_cert(&state, &actor_id, &req).await?;
                return match crate::federation_pool::originate_folder_changes_record(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &hex::encode(actor_id),
                    channel_hex,
                    &req.device_id,
                    &req.path,
                    req.manifest_hash.clone(),
                    req.size_bytes,
                    &req.change_type,
                    req.content_key_version,
                    req.thumbnail_hash.clone(),
                    req.path_sealed.clone(),
                    req.derived_through,
                    req.is_resolution,
                    req.deposited_by.clone(),
                    req.signature.clone(),
                    req.signer_key.clone(),
                    signer_cert,
                )
                .await
                {
                    Ok(Ok(seq)) => encode_reply(&SyncChangeRecordReply {
                        seq,
                        extra: Default::default(),
                    }),
                    Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                        peer_err,
                        "the cross-nest change record",
                    )),
                    Err(pool_err) => {
                        tracing::error!("federation folder changes record relay: {pool_err}");
                        Err(internal("federation record failed"))
                    }
                };
            }

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;

            // The device must be registered to **this connection actor** and hold
            // the `write` capability. Scoped to `actor_id` — a `device_id` is an
            // attacker-controllable wire param, so the unscoped
            // `get_device_capabilities` (which matches a device registered to *any*
            // actor) must not gate an authenticated plane; see the lookup's own doc
            // contract (`db/sync_storage.rs` `get_device_capabilities_for_actor`).
            match state
                .db
                .get_device_capabilities_for_actor(&actor_id, &device_id)
                .await
                .map_err(internal)?
            {
                Some(caps) if caps.contains("write") => {}
                Some(_) => {
                    return Err(coded("permission_denied", "device lacks write capability"));
                }
                // Dedicated code (not the overloaded `permission_denied`) so a
                // client can distinguish "this actor's device simply isn't
                // registered yet" from an authz/capability denial and self-heal
                // it. The shared Media write seam keys its register-and-retry on
                // exactly this code (`fauna_media_machine` `record_self_healing`):
                // a folder-less client registers its sync device only on
                // location-map, so a media upload from a never-mapped client lands
                // here — registering it write-capable + retrying is safe and
                // idempotent (`file-sync.md` § Device Registration).
                None => return Err(coded("device_unregistered", "device not registered")),
            }

            // Multi-writer Phase 1: the owner OR a `writer`-granted roster member
            // records (file-sync.md § Multi-writer shared sets — this is one of
            // the exactly-three widened kinds).
            //
            // A carried `folder_id` names the row: resolved under the write
            // gate alone, never falling through to the name forms and never
            // provisioning (a row names an existing row by construction), so a
            // same-named own set cannot shadow the shared set the writer meant
            // (`set-addressing.md` § The row form).
            let fs = writable_or_provisioned_backup_set(&state, &actor_id, &req).await?;

            // The metering + floor + backup/web routing is IDENTICAL to the
            // cross-nest federated record relay (`federation-shared-folders.md` § Cross-nest…):
            // both call this one core, so a same-nest and a federated write of
            // the same content behave identically (uniform shape — priority #2).
            // The gate differs (this plane's device write-capability check
            // above; the federated plane's structural foreign-member + writer
            // gate) and is the caller's job; the core takes an already-resolved,
            // already-authorized set.
            let seq = record_change_core(
                &state,
                &actor_id,
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
                )
                .with_deposited_by(parse_deposited_by(req.deposited_by.as_deref())?),
                // Same-nest: the signer's cert is resolved by reference over
                // this actor's grants (a carried `signer_cert` is ignored).
                crate::change_signature::CertCarriage::ByReference,
            )
            .await?;

            encode_reply(&SyncChangeRecordReply {
                seq,
                extra: Default::default(),
            })
        })
    })
}

/// The depositor a record request names (`folder-deposit.md` ruling 3), parsed
/// from its hex — refused `invalid_request` when malformed, never dropped, so
/// a record cannot store a statement other than the one it signed. Shared by
/// the same-nest door and the cross-nest relay's home side.
pub(crate) fn parse_deposited_by(hex: Option<&str>) -> Result<Option<[u8; 32]>, RpcError> {
    hex.map(|h| {
        fauna_core::hex32::decode(h)
            .map_err(|_| coded("invalid_request", "deposited_by must be 32-byte hex"))
    })
    .transpose()
}

/// The record body shared by the same-nest `fauna.sync.changes.record` handler
/// and the cross-nest `fauna.federation.folder.changes.record` relay: version
/// floor → owner-pays quota + member cap → backup/reserved/web routing → the
/// content-idempotent metered insert. The caller supplies an already-resolved,
/// already-authorized [`FolderRow`] and the recorder's actor id (the wire
/// `author_actor_id` the nest stamps); every authorization decision is the
/// caller's, so this core is gate-free by construction.
///
/// Returns the assigned `seq` (0 for a backup destination; on a
/// content-identical replay, the existing row's `seq` — the exactly-once
/// guarantee lives in `record_sync_change_metered`).
///
/// `path_sealed` is the client's opaque `SealedLabel` over `path`
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths). Both record
/// planes carry it into the same store, so a same-nest and a cross-nest write
/// of the same file seal identically; this nest holds no key that opens it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_change_core(
    state: &AppState,
    recorder: &[u8; 32],
    fs: &crate::db::FolderRow,
    path: &str,
    manifest_hash_hex: Option<&str>,
    size_bytes: i64,
    change_type: &str,
    content_key_version: Option<u64>,
    thumbnail_hash: Option<&str>,
    device_id: &[u8; 32],
    path_sealed: Option<&[u8]>,
    derived_through: Option<i64>,
    is_resolution: Option<bool>,
    carried: crate::change_signature::CarriedSignature<'_>,
    carriage: crate::change_signature::CertCarriage<'_>,
) -> Result<i64, RpcError> {
    let path_hash: [u8; 32] = fauna_core::sync::path_hash(path);
    let manifest_hash = manifest_hash_hex.and_then(parse_device_id);

    // THE approved compat break (S9 flip — `encryption-at-rest.md`
    // § Carve-outs, user-approved 2026-08-01): a record carrying no
    // `path_sealed` is REFUSED loudly on every plane whose resting plaintext
    // scrubbed at the flip — a silent hash-only row is un-appliable by
    // pullers and un-listable by everyone, so the visible error is strictly
    // better. Three exemptions, each a ratified plaintext class rather than a
    // writer convenience:
    // - a `public`-audience folder's names and paths are world-readable by
    //   ratified design — they are URLs (phase 4; `principles.md` § The user
    //   always controls their data, the one deliberate exception);
    // - a CUSTODY COPY's rows are
    //   machine-authored segment paths (the segment-backup coordinator holds
    //   no seal root and its paths are routing keys — the synthetic-label
    //   class, `encryption-at-rest.md` § Conformance → the held-for-friends
    //   row: "a backup *destination* never receives real paths").
    //
    //   ⚠ This arm keys on `is_reserved_custody_copy` — flag AND name — and NOT
    //   on the bare reserved name, on purpose: the predicate is the very one
    //   that routes the record to `backup_custody` below, so the exemption is
    //   coextensive with its only consumer by construction. A reserved name
    //   alone buys no plaintext: a sealless record on a `__` rail
    //   is refused right here as `path_seal_required`, BEFORE the wholesale
    //   reserved-rail refusal further down ever sees it. That ordering is the
    //   point — the wholesale refusal guards the GC direct-blob hazard, and
    //   were it ever relaxed (W2.3-style explicit routing, a new rail kind),
    //   this arm would still rest no plaintext path in `sync_changes` for a
    //   reserved set, so the S9 scrub's reserved-rail inventory exemption
    //   and this write exemption can never compose into a plaintext parking
    //   lot.
    let reserved_custody =
        crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name);
    let rests_plaintext_paths = fs.rests_plaintext_paths() || reserved_custody;
    if path_sealed.is_none() && !rests_plaintext_paths {
        return Err(coded(
            "path_seal_required",
            "this nest rests no plaintext paths (S9 flip): the record must carry \
             path_sealed — a seal-less writer cannot sync this set",
        ));
    }
    // The REST value for the plaintext column: only the exempt classes
    // above keep resting it; everything else rests NULL and the seal is the
    // only label.
    let rest_path = rests_plaintext_paths.then_some(path);

    // Owner-pays (multi-writer Phase 1): the metered actor is the SET OWNER
    // whoever records; a member recorder additionally carries the set's derived
    // channel for the same-step role-row bump + cap check. Cross-nest, the
    // recorder is always a foreign member (the owner is home-nest-local), so
    // `member_channel` is always `Some` on that plane.
    let owner: [u8; 32] = fs
        .actor_id
        .as_slice()
        .try_into()
        .map_err(|_| internal("folder owner id is not 32 bytes"))?;
    let member_channel: Option<[u8; 32]> = if owner == *recorder {
        None
    } else {
        // A non-owner recorder means a group-bound set by construction (the
        // caller's writer gate proved it); the channel keys their role row.
        let group_id = fs
            .mls_group_id
            .as_ref()
            .ok_or_else(|| internal("writer-writable set has no mls_group_id (unreachable)"))?;
        Some(fauna_mls::types::ChannelId::from_group_id(group_id).0)
    };

    // Version floor (KMH § M2, D2): a non-owner record must prove key-freshness
    // — floor established AND (stamp absent OR below it) → typed retryable
    // refusal (absent fails closed: an unstamped record cannot prove
    // freshness). Owner devices are exempt (their resume/drain machinery
    // self-corrects). The heal is the drain's requeue-under-current after the
    // next custody ingest — the client treats this like `supersede_head_mismatch`:
    // re-derive, retry.
    //
    // THE PUBLIC-AUDIENCE ARM (2026-08-21 — the seventh record site to
    // take phase 4's plaintext exemption, after the six S9 path-seal ones just
    // above). A `public`-audience folder rests its content plaintext by ratified
    // design, and `folders.md` § Target re-model puts members inside that
    // design: "audience rides **both** projection arms … a member's engine takes
    // the plaintext arm off it". A declassified record therefore carries NO
    // generation, which `>= floor` can never satisfy — so before this arm a
    // writer member could never declassify, and every member-authored file on a
    // folder the owner published stayed sealed at rest forever while the owner's
    // own files declassified fine (the site serving in part, silently).
    //
    // The gate predates the class rather than contradicting it: it landed
    // 2026-07-19, a month before phase 4, when every record was sealed and an
    // absent stamp could only mean an old client.
    //
    // Three reasons the exemption is not a way around the floor:
    // - it is UNSTAMPED-only — a record stamped BELOW the floor is still refused
    //   here, on a public folder as on any other. "No key at all" is the
    //   ratified plaintext shape; "a superseded key" is the rotation race
    //   KMH § M2 describes, and this does not merge them;
    // - it is PUBLIC-only — everywhere else an absent stamp still fails closed;
    // - it grants no access. Removal is enforced at the ROLE gate
    //   (`folder_authz::resolve_writable_folder` demands roster membership AND
    //   an explicit `writer` role row; evict deletes the row), which every
    //   caller passed before reaching this core. And the confidentiality a
    //   rotation protects is moot for bytes their owner published to the world.
    //
    // It also restores the refusal's documented contract: `file-sync.md`
    // § Multi-writer shared sets says a `stale_content_key` heals by re-sealing
    // "after the next rotation-commit custody ingest advances the current
    // generation". A plaintext record has no generation to advance to and must
    // not be re-sealed while public, so refusing it was never a retryable
    // refusal at all — it was a permanent wedge wearing a retryable code.
    let rests_plaintext_content = fs.is_public_audience();
    if let Some(channel) = &member_channel {
        let floor = state
            .db
            .get_folder_content_key_floor(channel)
            .await
            .map_err(internal)?;
        if let Some(floor) = floor {
            let fresh = content_key_version.is_some_and(|v| v as i64 >= floor);
            let declassified = content_key_version.is_none() && rests_plaintext_content;
            if !fresh && !declassified {
                return Err(coded(
                    "stale_content_key",
                    "record is stamped below the set's content-key floor — \
                     ingest the rotated content key and re-seal, then retry",
                ));
            }
        }
    }

    // Storage quota: the tier *is* the quota — every record is metered against
    // the SET OWNER's tier `max_storage_bytes`, uniform across all tiers
    // (`docs/goal/behavior/admin.md` § 2 Users; owner-pays, `file-sync.md`
    // § Multi-writer shared sets). A record that would push the owner past their
    // cap is rejected with the typed `storage_quota_exceeded` error; supersede /
    // delete credit the freed bytes back.
    let max_storage = state
        .db
        .get_user_tier_max_storage_bytes(&owner)
        .await
        .map_err(internal)?
        .unwrap_or(i64::MAX);

    // Custodian-authoritative backup custody: a RESERVED (`__*`)
    // a custody-copy set is a cross-location backup destination — records go
    // to the latest-per-path `backup_custody` projection that GC walks, NOT
    // the append-only `sync_changes` device-sync feed. This makes the
    // device-pull exclusion automatic (destination custody never enters the
    // feed) and gives latest-per-path reclamation on supersede / compacted-out
    // (`docs/goal/architecture/message-segment-store.md` § GC-safety —
    // custodian-authoritative custody). A `delete` (manifest_hash = None)
    // tombstones the path so its now-orphaned chunks reclaim. The reply `seq` is
    // 0 — a backup destination is write-only custody, never pulled back by a
    // device, so no monotonic sequence is assigned. (A rebuilt nest gets a
    // destination's copy back through the owner's `fauna.backup.custody.list`
    // and the open blob routes — the nest-held pull-back,
    // `fauna_sync_engine::reseed_pull` — never through this feed.)
    //
    // ORDINARY Backup folders stopped routing here at the folders re-model
    // phase 3 head unification (2026-08-17): they record to `sync_changes`
    // like every other folder (`file-sync.md` § Membership → *Target
    // state — head unification*), device-attributed and pullable.
    if crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
        match manifest_hash {
            Some(mh) => {
                // The custody writer is a foreign party under a grant, and the
                // ratified supersede rate cap is the owner's quota — so the
                // charge is DERIVED from what this nest actually holds under
                // the manifest (server-measured stored sizes), never taken
                // from the writer's declared `size_bytes` (declaring 0 would
                // make a supersede storm free — review (xxxi-c)). Bytes not
                // held ⇒ typed retryable refusal, never a zero charge; an
                // honest writer uploads before recording so it never sees it.
                // The routing seam mirrors `custody_set_retains_generations`
                // — the charge-honesty boundary IS the grace window's
                // foreign-writer trust boundary.
                let charge = {
                    let svc = state.backup_service.as_ref().ok_or_else(|| {
                        internal("backup custody record with no blob store configured")
                    })?;
                    crate::backup::custody_charge::derive_held_custody_charge(
                        &state.db,
                        &svc.local_blob_store(),
                        svc.encryption_key(),
                        &mh,
                    )
                    .await
                    .map_err(internal)?
                    .map_err(|refusal| {
                        coded(
                            "backup_bytes_not_held",
                            format!("{refusal} — upload the bytes, then re-record"),
                        )
                    })?
                };
                state
                    .db
                    .upsert_backup_custody(
                        // Owner-pays: the custody charge keys on the set owner
                        // (identical to the recorder pre-Phase-1).
                        &owner,
                        fs.id,
                        &path_hash,
                        rest_path,
                        &mh,
                        charge,
                        thumbnail_hash,
                        path_sealed,
                        max_storage,
                    )
                    .await
                    .map_err(quota_err)?
            }
            None => state
                .db
                .tombstone_backup_custody(fs.id, &path_hash)
                .await
                .map_err(internal)?,
        }
        return Ok(0);
    }

    // Past the backup branch, every record lands in `sync_changes` — and a
    // reference held only by reserved (`__`) sets is classified **direct blob**
    // by the GC: pinned, never walked for chunks (`backup-restore.md` § 9). A
    // chunked manifest recorded here would have its live chunks swept, silently
    // and unrecoverably. The nest's own direct-blob rails (`__mls`,
    // `__drafts`, `__index`) write their rows at the DB layer, not through this
    // kind, so no legitimate caller reaches this line with a reserved name —
    // except a backup coordinator whose destination set collided with a live
    // same-named rail and so never got the custody-copy flag. Refusing is what makes
    // that collision loud instead of lossy (`folders.create` rejects it up
    // front; this is the belt to that braces).
    // ⚠ W2.3 (`account-sync-plane.md` § Feeds and cursors → *Feed row + wire
    // evolution*) supersedes this refusal for
    // **feed-served scopes reached by explicit `item_class` routing** — and for
    // nothing else. The supersession is not written here because it does not
    // *weaken* this branch: a class-2 write arrives on its own kind
    // (`fauna.account.state.put` → `record_account_state_entry`), which never
    // enters `record_change_core`, so the legacy-shaped request this function
    // serves keeps being refused exactly as before. The charter's "supersede"
    // is about which request shape can reach a reserved scope at all, and the
    // answer is: the explicitly-routed one, never this one. `conformance_sync`
    // pins both halves so a later refactor cannot quietly merge them.
    if crate::db::snapshots::is_reserved_folder_name(&fs.name) {
        return Err(coded(
            "invalid_request",
            "the \"__\" folder namespace is reserved for the nest's internal rails; \
             a reserved set takes device-sync changes only as a backup destination",
        ));
    }

    // Writer-signed change records (`mls-group-key-material.md` § M2 →
    // *Multi-writer* → *Writer-signed change records*, ruling (3)). Placed
    // after the set-class routing above on purpose — reserved (`__`) sets are
    // out of scope by set class and never reach here — and before the insert,
    // so a refusal writes and charges nothing. The statement is rebuilt from
    // exactly what the row will store (the reader rebuilds it from the row),
    // with the recorder as its signed actor and the set's STORED nonce as its
    // binding: a record signed for another set, or for a deleted predecessor
    // of this one, fails here.
    // A row no page could serve is refused before anything is verified,
    // charged or written ([`unservable_change_len`]).
    if let Some(len) = unservable_change_len(crate::db::SyncChangeRow {
        seq: 0,
        path_hash: path_hash.to_vec(),
        manifest_hash: manifest_hash.map(|m| m.to_vec()),
        size_bytes,
        change_type: change_type.to_string(),
        created_at: 0,
        path: rest_path.map(str::to_string),
        device_id: Some(device_id.to_vec()),
        content_key_version: content_key_version.map(|v| v as i64),
        thumbnail_hash: thumbnail_hash.map(str::to_string),
        actor_id: recorder.to_vec(),
        path_sealed: path_sealed.map(<[u8]>::to_vec),
        derived_through,
        is_resolution,
        is_retention: None,
        item_class: None,
        origin_writer: None,
        origin_seq: None,
        entry_sealed: None,
        signature: carried.signature.map(<[u8]>::to_vec),
        signer_key: carried.signer_key.map(<[u8]>::to_vec),
        deposited_by: carried.deposited_by.map(|d| d.to_vec()),
    }) {
        return Err(coded("change_too_large", change_too_large_detail(len)));
    }

    let refuse = |r: crate::change_signature::IngestRefusal| r.into_rpc(SYNC);
    if carried.is_unsigned() {
        return Err(refuse(crate::change_signature::IngestRefusal::Required));
    }
    let verified = {
        let set_nonce = crate::change_signature::stored_set_nonce(fs).map_err(refuse)?;
        let thumbnail_hash = thumbnail_hash
            .map(|t| {
                fauna_core::hex32::decode(t).map_err(|_| {
                    coded(
                        "signature_invalid",
                        "a signed record's thumbnail_hash must be 32-byte hex",
                    )
                })
            })
            .transpose()?;
        let statement = fauna_protocol::sync_writer_sig::SignedChange {
            set_nonce,
            actor_id: *recorder,
            device_id: *device_id,
            path_hash,
            manifest_hash,
            change_type: change_type.to_string(),
            size_bytes,
            content_key_version,
            content_generation: None,
            path_sealed: path_sealed.map(<[u8]>::to_vec),
            thumbnail_hash,
            derived_through,
            is_resolution: is_resolution.unwrap_or(false),
            is_retention: false,
            deposited_by: carried.deposited_by,
        };
        crate::change_signature::verify_carried(&state.db, &statement, carried, carriage)
            .await
            .map_err(refuse)?
    };

    let seq = state
        .db
        .record_sync_change_metered_signed(
            // Recorder (row attribution — the wire `author_actor_id`).
            recorder,
            // Metered actor (owner-pays) + the member cap context.
            &owner,
            member_channel.as_ref(),
            &path_hash,
            manifest_hash.as_ref(),
            size_bytes,
            change_type,
            fs.id,
            device_id,
            rest_path,
            content_key_version.map(|v| v as i64),
            thumbnail_hash,
            path_sealed,
            derived_through,
            is_resolution,
            max_storage,
            Some(verified.as_row()),
        )
        .await
        .map_err(quota_err)?;
    verified.remember_cert(&state.db).await.map_err(internal)?;

    // Route a website folder's change into `web_files`
    // (`web_files_projection`). Keyed on the WEBSITE TOGGLE, not `mode` (phase 4 — the
    // ratified no-migration ruling maps every legacy `mode='web'` row to
    // toggle-off, owner re-enables by hand). The fan-out is IN ADDITION TO the
    // head row this function just recorded, never instead: `web_files` is not
    // a GC reachability source, so the co-written head row is what pins the
    // site's blobs (`backup/gc.rs`). This rail is the one that carries the M2
    // `content_key_version`, so it is the ONLY way a **sealed** (paywalled)
    // web file can be ingested at all (`monetization.md` § Pillar 2).
    if fs.website_enabled {
        crate::web_files_projection::route_web_file_change(
            &state.db,
            state.web_content_service.as_ref(),
            // The web_files rail belongs to the SITE OWNER, whoever recorded
            // (identical to the recorder pre-Phase-1).
            &owner,
            path,
            change_type,
            manifest_hash,
            fs.id,
            content_key_version.map(|v| v as i64),
        )
        .await;
    }

    // Best-effort same-nest download-half nudge: tell every connected participant
    // of the set to pull now instead of waiting out the rescan interval.
    notify_sync_changed(state, fs).await;

    Ok(seq)
}

/// Best-effort same-nest push nudge on a new sync record: fire
/// `PushEvent::SyncChanged` at every connected participant of the set (owner +
/// same-nest roster members) so their resident engines schedule an immediate
/// off-cadence pull instead of waiting out the rescan interval. The
/// download-half twin of [`notify_calendar_changed`](crate::bridge_caldav_handlers)
/// / [`segments::notify_mail_received`](crate::segments). Recipients are
/// `actor_channels` rows — **same-nest by construction** (a cross-nest member
/// has no connection here and is correctly skipped; its home nest is elsewhere),
/// so this grows no federation kind. Best-effort — `notify_push` drops it if a
/// participant has no live WS, and the periodic reconcile is the correctness
/// backstop, so a missed push costs only latency. Fired unconditionally after a
/// durable insert: a rare byte-identical replay re-fires a harmless no-op pull
/// (the metered path's idempotent early-return does not surface a new-vs-replay
/// bit, and threading one through the quota-critical path is not worth a
/// latency-only nudge). Per `docs/goal/behavior/file-sync.md` § Remote-change
/// nudge.
pub(crate) async fn notify_sync_changed(state: &AppState, fs: &crate::db::FolderRow) {
    notify_sync_changed_scoped(state, fs, None).await
}

/// [`notify_sync_changed`] with the generalized plane's **scope tag** (W2.3,
/// `account-sync-plane.md` § Feeds and cursors → *Feed row + wire evolution* —
/// *"scope-tagged so a replica pulls only the scope that moved"*).
///
/// A folder nudge passes `None`: its `folder` already names the scope that
/// moved, and adding a redundant tag there would change a shipped payload for
/// no reader. The account-state scope passes its scope name, because its rail
/// name (`__state`) is a nest-side placement detail the client engine does not
/// key on — the scope is what a replica pulls.
pub(crate) async fn notify_sync_changed_scoped(
    state: &AppState,
    fs: &crate::db::FolderRow,
    scope: Option<&str>,
) {
    let payload = || sync_changed_payload(fs, scope);
    match fs.mls_group_id.as_deref() {
        Some(group_id) => {
            // Shared set: `list_channel_actors` returns every same-nest roster
            // actor (owner + members), so a single fan-out covers everyone.
            let channel = fauna_mls::types::ChannelId::from_group_id(group_id).0;
            match state.db.list_channel_actors(&channel).await {
                Ok(actors) => {
                    for actor in actors {
                        state.ws.notify_push(&actor, payload());
                    }
                }
                Err(e) => tracing::warn!(
                    folder_id = fs.id,
                    error = %e,
                    "notify_sync_changed: channel roster read failed; skipping nudge"
                ),
            }
        }
        None => {
            // Owner-only set: no channel — nudge the owner's own connected
            // devices (the multi-device single-user sync case).
            if let Ok(owner) = <[u8; 32]>::try_from(fs.actor_id.as_slice()) {
                state.ws.notify_push(&owner, payload());
                // The account's third-party principals hear a scope-tagged
                // nudge too, filtered to the scopes they may list
                // (`transport.md` § Push events → *Third-party event doors*).
                if let Some(scope) = scope {
                    crate::events_doors::on_scope_changed(state, &owner, scope).await;
                }
            }
        }
    }
}

/// The `SyncChanged` push for `fs` — a folder nudge, or a scope-tagged plane
/// nudge when `scope` names one.
fn sync_changed_payload(
    fs: &crate::db::FolderRow,
    scope: Option<&str>,
) -> fauna_protocol::PushEvent {
    fauna_protocol::PushEvent::SyncChanged(fauna_protocol::push_events::SyncChangedPayload {
        folder: fs.name.clone(),
        // A folder nudge carries the set's hash address — the one address
        // a sealed set's nudge will have once its plaintext name leaves
        // the row; a scope-tagged plane nudge is addressed by its scope.
        folder_hash: match scope {
            None => fs.name_hash.clone().map(serde_bytes::ByteBuf::from),
            Some(_) => None,
        },
        scope: scope.map(str::to_string),
        ..Default::default()
    })
}

/// [`notify_sync_changed`] to the set's **owner** alone — a third-party
/// deposit just parked in its inbox (`file-sync.md` § Third-party deposit
/// ingress), which only the owner's seats can adopt, so a member learns
/// nothing of it until the adopted file's own record nudges everyone.
pub(crate) fn notify_owner_sync_changed(state: &AppState, fs: &crate::db::FolderRow) {
    if let Ok(owner) = <[u8; 32]>::try_from(fs.actor_id.as_slice()) {
        state.ws.notify_push(&owner, sync_changed_payload(fs, None));
    }
}

// ── fauna.sync.changes.supersede ──────────────────────────────────────────────

/// Mark one path's pre-head manifest rows superseded so nest GC reclaims their
/// now-unreferenced chunks — the destructive half of the M2 pre-bind re-seal
/// migration (`mls-group-key-material.md` § M2 bullet B). The caller supplies
/// the head manifest it has **verified retrievable + decryptable end-to-end**;
/// the DB marks older rows only if that hash equals the live head, so the head
/// is structurally unmarkable and a stale verify can't unpin live data
/// (`webdav-server.md` § Architectural rules "deletes nothing until verified"),
/// and only if the row directly below the head carries the manifest the
/// caller's re-seal sourced from, so a stale reissue can't bury a newer edit.
/// Owner-only + write-capable device (the `changes.record` gates): supersede is
/// a write, and the owner is the sole writer of a shared set.
fn changes_supersede_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.changes.supersede").await?;
            let req: SyncChangesSupersedeRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;

            // Same device write-capability gate as `changes.record` (scoped to
            // the connection actor — `device_id` is attacker-controllable).
            match state
                .db
                .get_device_capabilities_for_actor(&actor_id, &device_id)
                .await
                .map_err(internal)?
            {
                Some(caps) if caps.contains("write") => {}
                Some(_) => {
                    return Err(coded("permission_denied", "device lacks write capability"));
                }
                None => return Err(coded("device_unregistered", "device not registered")),
            }

            let fs = owned_folder(&state, &actor_id, &req).await?;

            // A reserved (`__*`) backup destination set never enters
            // `sync_changes` — its custody projection is latest-per-path
            // already, superseding on upsert. Ordinary Backup folders ride
            // the head feed since the phase 3 head unification (2026-08-17)
            // and supersede like every other folder.
            if crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
                return Err(coded(
                    "invalid_request",
                    "reserved backup destination sets supersede on custody upsert; nothing to mark",
                ));
            }

            let path_hash: [u8; 32] = fauna_core::sync::path_hash(&req.path);
            let manifest_hash = parse_device_id(&req.manifest_hash)
                .ok_or_else(|| coded("invalid_request", "invalid manifest_hash hex"))?;
            let source_manifest_hash = req
                .source_manifest_hash
                .as_deref()
                .map(|h| {
                    parse_device_id(h)
                        .ok_or_else(|| coded("invalid_request", "invalid source_manifest_hash hex"))
                })
                .transpose()?;

            match state
                .db
                .supersede_sync_changes_for_path(
                    fs.id,
                    &path_hash,
                    &manifest_hash,
                    source_manifest_hash.as_ref(),
                )
                .await
                .map_err(internal)?
            {
                crate::db::sync_storage::SupersedeOutcome::Marked(superseded) => {
                    encode_reply(&SyncChangesSupersedeReply {
                        superseded,
                        extra: Default::default(),
                    })
                }
                // Retryable, not a fault: the head moved under a concurrent
                // record (or the caller verified a stale copy). The client
                // re-verifies against the new head on its next pass.
                crate::db::sync_storage::SupersedeOutcome::HeadMismatch => Err(coded(
                    "supersede_head_mismatch",
                    "manifest is not the path's live head; re-verify and retry",
                )),
                // Deferred, not a fault: the head is not a reissue of the row
                // directly below it (a stale re-seal recorded after a newer
                // edit, or a caller that named no source). Marking would bury
                // the newer edit, so nothing is marked; the client logs and
                // defers exactly as for a head mismatch.
                crate::db::sync_storage::SupersedeOutcome::SourceNotHead => Err(coded(
                    "supersede_source_not_head",
                    "the head is not a reissue of the row below it; nothing marked",
                )),
            }
        })
    })
}

// ── fauna.sync.backup_status (≡ GET /api/v1/sync/backup-status) ──────────────

fn backup_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.backup_status").await?;
            let _req: SyncBackupStatusRequest = decode(&payload).map_err(malformed)?;

            let sets = state
                .db
                .get_folders_for_actor(&actor_id)
                .await
                .map_err(internal)?;

            encode_reply(&SyncBackupStatusReply {
                folders: sets
                    .into_iter()
                    .map(|(label, last_change_at)| BackupStatusEntry {
                        name: label.name,
                        name_hash: label.name_hash.map(fauna_protocol::ByteBuf::from),
                        name_sealed: label.name_sealed.map(fauna_protocol::ByteBuf::from),
                        last_change_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.status (≡ GET /api/v1/sync/status) ────────────────────────────

fn status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.status").await?;
            let req: SyncStatusRequest = decode(&payload).map_err(malformed)?;

            // Ownership check (the twin took `_bearer` and skipped it — see the
            // module docs' latent-authorization fix). `sync.status` exposes the
            // owner's sync topology (devices / destinations) and stays owner-only
            // even for a shared set — members read content, not topology. Reuse
            // the owner-scoped row rather than re-fetching name-only (which could
            // resolve a same-named row owned by another user).
            let fs = owned_folder(&state, &actor_id, &req).await?;

            // The folder's content reachability — the same verdict
            // `fauna.media.list` stamps on each item (`file-sync.md`
            // § Content reachability).
            let source_online = crate::chunk_relay::folder_content_reachable(
                &state.ws,
                state.sync.chunk_resolver.foreign_seats(),
                &fs,
            );

            encode_reply(&SyncStatusReply {
                folder: req.folder,
                source_online,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.files (≡ GET /api/v1/sync/files) ──────────────────────────────

fn files_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.files").await?;
            let req: SyncFilesRequest = decode(&payload).map_err(malformed)?;

            // Read (S2-P3): the owner OR a member of a group-bound shared set.
            let fs = readable_folder(&state, &actor_id, &req).await?;
            let files = state
                .db
                .get_files_for_folder(fs.id)
                .await
                .map_err(internal)?;

            encode_reply(&SyncFilesReply {
                files: files
                    .iter()
                    .map(|f| SyncFile {
                        // Empty string = the ratified scrub sentinel on this
                        // required wire field (readers render `path_sealed`).
                        path: f.path.clone().unwrap_or_default(),
                        manifest_hash: hex::encode(&f.manifest_hash),
                        size_bytes: f.size_bytes,
                        updated_at: f.updated_at,
                        path_sealed: f.path_sealed.clone().map(fauna_protocol::ByteBuf::from),
                        path_hash: Some(fauna_protocol::ByteBuf::from(f.path_hash.clone())),
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.devices.list (≡ GET /api/v1/sync/devices) ─────────────────────

fn devices_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.devices.list").await?;
            let _req: SyncDevicesListRequest = decode(&payload).map_err(malformed)?;

            let devices = state
                .db
                .list_devices_for_actor(&actor_id)
                .await
                .map_err(internal)?;

            let mut items = Vec::with_capacity(devices.len());
            for device in &devices {
                let places = state
                    .db
                    .get_device_folder_places(&device.device_id, &actor_id)
                    .await
                    .map_err(internal)?;
                // `online` = a WS-RPC connection bound to the device is open
                // (`devices.md` § Listing Devices → *The binding*): one
                // upgraded with a bearer the row's granted key minted, which
                // is what every native app seat and per-user sync agent
                // holds — or, for web's one seed-minted socket, one bound in
                // band by `fauna.auth.device_bind`. A row
                // with no principal (never enrolled, or its grant retired)
                // has nothing to bind to and reads offline.
                let online = device
                    .principal
                    .as_deref()
                    .and_then(|key| <[u8; 32]>::try_from(key).ok())
                    .is_some_and(|key| state.ws.has_connection_bound_to(&actor_id, &key));
                items.push(SyncDevice {
                    device_id: hex::encode(&device.device_id),
                    label: device.label.clone(),
                    // Forwarded verbatim, ungated: `list_devices_for_actor` is
                    // `WHERE actor_id = ?1`, so this reader is by SQL the
                    // registering owner and therefore always the seal's
                    // audience. No `FolderReadGrant` projection applies —
                    // there is no non-audience arm to project against, unlike
                    // the `media.list` / snapshot planes (path-sealing S5d/S5e).
                    label_sealed: device
                        .label_sealed
                        .clone()
                        .map(fauna_protocol::ByteBuf::from),
                    capabilities: device.capabilities.clone(),
                    registered_at: device.registered_at,
                    last_seen_at: device.last_seen,
                    online,
                    principal: device.principal.as_deref().map(hex::encode),
                    // The ward sees which device their guardian enrolled — the
                    // pattern is transparent by construction (family-safety.md
                    // § Full visibility). Always false on an unsupervised
                    // account: nothing can set the flag without a link.
                    guardian_marked: device.guardian_marked,
                    // The device's own word and a sibling's pending brake
                    // (`p2p.md` § Per-device participation) — what a
                    // sibling's `device-p2p-participation-toggle` paints.
                    p2p_participation: device.p2p_participation,
                    p2p_off_requested: device.p2p_off_requested,
                    folders: places
                        .into_iter()
                        .map(|(label, flags)| DeviceFolderRole {
                            name: label.name,
                            name_hash: label.name_hash.map(fauna_protocol::ByteBuf::from),
                            name_sealed: label.name_sealed.map(fauna_protocol::ByteBuf::from),
                            flags,
                            extra: Default::default(),
                        })
                        .collect(),
                    extra: Default::default(),
                });
            }

            encode_reply(&SyncDevicesListReply {
                devices: items,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.devices.p2p_participation.set ─────────────────────────────────

/// The devices page's `device-p2p-participation-toggle`
/// (`docs/goal/behavior/p2p.md` § Per-device participation). Two arms, the
/// `fauna.sync.device_grant.revoke` shape (`require_pop_triple_or_none`):
///
/// - **Owner arm** (the account session alone): may only ask a device to
///   turn its peer listeners OFF — raises the row's pending brake and nothing
///   else. `participating: true` is refused: enabling is local consent on
///   the device itself, and no nest state may bring a listener up.
/// - **Self arm** (the proof-of-possession triple, signed by the row's own
///   principal — `sync_devices.auth_device_key` — over
///   `device_p2p_participation_signed_message`): the device reporting its
///   own state. A row that carries no principal cannot self-report.
///
/// The ownership check runs first, so a stranger probing another account's
/// device id learns only `not_found`.
fn devices_p2p_participation_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                fauna_protocol::sync::KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
            )
            .await?;
            let req: SyncDeviceP2pParticipationSetRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;
            let device = state
                .db
                .get_device_for_user(&device_id, &actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| coded("not_found", "device not found"))?;

            match require_pop_triple_or_none(
                req.timestamp_ms,
                req.nonce.as_deref(),
                req.signature.as_deref(),
            )? {
                // The self arm: the row's own principal proves possession over
                // the row AND the verdict, so a captured report can neither be
                // re-aimed nor flipped.
                Some((timestamp_ms, nonce_hex, signature_hex)) => {
                    let principal = device
                        .principal
                        .as_deref()
                        .and_then(|k| <[u8; 32]>::try_from(k).ok())
                        .ok_or_else(|| {
                            coded(
                                "permission_denied",
                                "this device row carries no principal to self-report with",
                            )
                        })?;
                    let participating = req.participating;
                    verify_device_key_pop(
                        &state,
                        &principal,
                        timestamp_ms,
                        nonce_hex,
                        signature_hex,
                        &|nonce| {
                            fauna_protocol::auth::device_p2p_participation_signed_message(
                                &actor_id,
                                &principal,
                                &device_id,
                                participating,
                                timestamp_ms,
                                nonce,
                            )
                        },
                    )
                    .await?;
                    state
                        .db
                        .set_device_p2p_participation(&actor_id, &device_id, participating)
                        .await
                        .map_err(internal)?;
                }
                // The owner arm: brake only.
                None => {
                    if req.participating {
                        return Err(coded(
                            "permission_denied",
                            "peer transfers can only be turned on from the device itself",
                        ));
                    }
                    state
                        .db
                        .request_device_p2p_off(&actor_id, &device_id)
                        .await
                        .map_err(internal)?;
                }
            }

            let after = state
                .db
                .get_device_for_user(&device_id, &actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| coded("not_found", "device not found"))?;
            encode_reply(&SyncDeviceP2pParticipationSetReply {
                participating: after.p2p_participation,
                off_requested: after.p2p_off_requested,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.sync.devices.delete (≡ DELETE /api/v1/sync/devices/{id}) ───────────

fn devices_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.sync.devices.delete").await?;
            let req: SyncDeviceDeleteRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;

            // Ownership check.
            let device = state
                .db
                .get_device_for_user(&device_id, &actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| coded("not_found", "device not found"))?;

            // The guardian-enrolled-device marker (family-safety.md § Full
            // visibility): a supervised account cannot remove the device its
            // guardian enrolled — that device authenticates as this very
            // account, so this refusal is the only thing standing between the
            // child and unilaterally ending their own supervision. The guardian
            // un-enrolls by unmarking first (`fauna.family.device.mark`), and
            // graduation revokes it automatically.
            //
            // The predicate is `marked AND currently supervised`, never the flag
            // alone (rule a): a mark that outlives the link must be inert, or a
            // crash could strand a now-full account with an undeletable device
            // (nest/common.md § Client-state recoverability). It sits after the
            // ownership check — a stranger probing another account's device id
            // still learns only `not_found`.
            if device.guardian_marked
                && state
                    .db
                    .get_guardian_of(&actor_id)
                    .await
                    .map_err(internal)?
                    .is_some()
            {
                return Err(coded(
                    "guardian_marked",
                    "this device was enrolled by your guardian and cannot be removed",
                ));
            }

            // No sole-source refusal: the nest holds the head, so a folder no
            // device feeds is an ordinary nest-held folder and removing a
            // device's places loses nothing (`devices.md`, the removal checks).

            match state
                .db
                .delete_device(&device_id, &actor_id)
                .await
                .map_err(internal)?
            {
                (true, members_removed, revoked_key) => {
                    // Both halves of revocation: every session this device's
                    // grant minted dies with the row, not at its own 1 h
                    // expiry, and every socket those sessions already opened
                    // closes 4401 — otherwise the thief the deletion targets
                    // keeps a live full-actor bearer, or a live `User`-class
                    // socket, and can re-establish (sync-agent.md § Credential
                    // model — device revocation). In-memory and after the DB
                    // commit: a crash here is process death, which empties the
                    // token store and drops every socket anyway.
                    if let Some(key) = revoked_key {
                        state.revoke_device_authority(&actor_id, &key).await;
                    }
                    encode_reply(&SyncDeviceDeleteReply {
                        deleted: true,
                        folders_removed_from: members_removed,
                        extra: Default::default(),
                    })
                }
                (false, _, _) => Err(coded("not_found", "device not found")),
            }
        })
    })
}

// ── fauna.sync.devices.rename ────────────────────────────────────────────────

/// `devices.md` § Device names, rule (1): the owner names one of their own
/// rows. Writes the label pair only (`rename_sync_device`) — never the
/// register upsert, which also rewrites `capabilities` and `registered_at`.
fn devices_rename_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                fauna_protocol::sync::KIND_SYNC_DEVICES_RENAME,
            )
            .await?;
            let req: SyncDeviceRenameRequest = decode(&payload).map_err(malformed)?;

            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;
            // The two nest-authored pseudo-devices are never the user's to
            // name — `register_handler` refuses them at its door the same way.
            if device_id == fauna_core::label_custody::webdav_pseudo_device_id(&actor_id)
                || device_id == fauna_core::label_custody::reseed_pseudo_device_id(&actor_id)
            {
                return Err(coded(
                    "invalid_request",
                    "device_id is reserved for a nest-authored pseudo-device",
                ));
            }
            // The nest holds no key, so it cannot open the seal — but it can
            // refuse bytes that are not a sealed-label envelope at all, which
            // would otherwise rest as a name no app can render.
            if fauna_core::path_crypto::SealedLabel::from_bytes(&req.label_sealed).is_err() {
                return Err(coded(
                    "invalid_request",
                    "label_sealed is not a sealed-label envelope",
                ));
            }

            // Actor-scoped: another actor's row matches nothing and answers
            // `not_found`, exactly as `devices.delete`'s ownership check does.
            if state
                .db
                .rename_sync_device(&actor_id, &device_id, &req.label_sealed)
                .await
                .map_err(internal)?
            {
                encode_reply(&SyncDeviceRenameReply::default())
            } else {
                Err(coded("not_found", "device not found"))
            }
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

// ── fauna.sync.serve.announce (relay serving, ruled 2026-10-01) ─────────────

/// `file-sync.md` § Relay serving, step (1): admit the folders a process
/// serves relay reads for, and keep them on the connection the announce rode
/// in on.
///
/// The device must be one of the caller's registered devices. Each folder is a
/// `FolderRef`; a ref is a ROW, so the gate is the row-level reader's —
/// `folder_authz::can_read_folder` on the row the ref names, by its owner and
/// member arms alone — never the by-name resolver, which would resolve a
/// member's own same-named folder first, and never the admin discovery grant,
/// which holds no key and so no bodies to serve. A ref that is not admitted is
/// left out of the reply rather than refused: the announce says what the
/// process holds, and the nest keeps what it may ask. More refs than
/// [`fauna_protocol::sync::SERVE_ANNOUNCE_MAX_FOLDERS`] is refused whole and
/// replaces nothing.
fn serve_announce_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            use crate::folder_authz::FolderReadGrant;
            use fauna_core::folder_keys::FolderRef;
            use fauna_protocol::sync::{
                KIND_SYNC_SERVE_ANNOUNCE, SERVE_ANNOUNCE_MAX_FOLDERS, SyncServeAnnounceReply,
                SyncServeAnnounceRequest,
            };

            require_permission(&state, &actor_id, KIND_SYNC_SERVE_ANNOUNCE).await?;
            let req: SyncServeAnnounceRequest = decode(&payload).map_err(malformed)?;

            if req.folders.len() + req.foreign.len() > SERVE_ANNOUNCE_MAX_FOLDERS {
                return Err(coded(
                    "invalid_request",
                    format!("an announce names at most {SERVE_ANNOUNCE_MAX_FOLDERS} folders"),
                ));
            }
            let device_id = parse_device_id(&req.device_id)
                .ok_or_else(|| coded("invalid_request", "invalid device_id hex"))?;
            state
                .db
                .get_device_for_user(&device_id, &actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| coded("not_found", "device not found"))?;

            let mut folder_ids: Vec<i64> = Vec::new();
            let mut admitted = Vec::new();
            for wire in &req.folders {
                // A foreign set has no row here; its seat announces through
                // its own nest (§ Relay serving, the cross-nest leg).
                let Some(FolderRef::Local(id)) = FolderRef::parse(wire) else {
                    continue;
                };
                if folder_ids.contains(&id) {
                    continue;
                }
                let Some(row) = state.db.get_folder_by_id(id).await.map_err(internal)? else {
                    continue;
                };
                let grant = crate::folder_authz::can_read_folder(&state.db, &row, &actor_id)
                    .await
                    .map_err(internal)?;
                if matches!(
                    grant,
                    Some(FolderReadGrant::Owner | FolderReadGrant::Member)
                ) {
                    folder_ids.push(id);
                    admitted.push(FolderRef::Local(id).to_wire());
                }
            }

            // The state lives on the connection the request rode in on, which
            // only the dispatch core knows: the per-actor plane always sets it.
            let conn_id = crate::dispatch_core::current_caller()
                .map(|c| c.conn_id)
                .ok_or_else(|| internal("serve.announce called outside a client connection"))?;

            // Foreign folders (§ Relay serving → *A member on another nest*,
            // step (2)): this nest checks only that the device is the
            // caller's — done above — and forwards; the home nest gates.
            let foreign =
                forward_foreign_announces(&state, &actor_id, &device_id, &req.foreign).await;
            admitted.extend(
                foreign
                    .iter()
                    .map(|(f, _)| FolderRef::Foreign(f.channel_id).to_wire()),
            );
            let renewal = foreign_renewal_period(foreign.iter().map(|(_, l)| *l));
            let foreign: Vec<crate::ws::ForeignServing> =
                foreign.into_iter().map(|(f, _)| f).collect();

            // A socket already gone has nothing left to serve from; its
            // announce ended with it, which is the answer the relay needs —
            // and the home nests are told so at once.
            let Some((conn, previous)) = state.ws.replace_serving(
                &actor_id,
                conn_id,
                crate::ws::ServingAnnounce {
                    device_id,
                    folder_ids,
                    foreign: foreign.clone(),
                },
            ) else {
                withdraw_foreign(&state, &actor_id, &device_id, foreign);
                return encode_reply(&SyncServeAnnounceReply {
                    admitted,
                    extra: Default::default(),
                });
            };
            // This announce takes the renewing over; whatever the last one
            // admitted and this one does not is no longer served.
            if let Some(old) = conn.foreign_renewal.lock().unwrap().take() {
                old.cancel();
            }
            if let Some(previous) = previous {
                let dropped: Vec<_> = previous
                    .foreign
                    .into_iter()
                    .filter(|f| {
                        previous.device_id != device_id
                            || !foreign.iter().any(|n| n.channel_id == f.channel_id)
                    })
                    .collect();
                withdraw_foreign(&state, &actor_id, &previous.device_id, dropped);
            }
            if let Some(period) = renewal {
                let cancel = tokio_util::sync::CancellationToken::new();
                *conn.foreign_renewal.lock().unwrap() = Some(cancel.clone());
                // spawn-ok(connection-scoped): ends with the connection (revoked or closed) or the next announce's cancel
                tokio::spawn(keep_foreign_seats(
                    std::sync::Arc::clone(&state),
                    conn,
                    actor_id,
                    device_id,
                    foreign,
                    period,
                    cancel,
                ));
            }
            encode_reply(&SyncServeAnnounceReply {
                admitted,
                extra: Default::default(),
            })
        })
    })
}

/// The most distinct home nests one announce forwards to.
/// A member's foreign folders live on the few nests its shares came from;
/// entries naming a home nest past this many are left out of the reply, so an
/// announce cannot aim the nest at hundreds of hosts of the caller's choosing
/// (`deployment-threat-model.md` concern 2, item 3).
pub const SERVE_ANNOUNCE_MAX_FOREIGN_NESTS: usize = 8;

/// The per-actor meter on forwarded home nests: one event per distinct home
/// nest an announce dials, 32 over ten minutes — a reconnecting device with
/// its shares on a few nests stays far inside it, while a caller rotating
/// hosts reaches at most 32 a window. An entry whose home nest the meter
/// refuses is left out of the reply, never failing the announce. A constant
/// (an abuse bound the binary sets, not an admin preference).
pub const FOREIGN_FORWARD_METER: crate::bridge_rate_limit::LimiterConfig =
    crate::bridge_rate_limit::LimiterConfig {
        window: std::time::Duration::from_secs(600),
        max_events: 32,
    };

/// The meter's bucket key (the limiter's credential axis).
const FOREIGN_FORWARD_BUCKET: &str = "serve_announce_forward";

/// The shortest lease a home nest's reply is believed at:
/// the member's nest renews every entry at half the shortest lease, so a
/// hostile home nest answering `1` would have it renew every honest nest's
/// seat each second. Half of it is the fastest renewal any reply can cause.
pub const FOREIGN_LEASE_FLOOR_SECS: u64 = 60;

/// A home nest's granted lease, clamped to `[FOREIGN_LEASE_FLOOR_SECS,
/// FOREIGN_SEAT_LEASE]`: below the floor would renew
/// every entry too often; above the hard-coded lease would stretch the
/// renewal period past what honest home nests grant, so their seats lapse.
pub fn clamp_peer_lease(lease_secs: u64) -> u64 {
    lease_secs.clamp(
        FOREIGN_LEASE_FLOOR_SECS,
        crate::chunk_relay::FOREIGN_SEAT_LEASE.as_secs(),
    )
}

/// The renewal period's guard, in a module of its own so its type cannot be
/// built anywhere else: [`keep_foreign_seats`] demands a [`RenewalPeriod`] and
/// only [`foreign_renewal_period`] mints one, so a period computed from the raw
/// leases does not compile (`review-method.md` § *A guard
/// whose failure is silent lives inside the consumer, not the callers*).
mod renewal {
    /// How often a connection renews its foreign seats — always half of a
    /// clamped lease. The field is private to this module.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct RenewalPeriod(std::time::Duration);

    impl RenewalPeriod {
        pub(super) fn get(self) -> std::time::Duration {
            self.0
        }
    }

    /// How often a connection renews its foreign seats, from the leases the
    /// home nests granted: half the shortest, each clamped first
    /// ([`super::clamp_peer_lease`]), so no reply can make the nest renew
    /// faster than every `FOREIGN_LEASE_FLOOR_SECS / 2` or slower than an
    /// honest seat's lease allows. `None` when nothing was admitted. The one
    /// place a [`RenewalPeriod`] comes from.
    pub(super) fn foreign_renewal_period(
        leases: impl IntoIterator<Item = u64>,
    ) -> Option<RenewalPeriod> {
        leases
            .into_iter()
            .map(super::clamp_peer_lease)
            .min()
            .map(|l| RenewalPeriod(std::time::Duration::from_secs(l / 2)))
    }
}
use renewal::{RenewalPeriod, foreign_renewal_period};

/// The foreign entries an announce forwards, by home nest: each channel once,
/// a non-foreign ref or an empty URL left out, at most
/// [`SERVE_ANNOUNCE_MAX_FOREIGN_NESTS`] distinct home nests in the order they
/// are first named, and only the home nests `meter` admits for `actor_id` —
/// each spends one event, reached or not.
fn select_foreign_forwards(
    meter: &crate::bridge_rate_limit::Limiter,
    actor_id: &[u8; 32],
    entries: &[fauna_protocol::sync::SyncServeForeignEntry],
) -> Vec<(String, Vec<[u8; 32]>)> {
    use fauna_core::folder_keys::FolderRef;

    let mut seen: Vec<[u8; 32]> = Vec::new();
    let mut by_nest: Vec<(String, Vec<[u8; 32]>)> = Vec::new();
    for e in entries {
        let Some(FolderRef::Foreign(channel_id)) = FolderRef::parse(&e.folder) else {
            continue;
        };
        if e.nest_url.is_empty() || seen.contains(&channel_id) {
            continue;
        }
        if let Some((_, channels)) = by_nest.iter_mut().find(|(u, _)| *u == e.nest_url) {
            seen.push(channel_id);
            channels.push(channel_id);
            continue;
        }
        if by_nest.len() >= SERVE_ANNOUNCE_MAX_FOREIGN_NESTS {
            continue;
        }
        seen.push(channel_id);
        by_nest.push((e.nest_url.clone(), vec![channel_id]));
    }
    by_nest
        .into_iter()
        .filter(|_| meter.check(actor_id, actor_id, FOREIGN_FORWARD_BUCKET))
        .collect()
}

/// Forward each foreign entry of an announce to its home nest
/// (`fauna.federation.folder.serve.announce`, serving) and keep the ones it
/// admitted, each once, with the lease it granted. The home nests are bounded
/// and metered ([`select_foreign_forwards`]) and dialed concurrently; one
/// home nest's entries go in order, and a home nest that is not reached at
/// all is not dialed again for the rest of them. A refusal, an unknown kind,
/// an unparseable or non-foreign ref and a transport error alike leave the
/// entry out — never failing the rest of the announce.
async fn forward_foreign_announces(
    state: &std::sync::Arc<AppState>,
    actor_id: &[u8; 32],
    device_id: &[u8; 32],
    entries: &[fauna_protocol::sync::SyncServeForeignEntry],
) -> Vec<(crate::ws::ForeignServing, u64)> {
    let wanted = select_foreign_forwards(&state.sync.foreign_forward_meter, actor_id, entries);
    let actor_hex = hex::encode(actor_id);
    let device_hex = hex::encode(device_id);
    let forwards = wanted.into_iter().map(|(nest_url, channels)| {
        let actor_hex = &actor_hex;
        let device_hex = &device_hex;
        async move {
            let mut admitted = Vec::new();
            for channel_id in channels {
                match crate::federation_pool::originate_folder_serve_announce(
                    &state.federation_pool,
                    state,
                    &nest_url,
                    actor_hex,
                    &hex::encode(channel_id),
                    device_hex,
                    true,
                )
                .await
                {
                    Ok(Ok((lease_secs, home_nest_id))) if lease_secs > 0 => admitted.push((
                        crate::ws::ForeignServing {
                            channel_id,
                            nest_url: nest_url.clone(),
                            home_nest_id,
                        },
                        lease_secs,
                    )),
                    Ok(Ok(_)) => {}
                    Ok(Err(refused)) => {
                        tracing::debug!(
                            "serve.announce: the home nest admitted no seat: {}",
                            refused.code
                        );
                    }
                    Err(e) => {
                        tracing::debug!("serve.announce: the home nest was not reached: {e}");
                        break;
                    }
                }
            }
            admitted
        }
    });
    futures_util::future::join_all(forwards)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Tell each entry's home nest the seat no longer serves it — in the
/// background, since the reply never waits on it and a home nest that is not
/// reached lets the lease lapse by itself.
fn withdraw_foreign(
    state: &std::sync::Arc<AppState>,
    actor_id: &[u8; 32],
    device_id: &[u8; 32],
    entries: Vec<crate::ws::ForeignServing>,
) {
    if entries.is_empty() {
        return;
    }
    let state = std::sync::Arc::clone(state);
    let actor_hex = hex::encode(actor_id);
    let device_hex = hex::encode(device_id);
    // spawn-ok(request-scoped): one best-effort withdraw pass, bounded by the federation pool's per-request timeout; holds no key material
    tokio::spawn(async move {
        for f in entries {
            let _ = crate::federation_pool::originate_folder_serve_announce(
                &state.federation_pool,
                &state,
                &f.nest_url,
                &actor_hex,
                &hex::encode(f.channel_id),
                &device_hex,
                false,
            )
            .await;
        }
    });
}

/// Keep a connection's foreign seats leased: renew each every `period` (half
/// the shortest lease the home nests granted, [`foreign_renewal_period`])
/// while the connection stands, and say *no longer serving*
/// when it closes or is revoked (§ Relay serving → *A member on another nest*,
/// step (2)). Cancelled by the next announce on the connection, which withdraws
/// what it drops and renews the rest itself.
async fn keep_foreign_seats(
    state: std::sync::Arc<AppState>,
    conn: std::sync::Arc<crate::ws::RpcConnection>,
    actor_id: [u8; 32],
    device_id: [u8; 32],
    entries: Vec<crate::ws::ForeignServing>,
    period: RenewalPeriod,
    cancel: tokio_util::sync::CancellationToken,
) {
    let period = period.get();
    let mut revoked = conn.subscribe_revoked();
    let actor_hex = hex::encode(actor_id);
    let device_hex = hex::encode(device_id);
    loop {
        if conn.is_revoked() {
            break;
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = conn.ws_tx.closed() => break,
            _ = revoked.changed() => continue,
            () = tokio::time::sleep(period) => {
                for f in &entries {
                    let _ = crate::federation_pool::originate_folder_serve_announce(
                        &state.federation_pool,
                        &state,
                        &f.nest_url,
                        &actor_hex,
                        &hex::encode(f.channel_id),
                        &device_hex,
                        true,
                    )
                    .await;
                }
            }
        }
    }
    if !cancel.is_cancelled() {
        withdraw_foreign(&state, &actor_id, &device_id, entries);
    }
}

/// Register the device-sync control surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_sync_kinds`. Every
/// kind below is `forbid_replay = false` @5 s (reads + fast local DB
/// mutations) — including `device_grant.revoke`, whose own single-use guard is
/// the replay guard its proof-of-possession signatures are recorded in, not the
/// router's.
pub fn register_sync_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        ("fauna.sync.register", register_handler()),
        (
            "fauna.sync.device_grant.register",
            device_grant_register_handler(),
        ),
        (
            "fauna.sync.device_grant.revoke",
            device_grant_revoke_handler(),
        ),
        ("fauna.sync.changes.list", changes_list_handler()),
        ("fauna.sync.changes.record", changes_record_handler()),
        ("fauna.sync.changes.supersede", changes_supersede_handler()),
        ("fauna.sync.status", status_handler()),
        ("fauna.sync.files", files_handler()),
        ("fauna.sync.backup_status", backup_status_handler()),
        ("fauna.sync.devices.list", devices_list_handler()),
        ("fauna.sync.devices.delete", devices_delete_handler()),
        (
            fauna_protocol::sync::KIND_SYNC_DEVICES_RENAME,
            devices_rename_handler(),
        ),
        (
            fauna_protocol::sync::KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
            devices_p2p_participation_set_handler(),
        ),
        (
            fauna_protocol::sync::KIND_SYNC_SERVE_ANNOUNCE,
            serve_announce_handler(),
        ),
        (
            fauna_protocol::account_state::KIND_STATE_PUT,
            account_state_put_handler(),
        ),
        (
            fauna_protocol::account_state::KIND_STATE_RETIRE,
            account_state_retire_handler(),
        ),
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
}

#[cfg(test)]
use bytes::Bytes;

#[cfg(test)]
mod device_grant_tests {
    //! `fauna.sync.device_grant.register` — the renewal-grant registration half
    //! (sync-agent.md § Credential model; the mint half's tests live in
    //! `auth_core::device_auth_tests`).
    use super::*;

    use std::sync::Arc;

    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
    use fauna_core::identity::{ActorId, ActorKeypair};
    use fauna_protocol::sync::DeviceGrantRegisterRequest;

    use crate::routes::AppState;

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    fn encode_req<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).expect("encode req").to_vec())
    }

    const DEVICE_ID: [u8; 32] = [0xD7u8; 32];

    /// User + registered sync device for `identity`.
    async fn seed_device(state: &std::sync::Arc<AppState>, identity: &ActorKeypair) {
        let actor = identity.actor_id().0;
        state.db.create_user(&actor, "free", "test").await.unwrap();
        state
            .db
            .register_sync_device(&actor, &DEVICE_ID, "agent", None, "read,write")
            .await
            .unwrap();
    }

    fn grant_request(
        identity: &ActorKeypair,
        renewal_pub: [u8; 32],
        capabilities: Vec<Capability>,
    ) -> DeviceGrantRegisterRequest {
        let auth = DeviceAuthorization {
            actor_id: ActorId(identity.actor_id().0),
            device_key: renewal_pub,
            capabilities,
            created_at: Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(identity, &auth).expect("sign grant");
        DeviceGrantRegisterRequest {
            device_id: hex::encode(DEVICE_ID),
            authorization: EmbedAsBytes::from_signed(bytes, env),
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn register_stores_a_verified_grant() {
        let state = fixture_state().await;
        let identity = ActorKeypair::generate();
        seed_device(&state, &identity).await;
        let renewal = ActorKeypair::generate();
        let req = grant_request(
            &identity,
            renewal.actor_id().0,
            vec![Capability::RenewBearer],
        );
        device_grant_register_handler()(state.clone(), identity.actor_id().0, encode_req(&req))
            .await
            .expect("register succeeds");
        assert!(
            state
                .db
                .get_sync_device_grant(&identity.actor_id().0, &renewal.actor_id().0)
                .await
                .unwrap()
                .is_some(),
            "grant stored on the device row"
        );
    }

    #[tokio::test]
    async fn foreign_grant_is_refused() {
        let state = fixture_state().await;
        let identity = ActorKeypair::generate();
        let intruder = ActorKeypair::generate();
        seed_device(&state, &identity).await;
        state
            .db
            .create_user(&intruder.actor_id().0, "free", "test")
            .await
            .unwrap();
        // The intruder tries to register a grant naming the victim's actor_id.
        let req = grant_request(&identity, [0x11; 32], vec![Capability::RenewBearer]);
        let err = device_grant_register_handler()(state, intruder.actor_id().0, encode_req(&req))
            .await
            .expect_err("foreign grant must refuse");
        assert_eq!(err.code, "fauna.sync.permission_denied");
    }

    #[tokio::test]
    async fn renewal_scope_is_required() {
        let state = fixture_state().await;
        let identity = ActorKeypair::generate();
        seed_device(&state, &identity).await;
        let req = grant_request(&identity, [0x22; 32], vec![Capability::Post]);
        let err = device_grant_register_handler()(state, identity.actor_id().0, encode_req(&req))
            .await
            .expect_err("a non-RenewBearer grant must refuse");
        assert_eq!(err.code, "fauna.sync.invalid_grant");
    }

    #[tokio::test]
    async fn unregistered_device_is_refused() {
        let state = fixture_state().await;
        let identity = ActorKeypair::generate();
        state
            .db
            .create_user(&identity.actor_id().0, "free", "test")
            .await
            .unwrap();
        // No fauna.sync.register first — the grant has no row to attach to.
        let req = grant_request(&identity, [0x33; 32], vec![Capability::RenewBearer]);
        let err = device_grant_register_handler()(state, identity.actor_id().0, encode_req(&req))
            .await
            .expect_err("grant without a device row must refuse");
        assert_eq!(err.code, "fauna.sync.not_found");
    }

    /// End-to-end over the two kinds: register the grant through the handler,
    /// then mint through `device_auth_core` — the production flow an agent
    /// takes (admin registers grant at provision → agent renews app-dead).
    #[tokio::test]
    async fn registered_grant_mints_via_device_handshake() {
        use ed25519_dalek::Signer;

        let state = fixture_state().await;
        let identity = ActorKeypair::generate();
        seed_device(&state, &identity).await;
        let renewal = ActorKeypair::generate();
        let req = grant_request(
            &identity,
            renewal.actor_id().0,
            vec![Capability::RenewBearer],
        );
        device_grant_register_handler()(state.clone(), identity.actor_id().0, encode_req(&req))
            .await
            .expect("register succeeds");

        let actor = identity.actor_id().0;
        let device_key = renewal.actor_id().0;
        let ts = fauna_core::data::Timestamp::now_millis();
        let nonce = b"e2e-nonce";
        let nest = state.bound_identity();
        let msg = fauna_protocol::auth::device_handshake_signed_message(
            &actor,
            &device_key,
            ts,
            &nest,
            nonce,
        );
        let sig = renewal.signing_key().sign(&msg);
        let mint = crate::auth_core::device_auth_core(
            &state,
            &hex::encode(actor),
            &hex::encode(device_key),
            ts,
            &hex::encode(sig.to_bytes()),
            nonce,
            &hex::encode(nest),
        )
        .await;
        assert!(mint.is_ok(), "registered grant mints a bearer end-to-end");
    }
}

#[cfg(test)]
mod sync_nudge_tests {
    //! `notify_sync_changed` — the same-nest download-half push nudge fired by
    //! `record_change_core` after every durable sync record (file-sync.md
    //! § Remote-change nudge). A `PushEvent::SyncChanged { folder_id }` reaches
    //! every connected same-nest participant (owner + roster members) so their
    //! engines pull immediately; an owner-only set nudges the owner's own
    //! devices. The download twin of the caldav `fauna.calendar.changed` tests.
    use super::*;
    use std::sync::Arc;

    use fauna_core::identity::ActorKeypair;

    /// The nonce every fixture set here is created under and every fixture
    /// record is signed under.
    const SET_NONCE: [u8; 32] = [0x5e; 32];
    const DEVICE: [u8; 32] = [0xd7; 32];
    // S9 flip: a sealless record refuses — seal the way a keyed engine would
    // (any opaque envelope satisfies the nest, which never opens it).
    const SEAL: &[u8] = b"nudge-test-seal";

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    /// Create `name` owned by `owner` under [`SET_NONCE`] — a set a signed
    /// record can bind to.
    async fn create_set(state: &AppState, name: &str, owner: &[u8; 32]) {
        state
            .db
            .create_folder_with_options(
                name,
                owner,
                crate::db::FolderOptions {
                    set_nonce: Some(SET_NONCE.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    /// Record a sealed `create` of `path` through `record_change_core`,
    /// signed directly by `recorder` under [`SET_NONCE`] — the record a
    /// writer engine sends.
    async fn record_signed(
        state: &AppState,
        recorder: &ActorKeypair,
        fs: &crate::db::FolderRow,
        path: &str,
        manifest_hex: &str,
        size_bytes: i64,
    ) -> Result<i64, RpcError> {
        let mut req = SyncChangeRecordRequest {
            folder: fs.name.clone(),
            device_id: hex::encode(DEVICE),
            path: path.into(),
            manifest_hash: Some(manifest_hex.into()),
            size_bytes,
            change_type: "create".into(),
            path_sealed: Some(fauna_protocol::ByteBuf::from(SEAL.to_vec())),
            ..Default::default()
        };
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(recorder)
            .sign_record(&mut req, SET_NONCE)
            .expect("the fixture record signs");
        record_change_core(
            state,
            &recorder.actor_id().0,
            fs,
            path,
            Some(manifest_hex),
            size_bytes,
            "create",
            None,
            None,
            &DEVICE,
            Some(SEAL),
            None,
            None,
            crate::change_signature::CarriedSignature::new(
                req.signature.as_ref(),
                req.signer_key.as_ref(),
            ),
            crate::change_signature::CertCarriage::ByReference,
        )
        .await
    }

    /// Receive exactly one `fauna.sync.changed` push (bounded) and assert it
    /// names `folder`.
    async fn expect_sync_changed(rx: &mut tokio::sync::mpsc::Receiver<Bytes>, folder: &str) {
        let bytes = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for fauna.sync.changed push")
            .expect("push channel closed");
        let frame = fauna_protocol::decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            fauna_protocol::Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = fauna_protocol::PushEvent::from_push(&push.kind, push.payload);
        match event {
            fauna_protocol::PushEvent::SyncChanged(p) => {
                assert_eq!(p.folder, folder);
                // The hash address rides every folder nudge — the one address
                // a sealed set's nudge keeps once its plaintext name leaves.
                assert!(p.folder_hash.is_some(), "a folder nudge carries its hash");
                assert!(p.names_set(folder), "the nudge's hash names {folder}");
            }
            other => panic!("expected SyncChanged push, got {}", other.kind()),
        }
    }

    /// A shared set: an owner's record nudges a roster MEMBER's live connection —
    /// this is the collaboration-latency case the nudge exists to fix.
    #[tokio::test]
    async fn shared_set_record_nudges_member() {
        let state = fixture_state().await;
        let owner_kp = ActorKeypair::from_secret([0xa1u8; 32]);
        let owner = owner_kp.actor_id().0;
        let member = [0xb2u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();

        let raw_group_id = vec![0x7cu8; 20];
        let channel = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        create_set(&state, "shared", &owner).await;
        state
            .db
            .set_folder_mls_group("shared", &owner, Some(&raw_group_id))
            .await
            .unwrap();
        // The roster the nest fans out over: owner + member.
        state
            .db
            .register_actor_channel(&owner, &channel)
            .await
            .unwrap();
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .unwrap();
        let fs = state
            .db
            .get_folder("shared")
            .await
            .unwrap()
            .expect("set exists");

        // A member device is live; the owner records a change.
        let (_conn, mut rx) = state.ws.subscribe(member);
        record_signed(&state, &owner_kp, &fs, "hello.txt", &"11".repeat(32), 10)
            .await
            .expect("record succeeds");

        expect_sync_changed(&mut rx, &fs.name).await;
    }

    /// An owner-only (unshared, no MLS group) set nudges the owner's OWN other
    /// devices — the multi-device single-user sync case.
    #[tokio::test]
    async fn owner_only_set_record_nudges_owner() {
        let state = fixture_state().await;
        let owner_kp = ActorKeypair::from_secret([0xc3u8; 32]);
        let owner = owner_kp.actor_id().0;
        state.db.create_user(&owner, "free", "test").await.unwrap();
        create_set(&state, "solo", &owner).await;
        let fs = state
            .db
            .get_folder("solo")
            .await
            .unwrap()
            .expect("set exists");
        assert!(fs.mls_group_id.is_none(), "owner-only set has no group");

        let (_conn, mut rx) = state.ws.subscribe(owner);
        record_signed(&state, &owner_kp, &fs, "notes.txt", &"22".repeat(32), 5)
            .await
            .expect("record succeeds");

        expect_sync_changed(&mut rx, &fs.name).await;
    }
}

#[cfg(test)]
mod feed_admission_tests {
    //! The coupling pin between `FEED_SERVED_KINDS` and
    //! [`admit_content_scope`] — the test the served-kind set's growth
    //! contract names: a kind must not join the feed without a ruled
    //! admission for its scope-id family.
    use super::*;
    use std::sync::Arc;

    use crate::db::CacheDb;
    use crate::segments::records_db::FEED_SERVED_KINDS;

    /// Every served kind's admission is RULED — a decided authorization
    /// refusal (`forbidden`) for a caller with no claim on the scope — never
    /// the fail-closed "no admission rule" arm. Growing `FEED_SERVED_KINDS`
    /// without an `admit_content_scope` arm turns exactly this test red
    /// (the new kind falls through to `invalid_request`), which is the
    /// red-first moment the constant's doc contract promises.
    #[tokio::test]
    async fn every_feed_served_kind_has_a_ruled_admission() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let state = AppState::for_test(db);
        // A caller with no claim: not the scope's actor, on no channel roster.
        let caller = [0xDDu8; 32];
        for kind in FEED_SERVED_KINDS {
            let scope = fauna_protocol::scope::ContentScope::new(kind, [0xEEu8; 32])
                .expect("served kinds are well-formed tags");
            let err = admit_content_scope(&state, &caller, &scope)
                .await
                .expect_err("an unentitled caller is refused for every kind");
            assert!(
                err.code == "fauna.sync.forbidden",
                "kind {kind:?}: admission must be a RULED authorization refusal \
                 (fauna.sync.forbidden), not the fail-closed unruled arm — got {}",
                err.code
            );
        }
    }

    /// The conv arm's Plan-9 leg: a MEMBER of a channel this nest holds only
    /// as an opaque backup is refused loudly — never admitted onto an empty
    /// mirror, which a walking replica would record as converged-empty forever
    /// (the exact misread charter ruling 4 forbids). Membership is checked
    /// first, so only a member can ever see this refusal.
    #[tokio::test]
    async fn a_members_conv_walk_on_a_pure_backup_destination_is_refused() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let state = AppState::for_test(db.clone());
        let member = [0xA7u8; 32];
        let channel = [0x6Eu8; 32];
        db.register_actor_channel(&member, &channel).await.unwrap();
        let scope = fauna_protocol::scope::ContentScope::new("conv", channel).unwrap();

        // Serving nest → admitted.
        admit_content_scope(&state, &member, &scope)
            .await
            .expect("a member is admitted on a serving nest");

        // The channel's `__conv/<hex>` reserved set flips to a custody copy
        // (the same arrangement `require_local_conv_serving_gate` pins for
        // `channel.fetch`) → refused, not answered empty.
        let name = format!("__conv/{}", hex::encode(channel));
        db.create_folder_with_options(
            &name,
            &channel,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let err = admit_content_scope(&state, &member, &scope)
            .await
            .expect_err("a pure-backup destination refuses the feed");
        assert_eq!(err.code, "fauna.sync.pure_backup_destination");
    }

    /// **What the conv arm's membership gate actually reaches** — measured, so
    /// no session re-derives it from the gate's name.
    ///
    /// `admit_content_scope`'s conv arm reads `actor_channels`
    /// (`is_actor_in_channel`), and that roster is *written for the caller* by
    /// the sibling conversation doors on any **unclaimed** channel — which is
    /// every DM / group / scheduling conversation (the claim-read gate gates
    /// only *claimed* folder channels). So the refusal an unentitled caller
    /// meets in [`every_feed_served_kind_has_a_ruled_admission`] is lifted by
    /// one prior `channel.send` on the same channel id — **even when that send
    /// is itself refused**, because the auto-register runs before ingest and is
    /// not rolled back.
    ///
    /// This is the ruled posture, not a regression: the owner doc
    /// (`account-sync-plane.md` § Implementation status today → *Built — conv
    /// on the content-scope feed…*)
    /// bounds this door strictly below `channel.fetch`, which already serves
    /// any authenticated local actor the channel's envelopes on the same
    /// precondition — knowing the channel id. What the test pins is the gate's
    /// **reach**: real against a passive never-admitted actor, ~zero against a
    /// deliberate one (the cost is one extra RPC). Read it before treating a
    /// conv-feed admission as evidence of channel membership.
    ///
    /// **The posture is design-accepted — do NOT re-file it as a finding.**
    /// the security review graded it a pre-existing residual and
    /// re-affirmed that, having enumerated this door together
    /// with `channel_actors_handler`. Both reached it by tracing this call
    /// graph, and that pass predicted the next turn would too — it did, which
    /// is why the fact is pinned here rather than left to a fourth
    /// re-derivation. Moving the gate to the (since-retired) `group_members`
    /// authority put on the custody arm was not a mechanical
    /// option: a DM had no `group_members` rows, so the swap failed DM channels
    /// closed.
    #[tokio::test]
    async fn the_conv_feed_gate_is_lifted_by_the_callers_own_refused_send() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let state = Arc::new(AppState::for_test(db.clone()));
        let stranger = [0x5Au8; 32];
        let channel = [0x6Fu8; 32]; // unclaimed — an ordinary conversation
        db.create_user(&stranger, "free", "test").await.unwrap();
        let scope = fauna_protocol::scope::ContentScope::new("conv", channel).unwrap();

        // The class gate is per-actor, never per-channel: an ordinary user
        // holds `channel.send` for every channel id there is.
        crate::bridge_method_allowlist::require_permission(
            &db,
            &stranger,
            "fauna.conversations.channel.send",
            internal,
        )
        .await
        .expect("an ordinary user's caller class permits channel.send");

        // Before: on no roster, so refused.
        let err = admit_content_scope(&state, &stranger, &scope)
            .await
            .expect_err("a caller who has never touched the channel is refused");
        assert_eq!(err.code, "fauna.sync.forbidden");

        // The lift: one send of a body that is not a channel envelope. Ingest
        // REFUSES it (`classify_envelope_shape`), so nothing is appended and
        // the caller learns nothing from the call itself — but
        // `register_actor_channel_gated` already ran, and an unclaimed channel
        // admits any caller's self-registration.
        crate::conversations_handlers::channel_send_core(
            &state,
            &stranger,
            &channel,
            bytes::Bytes::from_static(b"not a channel envelope"),
            false,
            None,
            &[],
        )
        .await
        .expect_err("the send itself is refused at ingest");
        assert!(
            db.is_actor_in_channel(&stranger, &channel).await.unwrap(),
            "the refused send still wrote the roster row the feed gate reads"
        );

        // After: the same caller, the same scope, now admitted.
        admit_content_scope(&state, &stranger, &scope)
            .await
            .expect("the caller's own refused send lifted the membership gate");
    }
}

#[cfg(test)]
mod changes_record_size_tests {
    //! The `fauna.sync.changes.record` door's half of the negative-size
    //! refusal: the metering core owns the rule (all five record doors funnel
    //! there), and this pins the code a client actually sees.
    use super::*;
    use std::sync::Arc;

    use crate::db::CacheDb;

    /// A negative declared `size_bytes` comes back typed as
    /// `fauna.sync.invalid_size`, and nothing is metered. Without the core's
    /// guard this door answers `Ok(seq)` and *credits* the account — the
    /// unbounded quota evasion pinned by
    /// `db::sync_storage::tests::a_negative_declared_size_is_refused_and_credits_nothing`.
    #[tokio::test]
    async fn the_record_door_refuses_a_negative_size() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let state = Arc::new(AppState::for_test(db));
        // A real key, and a set with a stored nonce: the record is signed the
        // way a writer engine signs it, so it clears the `signature_required`
        // gate and reaches the metering core this test is about.
        let actor_kp = fauna_core::identity::ActorKeypair::from_secret([0x44u8; 32]);
        let actor = actor_kp.actor_id().0;
        let set_nonce = [0x5eu8; 32];
        let device = [0xD4u8; 32];
        state.db.create_user(&actor, "free", "owner").await.unwrap();
        state
            .db
            .create_folder_with_options(
                "photos",
                &actor,
                crate::db::FolderOptions {
                    set_nonce: Some(set_nonce.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        state
            .db
            .register_sync_device(&actor, &device, "laptop", None, "read,write")
            .await
            .unwrap();

        let mut req = SyncChangeRecordRequest {
            folder: "photos".into(),
            device_id: hex::encode(device),
            path: "a.txt".into(),
            manifest_hash: Some(hex::encode([0xC1u8; 32])),
            size_bytes: -1_000_000,
            change_type: "create".into(),
            // Present so the S9 seal check passes and the record reaches the
            // metering core — this test is about the size, not the seal.
            path_sealed: Some(fauna_protocol::ByteBuf::from(vec![0xA0u8; 8])),
            ..Default::default()
        };
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(&actor_kp)
            .sign_record(&mut req, set_nonce)
            .expect("the fixture record signs");
        let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
        let err = changes_record_handler()(state.clone(), actor, payload)
            .await
            .expect_err("a negative size must be refused");
        assert_eq!(err.code, "fauna.sync.invalid_size");

        assert_eq!(
            state
                .db
                .list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used,
            0,
            "the refused record credited nothing"
        );
        assert!(
            state
                .db
                .get_sync_changes(&actor, 0)
                .await
                .unwrap()
                .is_empty(),
            "and landed no row"
        );
    }
}

#[cfg(test)]
mod change_page_budget_tests {
    //! The change-row serve page fits the WS message cap the app's socket
    //! enforces (`transport.md` § Max frame). The class-2 account-state feed
    //! pages by bytes alone, so a page of many small entries is the case an
    //! estimated row cost breaks.
    use super::*;
    use crate::db::SyncChangeRow;

    fn state_entry(seq: i64, entry_len: usize) -> SyncChangeRow {
        SyncChangeRow {
            seq,
            path_hash: vec![0xab; 32],
            manifest_hash: None,
            size_bytes: 0,
            change_type: "put".into(),
            created_at: 1_790_000_000_000 + seq,
            path: None,
            device_id: Some(vec![0xcd; 32]),
            content_key_version: None,
            thumbnail_hash: None,
            actor_id: vec![0xef; 32],
            path_sealed: None,
            derived_through: Some(seq),
            is_resolution: None,
            is_retention: None,
            item_class: Some("state-entry".into()),
            origin_writer: Some(vec![0x12; 32]),
            origin_seq: Some(seq),
            entry_sealed: Some(vec![0x34; entry_len]),
            signature: Some(vec![0x56; 64]),
            signer_key: Some(vec![0x78; 32]),
            deposited_by: None,
        }
    }

    fn encoded_reply_len(rows: &[SyncChangeRow]) -> usize {
        encode_reply(&SyncChangesListReply {
            caller_access: None,
            residency: None,
            changes: rows.iter().map(change_to_wire).collect(),
            complete_through_seq: Some(i64::MAX),
            retirable_through_seq: Some(i64::MAX),
            replica_id: Some(fauna_protocol::ByteBuf::from(vec![0u8; 32])),
            extra: Default::default(),
            signer_certs: Vec::new(),
        })
        .expect("encode")
        .len()
    }

    #[test]
    fn a_page_of_small_state_entries_encodes_within_the_ws_message_cap() {
        for entry_len in [64, 300, 1_000, 4_000, 16_000, 64 * 1024] {
            let rows: Vec<_> = (1..=8_000).map(|s| state_entry(s, entry_len)).collect();
            let (page, rest) = take_change_page(rows);
            assert!(
                !page.is_empty() && !rest.is_empty(),
                "entry {entry_len}: the budget, not the row supply, closed the page"
            );
            let len = encoded_reply_len(&page);
            assert!(
                len <= fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE,
                "entry {entry_len}: a {}-row page encodes to {len} bytes, over the {}-byte WS cap",
                page.len(),
                fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE
            );
            // A real page, not a starved one: the walk still advances in
            // large steps.
            assert!(
                len > crate::segments::SERVE_PAGE_BUDGET_BYTES / 2,
                "entry {entry_len}: a {}-row page of {len} bytes is under half the budget",
                page.len()
            );
        }
    }

    /// The folder arm of `fauna.sync.changes.list` pages a log that encodes
    /// past the WS cap: every reply fits the cap, and a walk by the `since`
    /// cursor until an empty page sees every row exactly once, in order.
    #[tokio::test]
    async fn the_folder_arm_pages_a_log_past_the_cap_and_a_walk_sees_every_row_once() {
        use std::sync::Arc;
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        let state = Arc::new(AppState::for_test(db));
        let actor = [0x41u8; 32];
        state.db.create_user(&actor, "free", "owner").await.unwrap();
        let folder_id = state
            .db
            .create_folder_with_options("photos", &actor, Default::default())
            .await
            .unwrap();
        // ~4 KiB of path per row: 1,200 rows encode to ~5 MiB, two and a half
        // frames' worth.
        let total = 1_200usize;
        let mut recorded = Vec::with_capacity(total);
        for i in 0..total {
            let mut path_hash = [0u8; 32];
            path_hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            let path = format!("{i:06}/{}", "p".repeat(4_000));
            recorded.push(
                state
                    .db
                    .record_sync_change(
                        &actor,
                        &path_hash,
                        Some(&[0xC1u8; 32]),
                        1,
                        "create",
                        Some(folder_id),
                        None,
                        Some(&path),
                    )
                    .await
                    .unwrap(),
            );
        }

        let mut seen = Vec::new();
        let mut since = 0i64;
        let mut pages = 0;
        loop {
            let req = SyncChangesListRequest {
                folder: Some("photos".into()),
                since,
                ..Default::default()
            };
            let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
            let reply_bytes = changes_list_handler()(state.clone(), actor, payload)
                .await
                .expect("the folder arm answers");
            assert!(
                reply_bytes.len() <= fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE,
                "page {pages}: a {}-byte reply is over the {}-byte WS cap",
                reply_bytes.len(),
                fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE
            );
            let reply: SyncChangesListReply =
                fauna_cbor::decode_strict(&reply_bytes).expect("decode reply");
            let Some(top) = reply.changes.last().map(|c| c.seq) else {
                break;
            };
            assert!(top > since, "the walk advances");
            seen.extend(reply.changes.iter().map(|c| c.seq));
            since = top;
            pages += 1;
            assert!(pages <= total, "the walk terminates");
        }
        assert!(pages >= 3, "a ~5 MiB log takes several pages, got {pages}");
        assert_eq!(seen, recorded, "every row once, in seq order");
    }

    /// A page read of a log far larger than one page loads a
    /// bounded number of rows — the cap rides the query, so the rows a page
    /// leaves behind are at most the cap's remainder, never the whole tail —
    /// and the cap never binds before the frame: no page could hold more rows
    /// than it loads.
    #[tokio::test]
    async fn a_folder_page_read_loads_a_bounded_number_of_rows() {
        let db = crate::db::CacheDb::open_in_memory().expect("in-memory db");
        let actor = [0x42u8; 32];
        db.create_user(&actor, "free", "owner").await.unwrap();
        let folder_id = db
            .create_folder_with_options("notes", &actor, Default::default())
            .await
            .unwrap();
        let cap = FOLDER_CHANGE_PAGE_MAX_ROWS as usize;
        let total = cap + 2_000;
        let mut recorded = Vec::with_capacity(total);
        for i in 0..total {
            let mut path_hash = [0u8; 32];
            path_hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            recorded.push(
                db.record_sync_change(
                    &actor,
                    &path_hash,
                    None,
                    0,
                    "delete",
                    Some(folder_id),
                    None,
                    None,
                )
                .await
                .unwrap(),
            );
        }

        // The default page: one page's worth plus one loaded, of a log the
        // cap leaves 2,000 rows short of.
        let (page, rest, _) =
            folder_change_page(&db, folder_id, 0, None, FOLDER_CHANGE_PAGE_MAX_ROWS)
                .await
                .expect("page");
        assert!(!page.is_empty(), "the walk advances");
        assert_eq!(
            page.len() + rest.len(),
            cap,
            "the load stops at the cap, not at the end of the log"
        );
        let loaded: Vec<i64> = page.iter().chain(rest.iter()).map(|r| r.seq).collect();
        assert_eq!(loaded, recorded[..cap], "a contiguous prefix of the log");

        // A smaller asked-for page loads that many, and a walk by the cursor
        // still sees every row once, in order: close early, never skip.
        let mut seen = Vec::new();
        let mut since = 0i64;
        loop {
            let (page, rest, _) = folder_change_page(&db, folder_id, since, None, 4_000)
                .await
                .expect("page");
            assert!(page.len() + rest.len() <= 4_000, "the asked-for cap binds");
            let Some(top) = page.last().map(|r| r.seq) else {
                break;
            };
            seen.extend(page.iter().map(|r| r.seq));
            since = top;
        }
        assert_eq!(seen, recorded, "every row once, in seq order");

        // The cap never closes a page the frame would have let grow: rows
        // costing nothing but their framing overhead fill a page short of it.
        let (page, _) = crate::segments::take_page_within_budget(vec![(); cap + 10], |_| 0);
        assert!(
            page.len() < cap,
            "a page of {} rows reached the {cap}-row load cap",
            page.len()
        );
    }

    /// A page whose delegated signers' certs would pass the frame's headroom
    /// closes earlier: rows go back from the tail, never skipped, until the
    /// page and its side table fit the cap together.
    #[tokio::test]
    async fn a_page_heavy_with_signer_certs_closes_early_to_fit_the_cap() {
        use std::sync::Arc;
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        let actor = [0xefu8; 32];
        let rows = || -> Vec<SyncChangeRow> {
            (1..=8_000)
                .map(|s| {
                    let mut row = state_entry(s, 300);
                    // A distinct delegated signer per row, each with a 2 KiB cert:
                    // a full row page's certs alone are megabytes.
                    let mut key = [0u8; 32];
                    key[..8].copy_from_slice(&(s as u64).to_be_bytes());
                    row.actor_id = actor.to_vec();
                    row.signer_key = Some(key.to_vec());
                    row
                })
                .collect()
        };
        for row in &rows() {
            let key: [u8; 32] = row.signer_key.as_deref().unwrap().try_into().unwrap();
            let cert = fauna_core::encoding::EmbedAsBytes {
                envelope: vec![0x11; 100],
                bytes: vec![0x22; 2_048],
                signer_auth: None,
            };
            db.upsert_sync_signer_cert(
                &actor,
                &key,
                &actor,
                &encode_canonical(&cert).expect("encode cert"),
            )
            .await
            .unwrap();
        }
        let (plain_page, _) = take_change_page(rows());
        let (page, rest, certs) = take_signed_change_page(&db, rows()).await.expect("cut");
        assert!(
            page.len() < plain_page.len(),
            "the certs closed the page earlier than the rows alone"
        );
        assert_eq!(
            certs.len(),
            page.len(),
            "one cert per delegated signer in the page"
        );
        // Contiguous prefix: page then rest is the input, nothing skipped.
        let walked: Vec<i64> = page.iter().chain(rest.iter()).map(|r| r.seq).collect();
        assert_eq!(walked, (1..=8_000).collect::<Vec<i64>>());
        let len = encode_reply(&SyncChangesListReply {
            caller_access: None,
            residency: None,
            changes: page.iter().map(change_to_wire).collect(),
            complete_through_seq: None,
            retirable_through_seq: None,
            replica_id: None,
            extra: Default::default(),
            signer_certs: certs,
        })
        .expect("encode")
        .len();
        assert!(
            len <= fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE,
            "a {}-row page with its certs encodes to {len} bytes, over the cap",
            page.len()
        );
    }
}

#[cfg(test)]
mod unservable_change_tests {
    //! The record door refuses a change row no change-log page could serve
    //! (`transport.md` § Max frame, *count-paged serve replies are
    //! byte-budgeted*): one byte over the cap is refused
    //! `change_too_large` and writes nothing; a row at the cap lands and is
    //! served on a page of its own, so the walk never freezes behind it.
    use super::*;

    use fauna_core::identity::ActorKeypair;

    const SET_NONCE: [u8; 32] = [0x6a; 32];
    const DEVICE: [u8; 32] = [0xd8; 32];
    const MANIFEST: [u8; 32] = [0x22; 32];

    /// The signed record request a writer engine sends for `path`, its label
    /// `seal` being the variable field an oversized row grows.
    fn signed_request(
        recorder: &ActorKeypair,
        fs: &crate::db::FolderRow,
        path: &str,
        seal: &[u8],
    ) -> SyncChangeRecordRequest {
        let mut req = SyncChangeRecordRequest {
            folder: fs.name.clone(),
            device_id: hex::encode(DEVICE),
            path: path.into(),
            manifest_hash: Some(hex::encode(MANIFEST)),
            size_bytes: 10,
            change_type: "create".into(),
            path_sealed: Some(fauna_protocol::ByteBuf::from(seal.to_vec())),
            ..Default::default()
        };
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(recorder)
            .sign_record(&mut req, SET_NONCE)
            .expect("the fixture record signs");
        req
    }

    async fn record(
        state: &AppState,
        recorder: &ActorKeypair,
        fs: &crate::db::FolderRow,
        req: &SyncChangeRecordRequest,
    ) -> Result<i64, RpcError> {
        record_change_core(
            state,
            &recorder.actor_id().0,
            fs,
            &req.path,
            req.manifest_hash.as_deref(),
            req.size_bytes,
            &req.change_type,
            None,
            None,
            &DEVICE,
            req.path_sealed.as_ref().map(|b| &b[..]),
            None,
            None,
            crate::change_signature::CarriedSignature::new(
                req.signature.as_ref(),
                req.signer_key.as_ref(),
            ),
            crate::change_signature::CertCarriage::ByReference,
        )
        .await
    }

    /// What the door measures for `req`: the row it would store.
    fn measured(recorder: &ActorKeypair, req: &SyncChangeRecordRequest) -> usize {
        minted_change_wire_len(crate::db::SyncChangeRow {
            seq: 0,
            path_hash: fauna_core::sync::path_hash(&req.path).to_vec(),
            manifest_hash: Some(MANIFEST.to_vec()),
            size_bytes: req.size_bytes,
            change_type: req.change_type.clone(),
            created_at: 0,
            path: None,
            device_id: Some(DEVICE.to_vec()),
            content_key_version: None,
            thumbnail_hash: None,
            actor_id: recorder.actor_id().0.to_vec(),
            path_sealed: req.path_sealed.as_ref().map(|b| b.to_vec()),
            derived_through: None,
            is_resolution: None,
            is_retention: None,
            item_class: None,
            origin_writer: None,
            origin_seq: None,
            entry_sealed: None,
            signature: req.signature.as_ref().map(|b| b.to_vec()),
            signer_key: req.signer_key.as_ref().map(|b| b.to_vec()),
            deposited_by: None,
        })
    }

    #[tokio::test]
    async fn a_row_over_the_serve_cap_is_refused_and_one_at_it_is_served_alone() {
        let state = crate::test_support::fixture_state();
        let owner_kp = ActorKeypair::from_secret([0xe4u8; 32]);
        let owner = owner_kp.actor_id().0;
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state
            .db
            .create_folder_with_options(
                "big",
                &owner,
                crate::db::FolderOptions {
                    set_nonce: Some(SET_NONCE.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let fs = state
            .db
            .get_folder("big")
            .await
            .unwrap()
            .expect("set exists");

        // The label length that puts the row exactly at the cap: past 64 KiB a
        // byte string's head is a fixed five bytes, so the measure grows one
        // byte per label byte from a probe of 1 MiB.
        let probe = 1 << 20;
        let probe_req = signed_request(&owner_kp, &fs, "a.bin", &vec![7; probe]);
        let at_cap = probe + (MAX_SERVABLE_CHANGE_WIRE_BYTES - measured(&owner_kp, &probe_req));

        let over = signed_request(&owner_kp, &fs, "a.bin", &vec![7; at_cap + 1]);
        assert_eq!(
            measured(&owner_kp, &over),
            MAX_SERVABLE_CHANGE_WIRE_BYTES + 1
        );
        let err = record(&state, &owner_kp, &fs, &over)
            .await
            .expect_err("a row one byte over the cap is refused");
        assert_eq!(err.code, "fauna.sync.change_too_large");
        let rows = state
            .db
            .get_sync_changes_for_folder(fs.id, 0, None)
            .await
            .unwrap();
        assert!(rows.is_empty(), "the refusal writes nothing");

        let at = signed_request(&owner_kp, &fs, "a.bin", &vec![7; at_cap]);
        assert_eq!(measured(&owner_kp, &at), MAX_SERVABLE_CHANGE_WIRE_BYTES);
        let big_seq = record(&state, &owner_kp, &fs, &at)
            .await
            .expect("a row at the cap is recorded");
        let small = signed_request(&owner_kp, &fs, "b.bin", b"small-seal");
        let small_seq = record(&state, &owner_kp, &fs, &small)
            .await
            .expect("a small row after it is recorded");

        // The reader's walk: the cap row is served on a page of its own, and
        // the row after it is the next page's — never an empty page.
        let rows = state
            .db
            .get_sync_changes_for_folder(fs.id, 0, None)
            .await
            .unwrap();
        let (page, rest, _certs) = take_signed_change_page(&state.db, rows).await.unwrap();
        let seqs =
            |rows: &[crate::db::SyncChangeRow]| rows.iter().map(|r| r.seq).collect::<Vec<_>>();
        assert_eq!(seqs(&page), vec![big_seq]);
        assert_eq!(seqs(&rest), vec![small_seq]);
        let (page, rest, _certs) = take_signed_change_page(&state.db, rest).await.unwrap();
        assert_eq!(seqs(&page), vec![small_seq]);
        assert!(rest.is_empty());
    }
}

/// The bounds on relay serving across nests
/// (`deployment-threat-model.md` concern 2, item 3).
#[cfg(test)]
mod foreign_forward_bound_tests {
    use super::*;
    use fauna_core::folder_keys::FolderRef;
    use fauna_protocol::sync::SyncServeForeignEntry;

    fn entry(channel: u8, nest: usize) -> SyncServeForeignEntry {
        SyncServeForeignEntry {
            folder: FolderRef::Foreign([channel; 32]).to_wire(),
            nest_url: format!("https://home-{nest}.example"),
            extra: Default::default(),
        }
    }

    fn meter() -> crate::bridge_rate_limit::Limiter {
        crate::bridge_rate_limit::Limiter::with_config(FOREIGN_FORWARD_METER)
    }

    /// An announce naming more distinct home nests than the cap forwards to
    /// the cap's first-named nests only; several channels of one admitted
    /// nest all go, each once.
    #[test]
    fn an_announce_forwards_to_at_most_the_cap_of_home_nests() {
        let mut entries: Vec<_> = (0..200u8).map(|i| entry(i, i as usize)).collect();
        entries.push(entry(250, 0));
        entries.push(entry(250, 0));
        let picked = select_foreign_forwards(&meter(), &[0x11; 32], &entries);
        assert_eq!(picked.len(), SERVE_ANNOUNCE_MAX_FOREIGN_NESTS);
        assert_eq!(picked[0].0, "https://home-0.example");
        assert_eq!(picked[0].1, vec![[0u8; 32], [250u8; 32]]);
        let channels: usize = picked.iter().map(|(_, c)| c.len()).sum();
        assert_eq!(channels, SERVE_ANNOUNCE_MAX_FOREIGN_NESTS + 1);
    }

    /// Bursts that spend the actor's meter are refused inside the window; the
    /// meter is per actor, so another actor still forwards.
    #[test]
    fn a_burst_past_the_meter_inside_the_window_is_refused() {
        let meter = meter();
        let actor = [0x11; 32];
        let bursts = FOREIGN_FORWARD_METER.max_events as usize / SERVE_ANNOUNCE_MAX_FOREIGN_NESTS;
        for b in 0..bursts {
            let entries: Vec<_> = (0..SERVE_ANNOUNCE_MAX_FOREIGN_NESTS)
                .map(|i| entry(i as u8, b * 100 + i))
                .collect();
            assert_eq!(
                select_foreign_forwards(&meter, &actor, &entries).len(),
                SERVE_ANNOUNCE_MAX_FOREIGN_NESTS,
                "burst {b} is inside the meter"
            );
        }
        let entries: Vec<_> = (0..SERVE_ANNOUNCE_MAX_FOREIGN_NESTS)
            .map(|i| entry(i as u8, 9000 + i))
            .collect();
        assert!(
            select_foreign_forwards(&meter, &actor, &entries).is_empty(),
            "the next burst inside the window forwards nothing"
        );
        assert_eq!(
            select_foreign_forwards(&meter, &[0x22; 32], &entries).len(),
            SERVE_ANNOUNCE_MAX_FOREIGN_NESTS,
            "another actor has its own meter"
        );
    }

    /// A peer's lease reply is clamped: `1` cannot make the nest renew each
    /// second, `u64::MAX` cannot stop renewals so honest seats lapse.
    #[test]
    fn a_peer_lease_reply_is_clamped() {
        let lease = crate::chunk_relay::FOREIGN_SEAT_LEASE.as_secs();
        assert_eq!(clamp_peer_lease(1), FOREIGN_LEASE_FLOOR_SECS);
        assert_eq!(clamp_peer_lease(0), FOREIGN_LEASE_FLOOR_SECS);
        assert_eq!(clamp_peer_lease(u64::MAX), lease);
        assert_eq!(clamp_peer_lease(lease), lease);
        assert!(FOREIGN_LEASE_FLOOR_SECS < lease);
        // The renewal period both lease sites take: the shortest of several
        // replies, each clamped.
        let half = |secs: u64| Some(std::time::Duration::from_secs(secs / 2));
        let foreign_renewal_period =
            |leases: &[u64]| foreign_renewal_period(leases.iter().copied()).map(RenewalPeriod::get);
        assert_eq!(
            foreign_renewal_period(&[lease, 1]),
            half(FOREIGN_LEASE_FLOOR_SECS)
        );
        assert_eq!(foreign_renewal_period(&[0]), half(FOREIGN_LEASE_FLOOR_SECS));
        assert_eq!(
            foreign_renewal_period(&[lease, u64::MAX]),
            half(lease),
            "an honest 120 s seat renews before it lapses"
        );
        assert_eq!(foreign_renewal_period(&[u64::MAX]), half(lease));
        assert_eq!(foreign_renewal_period(&[]), None);
    }

    /// The announce path spends the nest's ONE shared meter: bursts forwarded
    /// through [`forward_foreign_announces`] against one `AppState` leave the
    /// actor's meter in `state.sync` spent, so a fresh per-call meter (which
    /// never accumulates) turns this red. The home
    /// nests are unparseable URLs, refused before any dial; each still spends
    /// its event, as a home nest that was not reached does.
    #[tokio::test]
    async fn the_announce_path_spends_the_shared_meter() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        // The production federation kinds, so the forward reaches the dial.
        let mut router = crate::federation_router::FederationRouter::builder();
        crate::federation_handlers::register_federation_handlers(&mut router);
        let state = std::sync::Arc::new(AppState {
            federation_router: std::sync::Arc::new(router.build()),
            ..AppState::for_test(db)
        });
        let actor = [0x33; 32];
        let device = [0x44; 32];
        let burst = |b: usize| -> Vec<SyncServeForeignEntry> {
            (0..SERVE_ANNOUNCE_MAX_FOREIGN_NESTS)
                .map(|i| SyncServeForeignEntry {
                    folder: FolderRef::Foreign([i as u8; 32]).to_wire(),
                    nest_url: format!("not a url {b} {i}"),
                    extra: Default::default(),
                })
                .collect()
        };
        let bursts = FOREIGN_FORWARD_METER.max_events as usize / SERVE_ANNOUNCE_MAX_FOREIGN_NESTS;
        for b in 0..bursts {
            let admitted = forward_foreign_announces(&state, &actor, &device, &burst(b)).await;
            assert!(admitted.is_empty(), "no home nest is reached");
        }
        assert!(
            select_foreign_forwards(&state.sync.foreign_forward_meter, &actor, &burst(bursts))
                .is_empty(),
            "the announces spent the actor's meter in the nest's state"
        );
    }
}
