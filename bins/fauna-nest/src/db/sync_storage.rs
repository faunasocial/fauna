//! Sync device, file version, folder, and snapshot methods.

use super::{CacheDb, blob_to_array, now_epoch_millis, now_epoch_secs};
use super::{
    ConflictCandidateRow, DeviceSummary, FileVersionRow, FolderMemberRow, FolderRow, ResolveWinner,
    ResolvedReport, SnapshotFileRow, SnapshotRow, SyncChangeRow, SyncConflictRow, SyncDeviceRow,
    SyncFileInfo,
};
use anyhow::{Context, Result};
use fauna_protocol::folders::PlaceFlags;
use rusqlite::OptionalExtension;

/// What became of a [`CacheDb::set_sync_device_grant`] attempt. Three-valued
/// because the two refusals need different wire errors: a missing device row
/// is client-fixable (`fauna.sync.register` first), while a tombstoned key is
/// a revoked credential that must never re-attach (`sync-agent.md`
/// § Credential model — device revocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantStoreOutcome {
    /// The grant is stored on the device row.
    Stored,
    /// The actor has no such device row — register the device first.
    NoDevice,
    /// The grant's device key was revoked by a device deletion; re-attaching
    /// it is refused permanently.
    GrantRevoked,
}

/// What a [`CacheDb::revoke_device_grant`] did, beyond the tombstone it
/// always writes (`sync-agent-credentials.md` § Credential model → the RULED
/// 2026-08-15 block, decision 2; since the RULED 2026-09-28 block's decision
/// 4, a revoke clears the named row's grant columns and keeps the row, full
/// stop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantRevokeOutcome {
    /// Whether a stored grant was actually cleared off a row. `false` is
    /// success too: the key is tombstoned either way.
    pub cleared: bool,
}

/// The `folders` column list every full-[`FolderRow`] projection selects, as
/// a literal so the SELECTs stay `&'static str` (`concat!("SELECT ",
/// folder_columns!(), " FROM …")`).
///
/// **One list, one mapper, eight call sites.** It is a macro rather than a
/// `const` only because `concat!` needs a literal. Before S5 the list and its
/// 19-field mapper were copy-pasted eight times, so growing the row meant
/// sixteen identical edits and any missed pair was a positional-index bug
/// (`row.get(17)` silently reading the wrong column). Add a column here and in
/// [`folder_row_from`], never at a call site.
macro_rules! folder_columns {
    () => {
        "id, name, actor_id, created_at, node_cache, custody_copy, \
         retention_policy, cached_snapshot_count, cached_total_bytes, \
         cached_last_snapshot_at, include_paths, \
         exclude_paths, high_cadence, mls_group_id, webdav_enabled, \
         conflict_policy, web_paywall_tier, name_sealed, name_hash, \
         include_paths_sealed, exclude_paths_sealed, retention_policy_sealed, \
         nest_snapshots, nest_snapshot_quiet_secs, version_retention, \
         audience, website_enabled, public_floor_seq, nest_content_residency, \
         exclusive_editing, audience_attestation, set_nonce"
    };
}

/// Map one row of [`folder_columns!`] onto a [`FolderRow`] — the single
/// positional decoder for that list. See the macro's docs for why both live in
/// exactly one place.
fn folder_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<FolderRow> {
    Ok(FolderRow {
        id: row.get(0)?,
        // A sealed set rests its name NULL (schema 114): read as the empty
        // sentinel, which names no set ([`CacheDb::get_folder_for_actor`]).
        name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        actor_id: row.get(2)?,
        created_at: row.get(3)?,
        node_cache: row.get::<_, i64>(4)? != 0,
        custody_copy: row.get::<_, i64>(5)? != 0,
        retention_policy: row.get(6)?,
        cached_snapshot_count: row.get(7)?,
        cached_total_bytes: row.get(8)?,
        cached_last_snapshot_at: row.get(9)?,
        include_paths: row.get(10)?,
        exclude_paths: row.get(11)?,
        high_cadence: row.get::<_, i64>(12)? != 0,
        mls_group_id: row.get(13)?,
        webdav_enabled: row.get::<_, i64>(14)? != 0,
        conflict_policy: row.get(15)?,
        web_paywall_tier: row.get(16)?,
        name_sealed: row.get(17)?,
        name_hash: row.get(18)?,
        include_paths_sealed: row.get(19)?,
        exclude_paths_sealed: row.get(20)?,
        retention_policy_sealed: row.get(21)?,
        nest_snapshots: row.get::<_, Option<i64>>(22)?.map(|v| v != 0),
        nest_snapshot_quiet_secs: row.get(23)?,
        version_retention: row.get(24)?,
        audience: row.get(25)?,
        website_enabled: row.get::<_, i64>(26)? != 0,
        public_floor_seq: row.get(27)?,
        nest_content_residency: row.get(28)?,
        exclusive_editing: row.get::<_, i64>(29)? != 0,
        audience_attestation: row.get(30)?,
        set_nonce: row.get(31)?,
    })
}

/// The reserved folder name a segment `kind` backs up into, or `None` for a
/// kind that has no cross-location backup surface.
///
/// Re-exported from [`fauna_sync_engine::segment_backup`], which is where it now
/// lives: the **client-device** arms derive this name too — the re-seed delivery
/// leg names the set it records custody into
/// (`fauna_sync_engine::reseed`) — and a device cannot reach a nest binary's
/// private module. Same one-derivation contract, one crate wider.
pub use fauna_sync_engine::segment_backup::reserved_backup_set_name;

/// The reserved destination-side set name an **ordinary folder's** mirrored
/// corpus rests in — `__folder/<source-nest-id-hex>/<folder-id>`
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
///
/// The [`reserved_backup_set_name`] sibling for the folder axis, re-exported
/// from shared Rust (`fauna_core::data`) for the same reason: the source coordinator names
/// its engine's target with it, the federated custody relay re-derives it from
/// the **verified** `origin_nest_id` of the handshake (never a writer-declared
/// name — a writer can therefore never aim custody at another source nest's
/// mirror namespace), and `fauna.backup.destination.attach_folder`'s reply hands
/// it to the client as the config row's `folder_name`. Its inverse is
/// `fauna_core::data::parse_folder_backup_set_name`, whose doc
/// carries the owner-authed-door reasoning.
///
/// The name embeds the source nest's id because `folders.id` is per-nest (two
/// linked source nests can hold the same numeric id), and the stable
/// `folder_id` rather than the folder's name so a rename never re-homes
/// custody. The `__` prefix is what makes every reserved-rail behavior apply
/// structurally (`is_reserved_custody_copy`).
pub use fauna_core::data::folder_backup_set_name;

/// One **live** custody row — the latest generation this nest holds at a path
/// for an owner, as listed to that owner by `fauna.backup.custody.list`.
#[derive(Debug, Clone)]
pub struct BackupCustodyRow {
    /// The custody set's reserved name (`__mail`, `__conv/<hex>`, …).
    pub folder_name: String,
    /// Plaintext path, if the custody row carried one (`path_hash` is one-way,
    /// so a custody row recorded without plaintext stays `None` forever).
    pub path: Option<String>,
    pub path_hash: Vec<u8>,
    pub manifest_hash: Vec<u8>,
    pub size_bytes: i64,
    /// Epoch seconds at which **this nest** last accepted custody for the path.
    /// The destination's own receipt clock, never a value the uploading writer
    /// supplied — which is what makes the client audit's freshness check
    /// source-untrusted.
    pub updated_at: i64,
    /// SQLite rowid — the deterministic order's tiebreaker, carried so the
    /// serve handler can mint the page cursor. Never exposed on the wire as a
    /// field of its own.
    pub rowid: i64,
    /// The **source row's** sealed name — the client's opaque `SealedLabel` over
    /// the covered file's path, carried through the mirror verbatim by whichever
    /// push arm recorded it. `None` for every segment-axis row by design (their
    /// paths are machine-authored routing keys, recorded sealless under the
    /// reserved-backup exemption), and `None` for a folder row a custodian
    /// pushed without the sealed name.
    ///
    /// This is the field the covered-folder materialize arm re-homes on: a live
    /// folder set is not one of the plaintext-path classes, so a row that
    /// arrived without its sealed name cannot become a live row at all
    /// (`fauna.backup.custody_unsealed`; `crate::backup::materialize`). It rides
    /// `CustodyItem::path_sealed` on `fauna.backup.custody.list` (2026-10-06):
    /// the nest-held pull-back re-records each folder row on the rebuilt nest
    /// with it, which is the only way that nest's materialize can re-home the
    /// folder (`segment-backup-protocol.md` § Client-device custodian (pull) →
    /// *Restore* → *The nest-held pull-back*). The audit addresses by
    /// `path_hash` and ignores it.
    pub path_sealed: Option<Vec<u8>>,
}

/// One covered-folder custody row the materialize arm means to re-home, already
/// validated by the caller: the **source's** path hash (recovered from the
/// mirror row's hex-spelled leaf, which is what both push arms record), the
/// sealed name that arrived with it, and the manifest the live row will point
/// at. The triple `message-segment-store.md` § Client-device custodian (pull) →
/// *Restore* names, and nothing else — no key, no plaintext, no byte of content.
#[derive(Debug, Clone)]
pub struct FolderRehomeRow {
    pub path_hash: [u8; 32],
    pub path_sealed: Vec<u8>,
    pub manifest_hash: [u8; 32],
    pub size_bytes: i64,
}

/// One row of a materialize page: a custody row and the owner signature the
/// arm verified over its re-home statement
/// (`writer-signed-change-records.md` ruling (7)(a)(ii)).
#[derive(Debug)]
pub struct FolderRehomeSigned<'a> {
    pub row: &'a FolderRehomeRow,
    pub signature: RowSignature<'a>,
}

/// What [`CacheDb::materialize_folder_custody`] found at the target.
#[derive(Debug)]
pub enum FolderMaterializeOutcome {
    /// The page landed. `rehomed` rows were written now; `resumed` were
    /// already there (an earlier page, or a torn earlier run of this same
    /// ceremony) and were left alone; `remaining` custody rows are still not
    /// live in the target.
    Done {
        folder_id: i64,
        rehomed: u64,
        resumed: u64,
        remaining: u64,
    },
    /// The owner holds no live folder under that row id any more (deleted, or
    /// deleted and re-created, since the verb resolved it) — the target set is prepared by
    /// the ceremony's seed-holding process (`set_lifecycle::create_set`),
    /// never minted here. Nothing was changed.
    Missing,
    /// The named folder's stored set nonce is not the one the page was
    /// verified under (the set was deleted and re-created mid-verb). Nothing
    /// was changed.
    NonceMoved,
    /// The empty-target rule: the target holds `records` live row(s) this
    /// ceremony did not write. Nothing was changed — the transaction rolled back
    /// having written nothing.
    NotEmpty { records: i64 },
    /// The empty-target rule's other half: the target holds no records, but it
    /// is not *fresh* — it carries the publication-bearing property `property`,
    /// so re-homing into it would hand the restored corpus to that property's
    /// audience. Nothing was changed.
    NotFresh { property: &'static str },
}

/// Everything on a `folders` row that can expose the folder's records to a
/// party other than its owner, or hand that party write authority over them.
///
/// This exists as a struct, read and classified in one place, because the
/// empty-target rule is a **freshness** rule (`backup-destinations.md` § Third
/// destination kind → *Re-seed*) and row-emptiness is only its most obvious
/// half. A zero-row folder that is group-bound, public, website-serving or
/// WebDAV-exposed is somebody's audience already, and adopting one is how a
/// restored corpus ends up readable — and tombstonable — by a roster the owner
/// never meant to hand it to.
#[derive(Debug, Default)]
struct TargetFolderPosture {
    /// Non-NULL = bound to an MLS roster. The sharpest member of the family:
    /// `folder_authz::can_read_folder` grants every roster member
    /// `FolderReadGrant::Member` per FOLDER, not per row, and a member with an
    /// explicit `writer` role can tombstone what is re-homed into it.
    mls_group_id: Option<Vec<u8>>,
    /// The owner's explicit declassification (`'public'` = world-readable, names
    /// and paths resting unsealed).
    audience: Option<String>,
    /// The folder serves as the user's website: its records fan out to
    /// `web_files`.
    website_enabled: bool,
    /// The folder is exposed read/write to generic DAV clients by the MDA.
    webdav_enabled: bool,
    /// A paywalled website-enabled folder — still a *served* set, just a charged one.
    web_paywall_tier: Option<String>,
}

impl TargetFolderPosture {
    /// The name of the first property that makes this target unfresh, or `None`
    /// if it carries none of them.
    ///
    /// **Destructured rather than field-accessed, on purpose.** A
    /// publication-bearing column added to `folders` later must be bound here or
    /// this function stops compiling — which is the only mechanism that keeps
    /// the family from silently growing a member the empty-target rule never
    /// looks at. A hand-written list of `if` arms would have compiled fine and
    /// been wrong, which is exactly how the row-count check came to stand in for
    /// the freshness rule in the first place.
    fn unfresh_property(&self) -> Option<&'static str> {
        let Self {
            mls_group_id,
            audience,
            website_enabled,
            webdav_enabled,
            web_paywall_tier,
        } = self;
        if mls_group_id.is_some() {
            return Some("mls_group_id");
        }
        if audience.is_some() {
            return Some("audience");
        }
        if *website_enabled {
            return Some("website_enabled");
        }
        if *webdav_enabled {
            return Some("webdav_enabled");
        }
        if web_paywall_tier.is_some() {
            return Some("web_paywall_tier");
        }
        None
    }
}

/// What [`CacheDb::restore_backup_custody_generation`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRestore {
    /// The generation is live again for its path.
    Restored,
    /// No such generation for this owner (an unknown manifest, or one already
    /// reclaimed past T).
    NotFound,
    /// Refused: the generation was superseded before the owner's writer seat
    /// was taken, so it is a previous writer's numbering
    /// (`segment-backup-protocol.md` § Cross-location backup protocol → *The
    /// writer seat*). It ages out under T like any other.
    BeforeSeat,
}

/// One retained superseded custody generation, as listed to its owner by
/// `fauna.backup.generation.list`.
#[derive(Debug, Clone)]
pub struct BackupCustodyGenerationRow {
    /// The custody set's reserved name (`__mail`, `__conv/<hex>`, …) — the
    /// (kind, scope) address the client re-derives to target a restore.
    pub folder_name: String,
    /// Plaintext path, if the superseded custody row carried one.
    pub path: Option<String>,
    pub path_hash: Vec<u8>,
    pub manifest_hash: Vec<u8>,
    pub size_bytes: i64,
    /// Epoch seconds at which this generation stopped being live — its T clock
    /// start, from which the client computes the restore deadline.
    pub superseded_at: i64,
    /// SQLite rowid — the deterministic order's tiebreaker, carried so the
    /// serve handler can mint the page cursor. Never exposed on the wire as a
    /// field of its own.
    pub rowid: i64,
}

/// Does this custody set participate in the grace window? True iff it is a
/// **custody copy** (see `MIGRATIONS_BACKUP_CUSTODY_GENERATIONS` for why
/// ordinary folders are excluded).
///
/// Deriving the gate here, from `folder_id`, rather than threading a flag from
/// each of the three custody-recording call sites is deliberate: a flag can
/// drift per call site, and a call site that got it wrong would silently disable
/// the grace window for that path. Pure read — safe to call before the quota
/// decision.
fn custody_set_retains_generations(conn: &rusqlite::Connection, folder_id: i64) -> Result<bool> {
    let set: Option<(String, bool)> = conn
        .query_row(
            "SELECT COALESCE(name, ''), custody_copy FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .context("read folder for custody retention gate")?;
    let Some((name, custody_copy)) = set else {
        return Ok(false);
    };
    Ok(crate::db::snapshots::is_reserved_custody_copy(
        custody_copy,
        &name,
    ))
}

/// The bytes a retained generation currently holds for this exact
/// `(path, content)`, or 0 if none is retained. Pure read — the quota
/// arithmetic's "already charged" term, needed before any write happens.
fn retained_custody_generation_bytes(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8; 32],
    manifest_hash: &[u8],
) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT size_bytes FROM backup_custody_generations
             WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3",
            rusqlite::params![folder_id, path_hash.as_slice(), manifest_hash],
            |r| r.get(0),
        )
        .optional()
        .context("probe retained custody generation")?
        .unwrap_or(0))
}

/// Retain one superseded custody generation. The caller must have already
/// confirmed [`custody_set_retains_generations`] — this is the write half only,
/// so it can be ordered after the quota refusal point.
fn retain_custody_generation_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    uploader_actor: &[u8; 32],
    path_hash: &[u8; 32],
    manifest_hash: &[u8],
    size_bytes: i64,
    path: Option<&str>,
    now: i64,
) -> Result<bool> {
    // Already retained under this exact content (a path that flapped back and
    // forth): refresh the clock — over-retain, never under-retain — instead of
    // adding a second row. The bytes stay charged exactly once either way.
    conn.execute(
        "INSERT INTO backup_custody_generations
             (uploader_actor, folder_id, path_hash, manifest_hash, size_bytes, superseded_at, path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(folder_id, path_hash, manifest_hash) DO UPDATE SET
             superseded_at = excluded.superseded_at,
             path          = COALESCE(excluded.path, backup_custody_generations.path)",
        rusqlite::params![
            uploader_actor.as_slice(),
            folder_id,
            path_hash.as_slice(),
            manifest_hash,
            size_bytes,
            now,
            path,
        ],
    )
    .context("retain superseded custody generation")?;
    Ok(true)
}

/// Collapse a **reserved rail's** history at write time: mark every row for
/// `(folder_id, path_hash)` strictly below `head_seq` superseded, so their
/// blobs stop pinning themselves against GC once the grace window elapses.
/// Returns the number of rows marked. Idempotent — a re-run marks 0.
///
/// Called by the raw-opaque reserved rails (`__drafts`,
/// `__mls`) from their own INSERT site, under the same `conn` where they hold
/// it, so the head this collapses under is the row that INSERT just wrote.
///
/// **Why a rail and not an ordinary folder**. Every `sync_changes` row *is* a version
/// ([`file-versions.md`] § "Every recorded change IS a version"), and version
/// retention is **deliberately unbounded** — stated, not solved, for sets a
/// user can reach. A reserved rail is not such a set:
/// `folder_authz::enumerate_readable_folders` skips reserved names in
/// **both** passes, so `fauna.files.versions.{list,get}` can never serve a
/// rail's history, and every rail's own reader takes the newest row only
/// (`ORDER BY seq DESC LIMIT 1`). A rail's superseded rows are therefore
/// reachable by **no wire surface at all** — pure cost, zero recovery value —
/// which is exactly what makes collapsing them lossless where collapsing an
/// ordinary set's history would destroy a shipped feature. The rails also
/// store *whole-state* blobs, not deltas, so a catching-up device that sees
/// only the head is correct by construction, not merely tolerable.
///
/// Only rows **strictly below** the head are marked, so the rail's current
/// state is structurally unmarkable — the same invariant
/// [`CacheDb::supersede_sync_changes_for_path`] rests on. Two racing writers
/// are safe in either interleaving: each marks only below its own seq, so the
/// max-seq row is never marked and the loser's row is collapsed by the winner.
///
/// [`file-versions.md`]: ../../../../docs/goal/behavior/file-versions.md
/// The `sync_changes` columns every feed read selects, in the order
/// [`sync_change_row_from_sql`] maps them. One source of truth for all four read
/// paths: the list was once written out four times and a new column had to be
/// added to each by hand, which is a silent-divergence shape (a read path
/// that forgets a column serves a row whose field is `None` for no reason the
/// caller can see).
pub(crate) const SYNC_CHANGE_COLUMNS: &str = "seq, path_hash, manifest_hash, size_bytes, change_type, \
     created_at, path, device_id, content_key_version, thumbnail_hash, actor_id, path_sealed, \
     derived_through, is_resolution, is_retention, item_class, origin_writer, origin_seq, \
     entry_sealed, signature, signer_key";

/// Map one [`SYNC_CHANGE_COLUMNS`] row.
pub(crate) fn sync_change_row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<SyncChangeRow> {
    Ok(SyncChangeRow {
        seq: row.get(0)?,
        path_hash: row.get(1)?,
        manifest_hash: row.get(2)?,
        size_bytes: row.get(3)?,
        change_type: row.get(4)?,
        created_at: row.get(5)?,
        path: row.get(6)?,
        device_id: row.get(7)?,
        content_key_version: row.get(8)?,
        thumbnail_hash: row.get(9)?,
        actor_id: row.get(10)?,
        path_sealed: row.get(11)?,
        derived_through: row.get(12)?,
        is_resolution: row.get::<_, Option<i64>>(13)?.map(|v| v != 0),
        is_retention: row.get::<_, Option<i64>>(14)?.map(|v| v != 0),
        item_class: row.get(15)?,
        origin_writer: row.get(16)?,
        origin_seq: row.get(17)?,
        entry_sealed: row.get(18)?,
        signature: row.get(19)?,
        signer_key: row.get(20)?,
    })
}

/// Whether row `seq` of `folder_id` still verifies under the set's STORED
/// nonce ([`crate::change_signature::stored_row_signed_under`]) — the record
/// door's echo-delete guard's question about the path's head tombstone. A set
/// with no stored nonce answers `true`: no signed record verified against it,
/// so the incoming delete could not have either, and the guard stays as it was.
fn head_signed_under_stored_nonce_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    seq: i64,
) -> Result<bool> {
    let nonce: Option<Vec<u8>> = conn
        .query_row(
            "SELECT set_nonce FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |r| r.get(0),
        )
        .optional()
        .context("read the set's stored nonce")?
        .flatten();
    let Some(nonce) = nonce.and_then(|n| <[u8; 32]>::try_from(n).ok()) else {
        return Ok(true);
    };
    let row = conn
        .query_row(
            &format!(
                "SELECT {SYNC_CHANGE_COLUMNS} FROM sync_changes WHERE folder_id = ?1 AND seq = ?2"
            ),
            rusqlite::params![folder_id, seq],
            sync_change_row_from_sql,
        )
        .optional()
        .context("read the head tombstone")?;
    Ok(row.is_some_and(|row| crate::change_signature::stored_row_signed_under(&row, &nonce)))
}

/// The SQL value of a byte counter `col` moved by the statement's `?1`:
/// floored at zero on the way down (the generous direction) and SATURATED at
/// `i64::MAX` on the way up. Every quota-counter move that is not itself
/// refused by a `checked_add` ceiling goes through it: an overflowing integer
/// sum is stored by SQLite as REAL, after which every `i64` read of the counter
/// (the admin user listing, a member's role row, the folder roster) fails,
/// repairable from no app (`nest/common.md` § Client-state recoverability).
/// `?1 > 0 AND col > MAX - MAX(?1, 0)` tests for overflow without itself
/// overflowing; only then is `col + ?1` evaluated.
fn saturating_counter_move_sql(col: &str) -> String {
    format!(
        "MAX(0, CASE
             WHEN ?1 > 0 AND {col} > 9223372036854775807 - MAX(?1, 0)
             THEN 9223372036854775807
             ELSE {col} + ?1 END)"
    )
}

/// Move the retained-accounting counters for version rows leaving
/// (`sign = -1`) or rejoining (`sign = +1`) the charged population
/// (`file-versions.md` § Retention (4), slice 3): the folder OWNER's
/// `users.storage_bytes_used` moves by the summed sizes, and each non-owner
/// recorder's `folder_member_access.bytes_used` moves by their own rows' sum,
/// keyed by the set's derived channel — the same owner-pays + member-abuse
/// split `record_sync_change_metered` charges under. Both moves go through
/// [`saturating_counter_move_sql`]: undelete re-charges without a quota
/// refusal, so under an `i64::MAX`-class ceiling its `+` can leave SQLite's
/// integer domain, and the counter saturates instead. A missing folder row
/// moves nothing: that only happens mid-teardown, where the folder-delete
/// reclaim owns the accounting.
pub(crate) fn adjust_version_accounting_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    rows: &[(Vec<u8>, i64)],
    sign: i64,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let folder: Option<(Vec<u8>, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT actor_id, mls_group_id FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read folder for version accounting")?;
    let Some((owner, mls_group_id)) = folder else {
        return Ok(());
    };
    let total = rows
        .iter()
        .fold(0i64, |acc, (_, size)| acc.saturating_add(*size));
    conn.execute(
        &format!(
            "UPDATE users SET storage_bytes_used = {} WHERE actor_id = ?2",
            saturating_counter_move_sql("storage_bytes_used")
        ),
        rusqlite::params![sign.saturating_mul(total), owner.as_slice()],
    )
    .context("adjust owner storage_bytes_used for version accounting")?;
    // The member half: only rows a non-owner recorded, and only on a
    // group-bound set (an owner-only set has no member counters to move).
    if let Some(group_id) = mls_group_id {
        let channel = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
        let mut per_member: std::collections::BTreeMap<&[u8], i64> = Default::default();
        for (actor, size) in rows {
            if actor.as_slice() != owner.as_slice() {
                let sum = per_member.entry(actor.as_slice()).or_default();
                *sum = sum.saturating_add(*size);
            }
        }
        for (actor, member_total) in per_member {
            conn.execute(
                &format!(
                    "UPDATE folder_member_access SET bytes_used = {}
                     WHERE channel_id = ?2 AND actor_id = ?3",
                    saturating_counter_move_sql("bytes_used")
                ),
                rusqlite::params![sign.saturating_mul(member_total), channel.as_slice(), actor],
            )
            .context("adjust member bytes_used for version accounting")?;
        }
    }
    Ok(())
}

/// A version row that holds a quota charge (`sync_changes.charged = 1`), as
/// the release and transfer arithmetic reads it.
pub(crate) struct ChargedVersion {
    seq: i64,
    path_hash: Vec<u8>,
    manifest_hash: Vec<u8>,
    actor: Vec<u8>,
    size_bytes: i64,
}

/// The listable row that holds the charge for `(path, manifest)`, if any — the
/// row a same-manifest record takes its charge from
/// (`writer-signed-change-records.md` ruling (11)(g)). Listable = not
/// superseded, not soft-pruned; the newest such row, should a path that came
/// back to an older manifest carry two.
fn charged_row_for_manifest_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8],
    manifest_hash: &[u8],
) -> Result<Option<ChargedVersion>> {
    conn.query_row(
        "SELECT seq, actor_id, size_bytes FROM sync_changes
         WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3
           AND superseded_at IS NULL AND pruned_at IS NULL AND charged = 1
         ORDER BY seq DESC LIMIT 1",
        rusqlite::params![folder_id, path_hash, manifest_hash],
        |r| {
            Ok(ChargedVersion {
                seq: r.get(0)?,
                path_hash: path_hash.to_vec(),
                manifest_hash: manifest_hash.to_vec(),
                actor: r.get(1)?,
                size_bytes: r.get(2)?,
            })
        },
    )
    .optional()
    .context("read the charged row of a (path, manifest) pair")
}

/// Release the charges of version rows that just LEFT the listable population
/// (`file-versions.md` § Retention (4); `writer-signed-change-records.md`
/// ruling (11)(g)). Call it after the mark that took them out, with only rows
/// that were listable and `charged = 1` before it.
///
/// The charge follows the (path, manifest) pair, so a leaving row whose path
/// still lists an UNCHARGED row of the same manifest hands the flag to the
/// newest such row instead of crediting — the bytes are still listed, and the
/// surviving row's recorder takes the member half. Only when none remains are
/// the bytes credited. Crediting unconditionally is the refused
/// transfer-and-forget shape: prune a re-record and its credit frees bytes the
/// former head still lists.
pub(crate) fn release_version_charges_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    leaving: &[ChargedVersion],
) -> Result<()> {
    for row in leaving {
        let heir: Option<(i64, Vec<u8>, i64)> = conn
            .query_row(
                "SELECT seq, actor_id, size_bytes FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3
                   AND superseded_at IS NULL AND pruned_at IS NULL
                   AND charged = 0 AND seq != ?4
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![folder_id, row.path_hash, row.manifest_hash, row.seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context("find the row a leaving charge moves to")?;
        adjust_version_accounting_in_conn(
            conn,
            folder_id,
            &[(row.actor.clone(), row.size_bytes)],
            -1,
        )?;
        if let Some((heir_seq, heir_actor, heir_size)) = heir {
            conn.execute(
                "UPDATE sync_changes SET charged = (seq = ?1) WHERE seq IN (?1, ?2)",
                rusqlite::params![heir_seq, row.seq],
            )
            .context("move the charged flag to the surviving row")?;
            adjust_version_accounting_in_conn(conn, folder_id, &[(heir_actor, heir_size)], 1)?;
        }
    }
    Ok(())
}

pub(crate) fn collapse_reserved_rail_history_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8; 32],
    head_seq: i64,
) -> Result<u64> {
    Ok(conn
        .execute(
            "UPDATE sync_changes SET superseded_at = ?1
             WHERE folder_id = ?2 AND path_hash = ?3 AND seq < ?4
               AND manifest_hash IS NOT NULL AND superseded_at IS NULL",
            rusqlite::params![
                now_epoch_millis(),
                folder_id,
                path_hash.as_slice(),
                head_seq,
            ],
        )
        .context("collapse reserved rail history")? as u64)
}

/// Drop the retained generation for `(folder_id, path_hash, manifest_hash)` if
/// one exists, returning the bytes it was holding.
///
/// This maintains the accounting invariant the whole grace window rests on:
/// **a manifest that is live for a path is never simultaneously retained for
/// that path.** Without it, a path that flaps H1 → H2 → H1 would leave H1 both
/// live and retained, charged twice, and a later reclaim would credit back bytes
/// that are still live. Called on every path that makes a generation live again
/// — an ordinary re-record and an explicit restore alike.
fn unretain_custody_generation_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8; 32],
    manifest_hash: &[u8],
) -> Result<i64> {
    let held: Option<i64> = conn
        .query_row(
            "SELECT size_bytes FROM backup_custody_generations
             WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3",
            rusqlite::params![folder_id, path_hash.as_slice(), manifest_hash],
            |r| r.get(0),
        )
        .optional()
        .context("probe retained custody generation")?;
    let Some(held) = held else { return Ok(0) };
    conn.execute(
        "DELETE FROM backup_custody_generations
         WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3",
        rusqlite::params![folder_id, path_hash.as_slice(), manifest_hash],
    )
    .context("drop retained generation that became live again")?;
    Ok(held)
}

/// Outcome of a quota-checked storage write: it was refused because it would
/// push the actor past their tier's `max_storage_bytes`, or an underlying DB
/// error. Kept distinct from a plain `anyhow::Error` so the caller surfaces
/// `Exceeded` as the typed `storage_quota_exceeded` wire error (a client-
/// renderable rejection), not an internal 500. The accounting is uniform across
/// all tiers — the tier *is* the quota (`docs/goal/behavior/admin.md` § 2 Users).
#[derive(Debug)]
pub enum StorageQuotaError {
    /// Rejected: `used + requested` would exceed `max`. Nothing was written.
    Exceeded { used: i64, requested: i64, max: i64 },
    /// Rejected: a member recorder's `bytes_used + requested` would exceed
    /// their owner-set `byte_cap` (multi-writer Phase 1, `file-sync.md`
    /// § Multi-writer shared sets). Nothing was written — neither the owner's
    /// quota nor the member counter moved. Wire code: `member_cap_exceeded`.
    MemberCapExceeded { used: i64, requested: i64, cap: i64 },
    /// Rejected: the recorder declared a **negative** `size_bytes`. Nothing was
    /// written and nothing was charged or credited.
    ///
    /// This is a malformed-input refusal living in the metering core rather than
    /// at each door, because the declaration is the meter: under retained
    /// accounting the charge simply IS the declared size, so a negative one
    /// slips past the `charge > 0` ceiling check and then *credits*
    /// `storage_bytes_used`, and one record resets an account to 0 and the tier
    /// is spendable again. All four record doors funnel here
    /// (`fauna.sync.changes.record`, the two `fauna.federation.*.changes.record`
    /// relays, and `fauna.bridges.webdav_record_change`), so one refusal covers
    /// them and a fifth door cannot be added past it. Wire code: `invalid_size`.
    NegativeSize { size_bytes: i64 },
    /// An underlying database error — treat as internal.
    Db(anyhow::Error),
}

/// One file captured into a snapshot, carrying both halves of the path: the
/// plaintext (still resting this major), its `path_hash` routing companion, and
/// the opaque sealed label if a client has sealed one.
///
/// Snapshot creation is a **keyless server-side row copy** — the sealed label
/// rides from the membership projection into `snapshot_files` verbatim, because
/// the `SealedLabel` envelope names its own key generation
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
struct SnapshotCapturedFile {
    manifest_hash: Vec<u8>,
    size_bytes: i64,
    /// Epoch millis. The custody branch normalizes its seconds-granularity
    /// `updated_at` so `snapshot_files.mtime` carries one unit either way.
    mtime: i64,
    /// Required: the post-flip `snapshot_files` PK is `(snapshot_id,
    /// path_hash)`, and the capture SELECTs filter on the label pair, whose
    /// rows always carry the hash.
    path_hash: Vec<u8>,
    path_sealed: Option<Vec<u8>>,
}

impl From<anyhow::Error> for StorageQuotaError {
    fn from(e: anyhow::Error) -> Self {
        Self::Db(e)
    }
}

impl std::fmt::Display for StorageQuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exceeded {
                used,
                requested,
                max,
            } => write!(f, "storage quota exceeded: {used} + {requested} > {max}"),
            Self::MemberCapExceeded {
                used,
                requested,
                cap,
            } => write!(f, "member byte cap exceeded: {used} + {requested} > {cap}"),
            Self::NegativeSize { size_bytes } => {
                write!(f, "size_bytes must not be negative (got {size_bytes})")
            }
            Self::Db(e) => write!(f, "{e:#}"),
        }
    }
}

/// The outcome of a device registration subject to the caller's tier cap:
/// rejected because the actor already holds `max_devices` quota-counted rows,
/// or an underlying DB error. Kept distinct from a plain `anyhow::Error` for
/// exactly the reason [`StorageQuotaError`] is — the caller surfaces
/// `LimitExceeded` as the typed, client-renderable `fauna.sync.device_limit_exceeded`
/// wire error, never an internal 500. The tier *is* the quota
/// (`docs/goal/behavior/admin.md` § 2 Users).
#[derive(Debug)]
pub enum DeviceQuotaError {
    /// Rejected: registering a **new** `device_id` would take the actor past
    /// `max`, which they already meet or exceed (`count`). Nothing was written.
    /// A re-register of a row the actor already holds never reaches this.
    LimitExceeded { count: i64, max: i64 },
    /// An underlying database error — treat as internal.
    Db(anyhow::Error),
}

impl std::fmt::Display for DeviceQuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LimitExceeded { count, max } => {
                write!(f, "device limit reached for your tier: {count} of {max}")
            }
            Self::Db(e) => write!(f, "{e:#}"),
        }
    }
}

/// Fold `(manifest_hash, folder_name)` reference pairs into one entry per
/// hash, `direct_blob = true` iff **every** referencing folder is reserved
/// (`__`-prefixed — [`super::snapshots::is_reserved_folder_name`], the single
/// source of truth for the convention). Reserved rails store the referenced
/// blob directly; anything else is a `ChunkManifest` reference the GC walk
/// must decode. A hash referenced from both kinds classifies as a manifest —
/// decoding it will fail loudly rather than silently skipping a chunk walk.
fn fold_direct_blob_refs(pairs: Vec<(Vec<u8>, String)>) -> Vec<(Vec<u8>, bool)> {
    let mut folded: std::collections::HashMap<Vec<u8>, bool> = std::collections::HashMap::new();
    for (hash, name) in pairs {
        let reserved = super::snapshots::is_reserved_folder_name(&name);
        folded
            .entry(hash)
            .and_modify(|all_reserved| *all_reserved &= reserved)
            .or_insert(reserved);
    }
    folded.into_iter().collect()
}

/// The partial update [`CacheDb::update_folder_for_user`] applies — every
/// field `None` means **leave the column unchanged**.
///
/// The `Option<Option<_>>` fields are the columns whose wire type is itself
/// nullable: the outer `Option` is set-vs-unchanged, the inner one is the value
/// (so `Some(None)` clears). Only the sealed pair uses the clear arm in
/// production today — see [`Self::include_paths_sealed`].
///
/// **Why a struct.** This grew to nine positional arguments, three of them
/// `Option<Option<_>>`, across 23 call sites — and S5a's ninth argument landed a
/// red on `origin/main` for exactly that reason (six `cfg(test)` sites left at
/// eight). [`FolderOptions`] beside it already learned the same lesson on the
/// create twin ("the 8th positional arg broke two integration binaries on main
/// for days"), so the update twin now matches it: callers construct with
/// struct-update (`..Default::default()`) and a future field-add breaks nobody.
#[derive(Debug, Clone, Default)]
pub struct FolderUpdate<'a> {
    pub retention_policy: Option<Option<&'a str>>,
    pub include_paths: Option<Option<&'a str>>,
    pub exclude_paths: Option<Option<&'a str>>,
    pub webdav_enabled: Option<bool>,
    pub conflict_policy: Option<&'a str>,
    /// `Some` = stamp the keyed-writer set-name seal (S5); `None` = leave
    /// unchanged. Opaque here — the nest stores the blob and never opens it. The
    /// reserved-name refusal is the handler's (`fauna.folders.update`), not
    /// this bare column write's.
    pub name_sealed: Option<&'a [u8]>,
    /// `Some(name)` = rest this plaintext name — the →`public` flip's restore of
    /// the URL segment a sealed set rests NULL (`path-sealing.md` § the
    /// set-name plane); `None` = leave unchanged. The handler checks it hashes
    /// to the row's `name_hash`; a write that leaves the set sealed and
    /// non-public blanks it again ([`CacheDb::update_folder_by_id`]).
    pub name: Option<&'a str>,
    /// The owner-sealed `include_paths` list (S6-c). `Some(Some(blob))` stamps,
    /// `Some(None)` **clears**, `None` leaves unchanged.
    ///
    /// ⚠ **The clear arm is load-bearing, not defensive.** The handler passes
    /// `Some(...)` here whenever it writes [`Self::include_paths`], so a keyless
    /// writer's save drops the seal it cannot re-mint instead of leaving a row
    /// whose plaintext says one thing and whose seal opens to the list it
    /// replaced — post-flip the user would be shown the *stale* filesystem
    /// layout with nothing failing. Same rule, same reason as
    /// `register_sync_device`'s deliberately non-`COALESCE` `label_sealed`
    /// (S6-b); `file-sync.md` § Sealed names & paths.
    pub include_paths_sealed: Option<Option<&'a [u8]>>,
    /// The owner-sealed `exclude_paths` list — the
    /// [`Self::include_paths_sealed`] twin, under the same pair rule.
    pub exclude_paths_sealed: Option<Option<&'a [u8]>>,
    /// The label-audience-sealed `retention_policy` (S6-e). `Some(Some(blob))`
    /// stamps, `Some(None)` **clears**, `None` leaves unchanged — the same pair
    /// rule and the same reason as [`Self::include_paths_sealed`], applied to
    /// [`Self::retention_policy`]: post-flip a retained stale seal would show the
    /// user a retention rule that is no longer theirs, and show it silently.
    pub retention_policy_sealed: Option<Option<&'a [u8]>>,
    /// The nest place's snapshot switch. `Some(Some(b))` = the owner chose,
    /// `Some(None)` = **clear back to unset** (return this folder to the nest-wide
    /// behavior), `None` = leave unchanged. The clear arm is a real user gesture
    /// here, not a pair-consistency chore like the sealed arms above — "use the
    /// default" is a third state the user can pick, and folding it into `false`
    /// would silently stop snapshotting a folder that asked for the default.
    pub nest_snapshots: Option<Option<bool>>,
    /// The nest place's quiet period, in seconds, under the same three-state rule
    /// as [`Self::nest_snapshots`]: `Some(None)` clears back to the nest-wide
    /// cadence. A negative value is refused by the handler
    /// (`fauna.folders.update`), never normalized here.
    pub nest_snapshot_quiet_secs: Option<Option<i64>>,
    /// The per-set version-retention bounds JSON (`file-versions.md`
    /// § Retention). `Some(Some(json))` = the owner chose bounds,
    /// `Some(None)` = **clear back to keep-everything** (the handler maps a
    /// binds-nothing policy here — `NULL` is the honest resting value),
    /// `None` = leave unchanged.
    pub version_retention: Option<Option<&'a str>>,
    /// The audience declassification column (v41, phase 4).
    /// `Some(Some("public"))` = declassify; `Some(None)` = **clear** (the
    /// flip-back to the derived private/shared state); `None` = leave
    /// unchanged. Transition validation (bound-ness consistency, the
    /// WebDAV/paywall mutual refusals, the reserved-rail refusal) is the
    /// handler's job (`fauna.folders.update`); this is the bare column write.
    pub audience: Option<Option<&'a str>>,
    /// The website toggle (v41, phase 4). `Some(true)`/`Some(false)` = flip,
    /// `None` = leave unchanged. The reserved-rail refusal is the handler's.
    pub website_enabled: Option<bool>,
    /// The content-residency column (v45, phase 5).
    /// `Some(Some("metadata_only"))` = the consent-gated opt-in;
    /// `Some(None)` = **clear back to full** (the flip-back); `None` = leave
    /// unchanged. Validation (value set, reserved rails, the serving-toggle
    /// mutual refusals) is the handler's job; this is the bare column write.
    pub residency: Option<Option<&'a str>>,
    /// `Some(true)`/`Some(false)` = turn this folder's exclusive editing on
    /// or off; `None` = leave unchanged (`file-sync.md` § Exclusive editing).
    /// A plain bool, not an `Option<Option<_>>` like [`Self::residency`]: the
    /// column is `NOT NULL DEFAULT 0`, so "off" and "unset" are the same
    /// state and there is no third value to clear to. The reserved-rail
    /// refusal is the handler's, not this bare column write's.
    pub exclusive_editing: Option<bool>,
    /// `Some(blob)` = store the owner's audience attestation; `None` = leave
    /// unchanged. There is deliberately no clear arm: a flip-back keeps the
    /// last attestation so the next mint counts above it.
    pub audience_attestation: Option<&'a [u8]>,
    /// `Some` = overwrite the stored set nonce (`FolderRow::set_nonce`) —
    /// owner-only, like every field here; the handler checks the length.
    pub set_nonce: Option<&'a [u8]>,
}

/// One row of [`CacheDb::list_version_prune_population`] — the minimal
/// coordinates the version-retention evaluator needs
/// (`backup::version_prune::evaluate_version_retention_at`).
#[derive(Debug, Clone)]
pub struct VersionPruneCandidate {
    /// `sync_changes.path_hash` — evaluation groups per path.
    pub path_hash: Vec<u8>,
    /// `sync_changes.seq` — the version's stable id.
    pub seq: i64,
    /// `sync_changes.created_at`, epoch **millis** (the recording stamp).
    pub created_at: i64,
}

/// Options for [`CacheDb::create_folder_with_options`]. Grows by field-add:
/// callers construct with struct-update (`..Default::default()`), so a new
/// option never breaks existing call sites — the project's standing
/// fixture-shape convention for growing types; the 8th positional arg broke
/// two integration binaries on main for days.
/// A set's name as a projection hands it to a reader: the plaintext beside its
/// address and its seal, so the reply can carry all three and the reader renders
/// sealed-first (`path-sealing.md` § the set-name plane). `name` is the
/// empty-string sentinel when the row's plaintext is NULL (a scrubbed sealed
/// set); it is never the address — `name_hash` is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetLabel {
    pub name: String,
    pub name_hash: Option<Vec<u8>>,
    pub name_sealed: Option<Vec<u8>>,
}

impl SetLabel {
    /// Read the three columns at `idx`, `idx + 1`, `idx + 2` (`name`,
    /// `name_hash`, `name_sealed`), mapping a NULL name to the empty sentinel.
    fn from_row(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get::<_, Option<String>>(idx)?.unwrap_or_default(),
            name_hash: row.get(idx + 1)?,
            name_sealed: row.get(idx + 2)?,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct FolderOptions {
    pub retention_policy: Option<String>,
    pub include_paths: Option<String>,
    pub exclude_paths: Option<String>,
    /// Create-time conflict policy; `None` keeps the column's `'auto'` default
    /// authoritative (COALESCE in the INSERT).
    pub conflict_policy: Option<String>,
    /// The set's name sealed under the creating client's own root (S5,
    /// `file-sync.md` § Sealed names & paths). `None` on every app today — the
    /// shared create gesture holds no key material, so the seal usually arrives
    /// later via `fauna.folders.update`. Opaque: stored, never opened here.
    pub name_sealed: Option<Vec<u8>>,
    /// The set's `retention_policy` sealed under the creating client's
    /// label-audience root (S6-e). `None` on every app today for the same reason
    /// as [`Self::name_sealed`] — the shared create gesture holds no key material
    /// — so in practice the seal arrives on the first `fauna.folders.update`.
    /// Opaque: stored, never opened here.
    pub retention_policy_sealed: Option<Vec<u8>>,
    /// Create-time audience declassification (v41, phase 4): `Some("public")`
    /// = born declassified — plaintext from the first chunk, no re-seal pass
    /// ever needed. `None` = the private default. The handler validates the
    /// value ("shared" refused, reserved rails refused); this stores it.
    pub audience: Option<String>,
    /// The client-minted 32-byte set nonce (`FolderRow::set_nonce`). The
    /// handler checks the length; this stores it opaque.
    pub set_nonce: Option<Vec<u8>>,
    /// Mint the row as a **custody copy** (`folders.custody_copy` —
    /// `reserved-folders.md` § Destination capability). Set ONLY by the two
    /// nest-side provisioners (`federation_handlers::resolve_backup_custody_set`,
    /// `sync_handlers::writable_or_provisioned_backup_set`); no wire kind
    /// carries it, and the column's CHECK refuses it on a non-reserved name.
    pub custody_copy: bool,
}

/// A change record's verified writer signature, as it rests on the row
/// (`sync_changes.signature` / `signer_key` — `mls-group-key-material.md`
/// § M2 → *Writer-signed change records*). Only a caller that verified it
/// builds one.
#[derive(Debug, Clone, Copy)]
pub struct RowSignature<'a> {
    /// The 64-byte Ed25519 signature over the row's `SignedChange` statement.
    pub signature: &'a [u8],
    /// The 32-byte key it verifies under (device principal key, or the actor id
    /// for a direct signature).
    pub signer_key: &'a [u8],
}

/// The verified signatures a resolved conflict report carries, one per row it
/// mints: the winner head row's (ruling (1)(ii)) and the retained loser's
/// (ruling (10)(d)). `Default` = none (an unsigned caller, the tests that do
/// not care).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReportRowSignatures<'a> {
    pub winner: Option<RowSignature<'a>>,
    pub loser: Option<RowSignature<'a>>,
}

/// What [`CacheDb::conflict_winner_facts`] reads: the fields the choose-winner
/// head row is minted from.
#[derive(Debug, Clone)]
pub struct ConflictWinnerFacts {
    pub folder_id: i64,
    pub path_hash: Vec<u8>,
    pub path_sealed: Option<Vec<u8>>,
    pub device_id: Vec<u8>,
    pub size_bytes: i64,
    pub content_key_version: Option<i64>,
}

/// Outcome of [`CacheDb::supersede_sync_changes_for_path`]. Kept as a domain
/// enum (not an `Err`) so the handler surfaces `HeadMismatch` as the typed,
/// retryable `supersede_head_mismatch` wire error rather than an internal 500 —
/// a head that moved under a concurrent record is normal, not a fault.
#[derive(Debug, PartialEq, Eq)]
pub enum SupersedeOutcome {
    /// N older rows newly marked superseded (0 on an idempotent re-run).
    Marked(u64),
    /// The path has no live manifest head, or its head is not the manifest the
    /// caller verified. Nothing was marked.
    HeadMismatch,
}

/// The `snapshots` column list, in [`row_to_snapshot_row`]'s expected order.
/// Shared by the full-row snapshot `SELECT`s so the column order can't drift
/// between readers (mirrors the `ALIAS_SELECT_COLS` pattern in `mail_aliases`).
const SNAPSHOT_COLS: &str = "id, folder_id, created_at, file_count, total_bytes, parent_id, \
     device_id, max_change_seq, deletion_pending, soft_deleted, purge_after, \
     message_kind, message_manifest, placement_manifest, tag_hashes, tags_sealed";

/// Map a `snapshots` row (selected in [`SNAPSHOT_COLS`] order) to a [`SnapshotRow`].
fn row_to_snapshot_row(row: &rusqlite::Row) -> rusqlite::Result<SnapshotRow> {
    Ok(SnapshotRow {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        created_at: row.get(2)?,
        file_count: row.get(3)?,
        total_bytes: row.get(4)?,
        parent_id: row.get(5)?,
        device_id: row.get(6)?,
        max_change_seq: row.get(7)?,
        deletion_pending: row.get::<_, i64>(8)? != 0,
        soft_deleted: row.get::<_, i64>(9)? != 0,
        purge_after: row.get(10)?,
        message_kind: row.get(11)?,
        message_manifest: row.get(12)?,
        placement_manifest: row.get(13)?,
        tag_hashes: row.get(14)?,
        tags_sealed: row.get(15)?,
    })
}

/// One device place's roster row, written through **one** statement no matter
/// which door the caller came in by (the admin `add_folder_member` or
/// `fauna.folders.places.set`). The three flag columns are the whole place —
/// the legacy `role` column and the single-source bookkeeping it fed retired
/// with the role contraction (`folders.md` § Implementation status today).
fn upsert_folder_place(
    conn: &rusqlite::Connection,
    folder_id: i64,
    device_id: &[u8],
    flags: &PlaceFlags,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO folder_members
            (folder_id, device_id, originates, accepts, applies_deletes)
          VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            folder_id,
            device_id,
            flags.originates,
            flags.accepts,
            flags.applies_deletes
        ],
    )
    .context("upsert folder place")?;
    Ok(())
}

/// One device place plus the label of the device sitting in it — the row shape
/// `fauna.folders.members.list` projects onto `FolderMember`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderPlaceWithLabel {
    pub device_id: Vec<u8>,
    pub label: String,
    pub flags: PlaceFlags,
}

/// Read the place-flag columns off a roster row (all three `NOT NULL`).
fn place_flags_from_row(row: &rusqlite::Row<'_>, first_col: usize) -> rusqlite::Result<PlaceFlags> {
    Ok(PlaceFlags::new(
        row.get(first_col)?,
        row.get(first_col + 1)?,
        row.get(first_col + 2)?,
    ))
}

/// Record that `auth_device_key` may never mint a bearer for `actor_id`
/// again — the single write [`CacheDb::delete_device`] and
/// [`CacheDb::revoke_device_grant`] both perform, in the same transaction as
/// the row change that retires the grant.
fn tombstone_revoked_grant(
    tx: &rusqlite::Transaction,
    actor_id: &[u8],
    auth_device_key: &[u8],
    revoked_at: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO revoked_device_grants
         (actor_id, auth_device_key, revoked_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![actor_id, auth_device_key, revoked_at],
    )
    .context("tombstone revoked device grant")?;
    Ok(())
}

/// The latest-per-path live files for a folder — `create_snapshot` and
/// `create_snapshot_v2`'s identical query since the phase-3 head-plane
/// unification gave both the same `sync_changes` input (2026-08-17).
fn latest_live_snapshot_files(
    conn: &rusqlite::Connection,
    folder_id: i64,
) -> Result<Vec<SnapshotCapturedFile>> {
    let mut stmt = conn.prepare(
        "SELECT sc.manifest_hash, sc.size_bytes, sc.created_at, sc.path_hash, sc.path_sealed
         FROM sync_changes sc
         INNER JOIN (
             SELECT path_hash, MAX(seq) as max_seq
             FROM sync_changes
             WHERE folder_id = ?1 AND (path IS NOT NULL OR path_sealed IS NOT NULL)
             GROUP BY path_hash
         ) latest ON sc.path_hash = latest.path_hash AND sc.seq = latest.max_seq
         WHERE sc.change_type != 'delete' AND sc.manifest_hash IS NOT NULL
         ORDER BY sc.path_hash",
    )?;
    stmt.query_map(rusqlite::params![folder_id], |row| {
        Ok(SnapshotCapturedFile {
            manifest_hash: row.get(0)?,
            size_bytes: row.get(1)?,
            mtime: row.get(2)?,
            path_hash: row.get(3)?,
            path_sealed: row.get(4)?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()
    .map_err(Into::into)
}

impl CacheDb {
    // ==================== File Versions ====================
    //
    // Version history is a projection over the append-only `sync_changes` table
    // (file-sync.md § File Versions, ratified 2026-07-09): a version = a live
    // (`superseded_at IS NULL`) row with a manifest; `version_num` = the row's
    // `seq`. The former `file_versions` table was derived/dead — no production
    // writer ever existed — and is dropped by the migrations.

    /// Version-row projection shared by list/get.
    fn file_version_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileVersionRow> {
        Ok(FileVersionRow {
            path_hash: row.get(0)?,
            version_num: row.get(1)?,
            manifest_hash: row.get(2)?,
            size_bytes: row.get(3)?,
            created_at: row.get(4)?,
            folder_id: row.get(5)?,
            content_key_version: row.get(6)?,
            actor_id: row.get(7)?,
            pruned_at: row.get(8)?,
            purge_after: row.get(9)?,
            device_id: row.get(10)?,
            change_type: row.get(11)?,
            path_sealed: row.get(12)?,
            thumbnail_hash: row.get(13)?,
            derived_through: row.get(14)?,
            is_resolution: row.get::<_, Option<i64>>(15)?.map(|v| v != 0),
            signature: row.get(16)?,
            signer_key: row.get(17)?,
            is_retention: row
                .get::<_, Option<i64>>(18)?
                .and_then(|v| (v != 0).then_some(true)),
        })
    }

    /// List the versions of one file within the given folders, oldest→newest
    /// (`seq` ascending). `folder_ids` is the caller's readable-set scope —
    /// the handler resolves it via `folder_authz`; an empty slice returns
    /// nothing. Superseded rows are excluded exactly like the `changes.list`
    /// feed (their chunks may be GC-reclaimed; the M2 re-seal records a live
    /// twin); tombstones (`manifest_hash IS NULL`) are not restorable versions.
    /// Soft-pruned rows (`pruned_at IS NOT NULL` — file-versions.md § Retention
    /// (3)) leave the default projection; `include_pruned` is the recovery
    /// browse that lists them (still recoverable until `purge_after`).
    /// Prune-*pending* rows stay in the default projection — their 7-day
    /// cancellable window keeps them fully listable and restorable.
    pub async fn list_file_versions_in_sets(
        &self,
        folder_ids: &[i64],
        path_hash: &[u8; 32],
        include_pruned: bool,
    ) -> Result<Vec<FileVersionRow>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let path_hash = *path_hash;
        let ids_csv = folder_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let pruned_filter = if include_pruned {
            ""
        } else {
            "AND pruned_at IS NULL"
        };
        let conn = self.conn.lock().await;
        // `ids_csv` is built from i64s (no injection surface); rusqlite has no
        // native array binding.
        let mut stmt = conn
            .prepare(&format!(
                "SELECT path_hash, seq, manifest_hash, size_bytes, created_at,
                        folder_id, content_key_version, actor_id, pruned_at, purge_after,
                        device_id, change_type, path_sealed, thumbnail_hash, derived_through,
                        is_resolution, signature, signer_key, is_retention
                 FROM sync_changes
                 WHERE path_hash = ?1
                   AND folder_id IN ({ids_csv})
                   AND manifest_hash IS NOT NULL
                   AND superseded_at IS NULL
                   {pruned_filter}
                 ORDER BY seq",
            ))
            .context("prepare list_file_versions_in_sets")?;
        let rows = stmt
            .query_map(
                rusqlite::params![path_hash.as_slice()],
                Self::file_version_from_row,
            )
            .context("query file versions")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read file version row")?);
        }
        Ok(results)
    }

    /// Get one version by `(path_hash, version_num = seq)`. Returns the row
    /// regardless of folder — the handler authz-checks `folder_id` against
    /// the caller's readable sets and maps unauthorized to `not_found` (no
    /// existence oracle). Deliberately serves a soft-pruned row (`pruned_at`
    /// set): restore-as-re-point and the undelete verb both need its metadata
    /// while the 30-day recovery window is open.
    pub async fn get_file_version_by_seq(
        &self,
        path_hash: &[u8; 32],
        version_num: i64,
    ) -> Result<Option<FileVersionRow>> {
        let path_hash = *path_hash;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT path_hash, seq, manifest_hash, size_bytes, created_at,
                    folder_id, content_key_version, actor_id, pruned_at, purge_after,
                        device_id, change_type, path_sealed, thumbnail_hash, derived_through,
                        is_resolution, signature, signer_key, is_retention
             FROM sync_changes
             WHERE path_hash = ?1 AND seq = ?2
               AND manifest_hash IS NOT NULL
               AND superseded_at IS NULL
               AND folder_id IS NOT NULL",
            rusqlite::params![path_hash.as_slice(), version_num],
            Self::file_version_from_row,
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get file version"),
        }
    }

    // ── The version-retention prune pipeline (file-versions.md § Retention (3)) ──
    //
    // The § 7 layers state-for-state on the version plane: the evaluator marks
    // `prune_pending` under a 7-day cancellable `VersionBulkPrune` action; the
    // executor soft-prunes (`pruned_at` + 30-day `purge_after`); the GC-cycle
    // purge step stamps `superseded_at` — the *terminal* reclaim mark, after
    // which the existing pin predicate (`sync_change_manifest_refs`) releases
    // the chunks one GC grace later. Nothing on this path deletes a row.

    /// The evaluation population for one folder's version auto-prune, ordered
    /// by `(path_hash, seq)`: listable rows (`manifest_hash IS NOT NULL`,
    /// `superseded_at IS NULL`) not already in the prune pipeline. Excluding
    /// pipeline rows is also what makes the evaluator idempotent — a marked row
    /// leaves this population, so a slow window cannot pile up duplicate
    /// actions (the `is_active_snapshot` twin).
    pub async fn list_version_prune_population(
        &self,
        folder_id: i64,
    ) -> Result<Vec<VersionPruneCandidate>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT path_hash, seq, created_at FROM sync_changes
                 WHERE folder_id = ?1
                   AND manifest_hash IS NOT NULL
                   AND superseded_at IS NULL
                   AND (prune_pending IS NULL OR prune_pending = 0)
                   AND pruned_at IS NULL
                 ORDER BY path_hash, seq",
            )
            .context("prepare list_version_prune_population")?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                Ok(VersionPruneCandidate {
                    path_hash: row.get(0)?,
                    seq: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })
            .context("query version prune population")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read version prune candidate")?);
        }
        Ok(out)
    }

    /// Mark one version row targeted by an un-executed `VersionBulkPrune`
    /// pending action (Layer 2). The row stays listable and restorable;
    /// `cancel_pending_action` clears the mark.
    pub async fn mark_version_prune_pending(&self, seq: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sync_changes SET prune_pending = 1 WHERE seq = ?1",
            rusqlite::params![seq],
        )
        .context("mark_version_prune_pending")?;
        Ok(())
    }

    /// Soft-prune one version (the Layer-2 → Layer-3 hop): out of the default
    /// `versions.list` projection, recoverable for 30 days via
    /// `fauna.files.versions.undelete`. Guarded on the row still being live —
    /// a row superseded (or tombstoned) between scheduling and execution is
    /// left alone.
    pub async fn soft_prune_version(&self, seq: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now_millis = now_epoch_millis();
        let purge_after = now_epoch_secs() + 30 * 24 * 3600;
        // `pruned_at IS NULL` makes the hop idempotent: a duplicate
        // `VersionBulkPrune` referencing an already-pruned seq neither extends
        // its `purge_after` nor credits the meter twice.
        let affected = conn
            .execute(
                "UPDATE sync_changes
                 SET pruned_at = ?1, purge_after = ?2, prune_pending = NULL
                 WHERE seq = ?3 AND manifest_hash IS NOT NULL
                   AND superseded_at IS NULL AND pruned_at IS NULL",
                rusqlite::params![now_millis, purge_after, seq],
            )
            .context("soft_prune_version")?;
        if affected == 1 {
            // The row left the listable population — release its retained
            // charge (§ Retention (4): release = leaving listability; the
            // later purge moves the meter not at all). Only a row that HOLDS
            // the charge releases one, and it hands the flag to a surviving
            // same-manifest row before it credits (ruling (11)(g)).
            let (folder_id, charged, row): (i64, bool, ChargedVersion) = conn
                .query_row(
                    "SELECT folder_id, charged, path_hash, manifest_hash, actor_id, size_bytes
                     FROM sync_changes WHERE seq = ?1",
                    rusqlite::params![seq],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            ChargedVersion {
                                seq,
                                path_hash: r.get(2)?,
                                manifest_hash: r.get(3)?,
                                actor: r.get(4)?,
                                size_bytes: r.get(5)?,
                            },
                        ))
                    },
                )
                .context("read soft-pruned row for accounting")?;
            if charged {
                release_version_charges_in_conn(&conn, folder_id, &[row])?;
            }
        }
        Ok(affected == 1)
    }

    /// Restore a soft-pruned version to the listable population, Layer 1-style:
    /// every pipeline mark clears, so the row is exactly as listable, restorable
    /// and floor-counted as before the prune touched it. `Ok(false)` = the row
    /// is not currently soft-pruned (never pruned, already purged, or already
    /// undeleted — the handler maps that to `not_found`).
    pub async fn undelete_version(&self, seq: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE sync_changes
                 SET pruned_at = NULL, purge_after = NULL, prune_pending = NULL
                 WHERE seq = ?1 AND pruned_at IS NOT NULL AND superseded_at IS NULL",
                rusqlite::params![seq],
            )
            .context("undelete_version")?;
        if affected == 1 {
            // The row rejoined the listable population — re-charge it,
            // deliberately WITHOUT a quota/cap refusal (§ Retention (4):
            // recovery must never strand behind a full quota; an over-quota
            // resting state is expressible, and only positive-charge records
            // refuse). The charge follows the (path, manifest) pair (ruling
            // (11)(g)): the row is charged only when no listable row of its
            // path and manifest already holds the charge — its flag is decided
            // here, afresh, whatever it read while the row was out.
            let row: (i64, Vec<u8>, Vec<u8>, Vec<u8>, i64) = conn
                .query_row(
                    "SELECT folder_id, path_hash, manifest_hash, actor_id, size_bytes
                     FROM sync_changes WHERE seq = ?1",
                    rusqlite::params![seq],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .context("read undeleted row for accounting")?;
            let pair_charged: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM sync_changes
                     WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3
                       AND superseded_at IS NULL AND pruned_at IS NULL
                       AND charged = 1 AND seq != ?4)",
                    rusqlite::params![row.0, row.1, row.2, seq],
                    |r| r.get(0),
                )
                .context("read whether the undeleted row's pair is charged")?;
            conn.execute(
                "UPDATE sync_changes SET charged = ?1 WHERE seq = ?2",
                rusqlite::params![!pair_charged, seq],
            )
            .context("set the undeleted row's charged flag")?;
            if !pair_charged {
                adjust_version_accounting_in_conn(&conn, row.0, &[(row.3, row.4)], 1)?;
            }
        }
        Ok(affected == 1)
    }

    /// The GC-cycle purge step: stamp `superseded_at` on every soft-pruned row
    /// whose 30-day `purge_after` has elapsed — the terminal mark the pin
    /// predicate releases one GC grace later. Returns `(purged, guarded)`.
    ///
    /// The `EXISTS` guard requires a strictly newer row for the same
    /// `(folder_id, path_hash)` — true by construction for every row the
    /// evaluator marked (the head was structurally excluded, and rows are
    /// append-only), so a violation means the path's history mutated in a shape
    /// this pipeline never produces. Such a row is SKIPPED, stays soft-pruned
    /// (recoverable via the `include_pruned` browse + undelete), and is counted
    /// in `guarded` for the caller to log — over-retain, never over-delete.
    pub async fn purge_expired_soft_pruned_versions(&self) -> Result<(usize, usize)> {
        let conn = self.conn.lock().await;
        let now_secs = now_epoch_secs();
        let now_millis = now_epoch_millis();
        let newer_row_exists = "EXISTS (SELECT 1 FROM sync_changes n
                     WHERE n.folder_id = sync_changes.folder_id
                       AND n.path_hash = sync_changes.path_hash
                       AND n.seq > sync_changes.seq)";
        let expired = "pruned_at IS NOT NULL AND superseded_at IS NULL
                   AND purge_after IS NOT NULL AND purge_after <= ?1";
        let purged = conn
            .execute(
                &format!(
                    "UPDATE sync_changes SET superseded_at = ?2
                     WHERE {expired} AND {newer_row_exists}"
                ),
                rusqlite::params![now_secs, now_millis],
            )
            .context("purge expired soft-pruned versions")?;
        let guarded: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM sync_changes
                     WHERE {expired} AND NOT {newer_row_exists}"
                ),
                rusqlite::params![now_secs],
                |row| row.get(0),
            )
            .context("count guard-skipped soft-pruned versions")?;
        Ok((purged, guarded as usize))
    }

    // ==================== Sync ====================

    /// Register (or re-register) one sync device, refusing past the caller's
    /// tier `max_devices` when `max_devices` is `Some`.
    ///
    /// `None` is "no cap applies" — the embedded single-user desktop nest
    /// (`AppState::enforce_tier_quotas` off, where a tier ceiling on the
    /// owner's own machine is nonsense) and every nest-internal writer. The
    /// count and the insert share **one** connection lock, so two concurrent
    /// registers cannot both observe a sub-cap count and both insert.
    ///
    /// Two shapes the cap deliberately keeps working (`admin.md` § 2 Users):
    ///
    /// - **A re-register of an existing `device_id` always succeeds**, cap or
    ///   no cap. The statement below is an upsert, so a device already holding
    ///   a row is refreshing its label / capabilities, not consuming a slot;
    ///   refusing it would strand a device at the cap with no way to re-label
    ///   itself or to renew (`fauna_client_sync::register_this_machine` registers
    ///   on every provision).
    /// - **The nest's own WebDAV pseudo-device does not count.** It is written
    ///   here by `fauna.bridges.webdav_record_change`, not by the user's fleet,
    ///   and charging the user's tier for a row the nest authored would refuse
    ///   an honest device because a WebDAV client once wrote a file. Excluded
    ///   by its deterministic per-actor id, so at most one row per actor is
    ///   ever uncounted (`label_custody::webdav_pseudo_device_id`). Safe only
    ///   because the id is unreachable from a client: `fauna.sync.register`
    ///   refuses it at the door (`sync_handlers.rs::register_handler`) rather
    ///   than letting a caller occupy that exact row.
    pub async fn register_sync_device_capped(
        &self,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
        label: &str,
        label_sealed: Option<&[u8]>,
        capabilities: &str,
        max_devices: Option<i64>,
    ) -> std::result::Result<(), DeviceQuotaError> {
        let actor_id = *actor_id;
        let device_id = *device_id;
        // S9 flip: the plaintext label column rests only for the three
        // machine-authored synthetic labels (frozen constants — sealing them
        // protects nothing). A user-chosen label rests sealed-only; a sealless
        // register rests no label at all (the ratified Omit degrade — the
        // device re-labels on its next keyed register). For a writer that
        // never sends a keyed register the row rests nameless PERMANENTLY,
        // and that is accepted (user ruling 2026-08-02: no pre-expand alpha
        // clients are assumed to remain in the field, so no re-stamp path
        // exists or is owed — `file-sync.md` § Sealed names & paths).
        let label = if fauna_core::label_custody::is_synthetic_device_label(label) {
            label.to_string()
        } else {
            String::new()
        };
        let label_sealed = label_sealed.map(|b| b.to_vec());
        let capabilities = capabilities.to_string();
        let conn = self.conn.lock().await;
        // The cap check rides the same lock as the insert below — see the doc
        // comment: count, existence and write are one critical section.
        if let Some(max) = max_devices {
            let already_registered: bool = conn
                .query_row(
                    "SELECT 1 FROM sync_devices WHERE actor_id = ?1 AND device_id = ?2",
                    rusqlite::params![actor_id.as_slice(), device_id.as_slice()],
                    |_| Ok(()),
                )
                .optional()
                .context("device row lookup for tier cap")
                .map_err(DeviceQuotaError::Db)?
                .is_some();
            if !already_registered {
                let webdav = fauna_core::label_custody::webdav_pseudo_device_id(&actor_id);
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sync_devices \
                         WHERE actor_id = ?1 AND device_id != ?2",
                        rusqlite::params![actor_id.as_slice(), webdav.as_slice()],
                        |row| row.get(0),
                    )
                    .context("count sync devices for tier cap")
                    .map_err(DeviceQuotaError::Db)?;
                if count >= max {
                    return Err(DeviceQuotaError::LimitExceeded { count, max });
                }
            }
        }
        let now = now_epoch_millis();
        // An explicit upsert, NOT `INSERT OR REPLACE`: replace deletes the
        // conflicting row and re-inserts, so every column absent from the list
        // silently reverts to its default — which would reset
        // `guardian_marked` (family-safety.md § Full visibility) to 0 and hand
        // the ward a two-call bypass of the marker. The ward can read the
        // marked device's id off their own `devices.list` (transparency
        // guarantees it) and `fauna.sync.register` is scoped to the caller's
        // own actor, so re-registering that id is a call they can always make.
        // Listing every column this statement means to write keeps the
        // pre-marker behaviour byte-for-byte while leaving the flag alone; any
        // future column added here must make the same choice deliberately.
        //
        // ⚠ `label_sealed` is written **unconditionally alongside `label`**, and
        // deliberately NOT as `COALESCE(excluded.label_sealed, label_sealed)`:
        // the pair must move together. A keyless re-register (the ffi bearer-only
        // arm) therefore drops the row to nameless until the device's next keyed
        // register re-stamps it — whereas keeping the old seal would leave a row
        // whose seal opens to the *previous* name, shown to the user with
        // nothing failing. Post-flip there is no plaintext for any backfill to
        // re-stamp from; the transient namelessness is the accepted degrade
        // (`file-sync.md` § Sealed names & paths, incl. the 2026-08-02 ruling).
        conn.execute(
            "INSERT INTO sync_devices (actor_id, device_id, label, label_sealed, registered_at, last_seen, capabilities)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (actor_id, device_id) DO UPDATE SET
                 label = excluded.label,
                 label_sealed = excluded.label_sealed,
                 registered_at = excluded.registered_at,
                 last_seen = excluded.last_seen,
                 capabilities = excluded.capabilities",
            rusqlite::params![
                actor_id.as_slice(),
                device_id.as_slice(),
                label,
                label_sealed,
                now,
                now,
                capabilities
            ],
        )
        .context("register sync device")
        .map_err(DeviceQuotaError::Db)?;
        Ok(())
    }

    /// [`Self::register_sync_device_capped`] with no cap — every nest-internal
    /// writer and every test fixture. The user-facing door is
    /// `fauna.sync.register`, which passes the caller's tier cap.
    pub async fn register_sync_device(
        &self,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
        label: &str,
        label_sealed: Option<&[u8]>,
        capabilities: &str,
    ) -> Result<()> {
        self.register_sync_device_capped(
            actor_id,
            device_id,
            label,
            label_sealed,
            capabilities,
            None,
        )
        .await
        .map_err(|e| match e {
            DeviceQuotaError::Db(inner) => inner,
            // Unreachable: `None` never takes the cap branch. Folded rather
            // than `unreachable!()` so a future caller cannot panic a nest.
            other => anyhow::anyhow!(other.to_string()),
        })
    }

    /// Store a device's verified renewal grant (sync-agent.md § Credential
    /// model): the renewal device public key + the embed-as-bytes
    /// `DeviceAuthorization` wire blob, on the actor's own `(actor_id,
    /// device_id)` row. The caller (`fauna.sync.device_grant.register`) has
    /// already verified the envelope; the mint path re-verifies at use.
    /// Returns [`GrantStoreOutcome::NoDevice`] when the actor has no such
    /// device row (register the device first) — deliberately not an
    /// upsert-into-existence: a grant with no device row would be unreachable
    /// from the devices UI, i.e. unrevocable. Returns
    /// [`GrantStoreOutcome::GrantRevoked`] when the grant's device key was
    /// tombstoned by a device deletion — replaying a revoked grant must not
    /// restore renewal. The upsert in [`Self::register_sync_device`] leaves
    /// these columns untouched, so a routine login re-register never drops a
    /// grant.
    pub async fn set_sync_device_grant(
        &self,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
        auth_device_key: &[u8; 32],
        auth_grant: &[u8],
    ) -> Result<GrantStoreOutcome> {
        let actor_id = *actor_id;
        let device_id = *device_id;
        let auth_device_key = *auth_device_key;
        let auth_grant = auth_grant.to_vec();
        let conn = self.conn.lock().await;
        // The revocation-memory guard runs under the same connection lock as
        // `delete_device`'s tombstone write, so a register racing a delete
        // cannot slip a revoked key back in between the check and the store.
        let revoked: bool = conn
            .query_row(
                "SELECT 1 FROM revoked_device_grants
                 WHERE actor_id = ?1 AND auth_device_key = ?2",
                rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
                |_| Ok(()),
            )
            .optional()
            .context("check revoked device grant")?
            .is_some();
        if revoked {
            return Ok(GrantStoreOutcome::GrantRevoked);
        }
        let changed = conn
            .execute(
                "UPDATE sync_devices SET auth_device_key = ?3, auth_grant = ?4
                 WHERE actor_id = ?1 AND device_id = ?2",
                rusqlite::params![
                    actor_id.as_slice(),
                    device_id.as_slice(),
                    auth_device_key.as_slice(),
                    auth_grant
                ],
            )
            .context("set sync device grant")?;
        Ok(if changed > 0 {
            GrantStoreOutcome::Stored
        } else {
            GrantStoreOutcome::NoDevice
        })
    }

    /// Whether `device_key` is the store device principal of a device enrolled
    /// for ANY account on this nest and not since revoked — one source of the
    /// relay's admission answer (`crate::relay_admission`). Keyed on the device
    /// key alone because the relay's question carries no actor: an endpoint
    /// that connects to the relay proves only its own key.
    pub async fn is_enrolled_device_principal(&self, device_key: &[u8; 32]) -> Result<bool> {
        let device_key = *device_key;
        let conn = self.conn.lock().await;
        Ok(conn
            .query_row(
                "SELECT 1 FROM sync_devices d
                 WHERE d.auth_device_key = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM revoked_device_grants r
                       WHERE r.actor_id = d.actor_id
                         AND r.auth_device_key = d.auth_device_key)
                 LIMIT 1",
                rusqlite::params![device_key.as_slice()],
                |_| Ok(()),
            )
            .optional()
            .context("look up enrolled device principal")?
            .is_some())
    }

    /// Whether `(actor, renewal device key)` was revoked by a device deletion
    /// — the mint path's belt check beside the register-side
    /// [`GrantStoreOutcome::GrantRevoked`] refusal, so "a tombstoned key never
    /// mints" holds even against a grant row that reappeared some other way.
    pub async fn is_device_grant_revoked(
        &self,
        actor_id: &[u8; 32],
        auth_device_key: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = *actor_id;
        let auth_device_key = *auth_device_key;
        let conn = self.conn.lock().await;
        Ok(conn
            .query_row(
                "SELECT 1 FROM revoked_device_grants
                 WHERE actor_id = ?1 AND auth_device_key = ?2",
                rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
                |_| Ok(()),
            )
            .optional()
            .context("check revoked device grant")?
            .is_some())
    }

    /// Load the stored renewal-grant wire blob for `(actor, renewal device
    /// public key)` — the `fauna.auth.device_handshake` lookup. `None` when no
    /// device of this actor carries a grant for that key (never registered, or
    /// revoked by device deletion).
    pub async fn get_sync_device_grant(
        &self,
        actor_id: &[u8; 32],
        auth_device_key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let actor_id = *actor_id;
        let auth_device_key = *auth_device_key;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT auth_grant FROM sync_devices
                 WHERE actor_id = ?1 AND auth_device_key = ?2 AND auth_grant IS NOT NULL",
            )
            .context("prepare get_sync_device_grant")?;
        let result = stmt.query_row(
            rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(blob) => Ok(Some(blob)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get sync device grant"),
        }
    }

    /// Every live renewal-grant wire blob for `(actor, device key)` — the
    /// change-record signer resolution (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (1)): **any** `sync_devices` row
    /// of the actor whose `auth_device_key` is the signer and whose grant is
    /// present, never the recording device's row (on macOS the File Provider
    /// extension records under the app's device id while the machine grant may
    /// sit on the agent's row). Empty when the key is tombstoned in
    /// `revoked_device_grants` — deleting a device revokes its key everywhere,
    /// not one row. Unlike [`Self::get_sync_device_grant`] this assumes nothing
    /// about how many rows carry the key.
    pub async fn live_sync_device_grants(
        &self,
        actor_id: &[u8; 32],
        auth_device_key: &[u8; 32],
    ) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let revoked = conn
            .query_row(
                "SELECT 1 FROM revoked_device_grants
                 WHERE actor_id = ?1 AND auth_device_key = ?2",
                rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
                |_| Ok(()),
            )
            .optional()
            .context("check revoked signer key")?
            .is_some();
        if revoked {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT auth_grant FROM sync_devices
                 WHERE actor_id = ?1 AND auth_device_key = ?2 AND auth_grant IS NOT NULL
                 ORDER BY device_id",
            )
            .context("prepare live_sync_device_grants")?;
        let rows = stmt
            .query_map(
                rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .context("query live_sync_device_grants")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read one grant")?);
        }
        Ok(out)
    }

    /// Remember the cert a delegated change-record signer verified under
    /// (`sync_signer_certs`; canonical-encoded `EmbedAsBytes`), keyed by the
    /// row's actor, the device key and `cert_actor_id` — the actor the cert
    /// names, which the caller passes from the verify (never re-decoded here).
    /// Within one key the latest verified cert replaces the stored one — a
    /// re-ceremony certifies the same key with more capabilities — while a
    /// cert naming another identity sits beside it
    /// (`writer-signed-change-records.md` ruling (8)(i)).
    pub async fn upsert_sync_signer_cert(
        &self,
        actor_id: &[u8; 32],
        device_key: &[u8; 32],
        cert_actor_id: &[u8; 32],
        cert_wire: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sync_signer_certs
                 (actor_id, device_key, cert_actor_id, cert, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(actor_id, device_key, cert_actor_id) DO UPDATE
                 SET cert = excluded.cert, updated_at = excluded.updated_at
                 WHERE cert IS NOT excluded.cert",
            rusqlite::params![
                actor_id.as_slice(),
                device_key.as_slice(),
                cert_actor_id.as_slice(),
                cert_wire,
                now_epoch_millis()
            ],
        )
        .context("upsert sync signer cert")?;
        Ok(())
    }

    /// The stored certs for the given `(actor, device key)` signers — the
    /// `signer_certs` side table a list reply carries: EVERY cert at each
    /// pair, one per identity that certified the key in the actor's history
    /// (ruling (8)(i)). A signer with no stored cert is simply absent (its
    /// rows then fail `CertMissing` on a reader).
    pub async fn sync_signer_certs(
        &self,
        signers: &[([u8; 32], [u8; 32])],
    ) -> Result<Vec<Vec<u8>>> {
        if signers.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cert FROM sync_signer_certs WHERE actor_id = ?1 AND device_key = ?2
                 ORDER BY cert_actor_id",
            )
            .context("prepare sync_signer_certs")?;
        let mut out = Vec::new();
        for (actor, key) in signers {
            let certs = stmt
                .query_map(rusqlite::params![actor.as_slice(), key.as_slice()], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .context("read sync signer certs")?;
            for cert in certs {
                out.push(cert.context("read one sync signer cert")?);
            }
        }
        Ok(out)
    }

    /// Get the capabilities string for a device, or None if not found.
    pub async fn get_device_capabilities(&self, device_id: &[u8; 32]) -> Result<Option<String>> {
        let device_id = *device_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT capabilities FROM sync_devices WHERE device_id = ?1")
            .context("prepare get_device_capabilities")?;
        let result = stmt.query_row(rusqlite::params![device_id.as_slice()], |row| row.get(0));
        match result {
            Ok(caps) => Ok(Some(caps)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get device capabilities"),
        }
    }

    /// Get a device's capabilities **scoped to an owning actor** — the row only
    /// matches when the device was registered *by that actor* (`sync_devices` is
    /// keyed `(actor_id, device_id)`). Returns `None` when no such device belongs
    /// to the actor.
    ///
    /// This is the device-side authorization a recording door needs: a
    /// connection is authenticated to an actor by its bearer, and a `device_id`
    /// is an attacker-controllable wire param, so the door must confirm
    /// the named device is one of *that authenticated actor's* devices — not
    /// merely registered to *some* actor (which the actor-unscoped
    /// [`Self::get_device_capabilities`] cannot distinguish).
    pub async fn get_device_capabilities_for_actor(
        &self,
        actor_id: &[u8; 32],
        device_id: &[u8; 32],
    ) -> Result<Option<String>> {
        let actor_id = *actor_id;
        let device_id = *device_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT capabilities FROM sync_devices WHERE actor_id = ?1 AND device_id = ?2")
            .context("prepare get_device_capabilities_for_actor")?;
        let result = stmt.query_row(
            rusqlite::params![actor_id.as_slice(), device_id.as_slice()],
            |row| row.get(0),
        );
        match result {
            Ok(caps) => Ok(Some(caps)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get device capabilities for actor"),
        }
    }

    /// List all sync devices for an actor.
    pub async fn list_sync_devices(&self, actor_id: &[u8; 32]) -> Result<Vec<SyncDeviceRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, device_id, label, label_sealed, registered_at, last_seen,
                    capabilities, guardian_marked, auth_device_key,
                    p2p_participation, p2p_off_requested
                 FROM sync_devices WHERE actor_id = ?1 ORDER BY registered_at",
            )
            .context("prepare list_sync_devices")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                Ok(SyncDeviceRow {
                    actor_id: row.get(0)?,
                    device_id: row.get(1)?,
                    label: row.get(2)?,
                    label_sealed: row.get(3)?,
                    registered_at: row.get(4)?,
                    last_seen: row.get(5)?,
                    capabilities: row.get(6)?,
                    guardian_marked: row.get::<_, i64>(7)? != 0,
                    principal: row.get(8)?,
                    p2p_participation: row
                        .get::<_, Option<String>>(9)?
                        .map(|s| s == P2P_PARTICIPATION_ON),
                    p2p_off_requested: row.get::<_, i64>(10)? != 0,
                })
            })
            .context("query sync_devices")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read sync device row")?);
        }
        Ok(results)
    }

    /// Record a sync change. Returns the sequence number.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_sync_change(
        &self,
        actor_id: &[u8; 32],
        path_hash: &[u8; 32],
        manifest_hash: Option<&[u8; 32]>,
        size_bytes: i64,
        change_type: &str,
        folder_id: Option<i64>,
        device_id: Option<&[u8; 32]>,
        path: Option<&str>,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let path_hash = *path_hash;
        let manifest_hash = manifest_hash.copied();
        let device_id = device_id.copied();
        let change_type = change_type.to_string();
        let path = path.map(|s| s.to_string());
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                actor_id.as_slice(),
                path_hash.as_slice(),
                manifest_hash.as_ref().map(|h| h.as_slice()),
                size_bytes,
                change_type,
                now,
                folder_id,
                device_id.as_ref().map(|d| d.as_slice()),
                path,
            ],
        )
        .context("record sync change")?;
        Ok(conn.last_insert_rowid())
    }

    /// Client-facing manifest record with atomic per-actor storage-quota
    /// accounting **and** enforcement — the metered twin of
    /// [`Self::record_sync_change`] (which stays unmetered for nest-internal
    /// bookkeeping: index writer, backup snapshots, restore, GC). Every client
    /// recording door calls this (`fauna.sync.changes.record` among them), so a
    /// client can't dodge the cap by picking a door.
    ///
    /// Under one `conn` lock (check + apply is atomic — no TOCTOU window),
    /// **retained accounting** (`file-versions.md` § Retention (4), slice 3):
    /// a content record charges its FULL `size_bytes` — the former head stays
    /// listable as a retained version, so nothing is released at record time —
    /// and a `delete` charges nothing and refunds nothing (net zero: the
    /// former head keeps charging until the § Retention (3) pipeline releases
    /// it, via [`adjust_version_accounting_in_conn`]). Rejects with
    /// [`StorageQuotaError::Exceeded`] iff the charge would push the owner
    /// past `max_storage_bytes` — a *modify*, shrink included, can refuse; a
    /// delete and an idempotent re-record always succeed — else appends the
    /// `sync_changes` row and moves `users.storage_bytes_used` by the charge.
    /// Returns the seq.
    ///
    /// Idempotence: a re-record of the path's exact head content returns the
    /// existing seq and charges nothing (the content check below), so the rare
    /// case where both client paths observe the same change can't double-count.
    /// Multi-writer Phase 1 (owner-pays; `file-sync.md` § Multi-writer shared
    /// sets): `recorder` is the authenticated connection actor and lands on the
    /// row's `actor_id` (attribution); `owner` is the set owner, the **metered**
    /// actor — the delta always moves the *owner's* `storage_bytes_used`, and
    /// `max_storage_bytes` is the *owner's* ceiling. When the recorder is a
    /// member (`recorder != owner`), `member_channel` names the set's derived
    /// channel: the same locked step bumps the member's `folder_member_access.
    /// bytes_used` (floored at 0) and refuses [`StorageQuotaError::
    /// MemberCapExceeded`] before anything is written when a positive delta
    /// would pass their `byte_cap`. Owner devices pass `member_channel = None`
    /// and bypass the cap.
    ///
    /// `path_sealed` is the client's opaque `SealedLabel` envelope over the
    /// path (`docs/goal/behavior/file-sync.md` § Sealed names & paths) —
    /// stored verbatim, never inspected. `path` here is the REST value, not
    /// the wire value: since the S9 flip the caller passes `Some` plaintext
    /// only for a `web`-mode set (public paths by design — their exemption);
    /// every other plane rests `NULL` and the seal is the only label
    /// (`record_change_core` enforces its presence).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_sync_change_metered(
        &self,
        recorder: &[u8; 32],
        owner: &[u8; 32],
        member_channel: Option<&[u8; 32]>,
        path_hash: &[u8; 32],
        manifest_hash: Option<&[u8; 32]>,
        size_bytes: i64,
        change_type: &str,
        folder_id: i64,
        device_id: &[u8; 32],
        path: Option<&str>,
        content_key_version: Option<i64>,
        thumbnail_hash: Option<&str>,
        path_sealed: Option<&[u8]>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
        max_storage_bytes: i64,
    ) -> Result<i64, StorageQuotaError> {
        self.record_sync_change_metered_signed(
            recorder,
            owner,
            member_channel,
            path_hash,
            manifest_hash,
            size_bytes,
            change_type,
            folder_id,
            device_id,
            path,
            content_key_version,
            thumbnail_hash,
            path_sealed,
            derived_through,
            is_resolution,
            max_storage_bytes,
            None,
        )
        .await
    }

    /// [`Self::record_sync_change_metered`] carrying the writer's signature
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*) —
    /// the caller has already verified it (`record_change_core`); this stores
    /// it on the row verbatim.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_sync_change_metered_signed(
        &self,
        recorder: &[u8; 32],
        owner: &[u8; 32],
        member_channel: Option<&[u8; 32]>,
        path_hash: &[u8; 32],
        manifest_hash: Option<&[u8; 32]>,
        size_bytes: i64,
        change_type: &str,
        folder_id: i64,
        device_id: &[u8; 32],
        path: Option<&str>,
        content_key_version: Option<i64>,
        thumbnail_hash: Option<&str>,
        path_sealed: Option<&[u8]>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
        max_storage_bytes: i64,
        signature: Option<RowSignature<'_>>,
    ) -> Result<i64, StorageQuotaError> {
        let conn = self.conn.lock().await;
        Self::record_sync_change_in_conn(
            &conn,
            recorder,
            owner,
            member_channel,
            path_hash,
            manifest_hash,
            size_bytes,
            change_type,
            folder_id,
            device_id,
            path,
            content_key_version,
            thumbnail_hash,
            path_sealed,
            derived_through,
            is_resolution,
            max_storage_bytes,
            signature,
        )
    }

    /// [`Self::record_sync_change_metered`]'s whole body, over a **caller-held**
    /// connection.
    ///
    /// Extracted so the covered-folder materialize arm can re-home its rows
    /// inside its own transaction: the empty-target rule has to be re-asserted
    /// *at the write* rather than only at the top of the verb (the segments
    /// arm's 2026-08-30 hardening, applied to the folder axis), and that means
    /// the insert must compose into a transaction this function does not own.
    /// A second row writer was the alternative, and it would have forked the
    /// content-idempotence check, the retained accounting, the owner ceiling and
    /// the member cap away from the door every other record comes through. There
    /// is exactly one metered *record* insert in the tree, and this is it; the
    /// conflict doors that mint charged rows (the pre-resolved report's two rows,
    /// the choose-winner head) share its refusals, its charge and its
    /// same-manifest transfer through [`vet_row_charge_in_conn`] /
    /// [`settle_row_charge_in_conn`] rather than a copy.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_sync_change_in_conn(
        conn: &rusqlite::Connection,
        recorder: &[u8; 32],
        owner: &[u8; 32],
        member_channel: Option<&[u8; 32]>,
        path_hash: &[u8; 32],
        manifest_hash: Option<&[u8; 32]>,
        size_bytes: i64,
        change_type: &str,
        folder_id: i64,
        device_id: &[u8; 32],
        path: Option<&str>,
        content_key_version: Option<i64>,
        thumbnail_hash: Option<&str>,
        path_sealed: Option<&[u8]>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
        max_storage_bytes: i64,
        signature: Option<RowSignature<'_>>,
    ) -> Result<i64, StorageQuotaError> {
        // The declaration IS the meter on this plane, so it is checked before
        // anything is read or written — under retained accounting the `charge`
        // below simply IS this number, so a negative one rides straight past the
        // `charge > 0` ceiling check and then *credits* the owner, resetting a
        // full account to 0 for the price of one record
        // ([`StorageQuotaError::NegativeSize`]). Refused for a `delete`
        // too, though that arm ignores the value: "negative is fine as long as
        // you also say delete" is a carve-out every future reader would have to
        // re-derive, and no honest engine sends one (they send 0).
        if size_bytes < 0 {
            return Err(StorageQuotaError::NegativeSize { size_bytes });
        }
        let actor_id = *recorder;
        let owner = *owner;
        let member_channel = member_channel.copied();
        let path_hash = *path_hash;
        let manifest_hash = manifest_hash.copied();
        let device_id = *device_id;
        let change_type = change_type.to_string();
        let path = path.map(|s| s.to_string());
        let thumbnail_hash = thumbnail_hash.map(|s| s.to_string());
        let path_sealed = path_sealed.map(<[u8]>::to_vec);
        let now = now_epoch_millis();

        // What this record contributes to the path's live size (a `delete` —
        // signalled by change_type or a null manifest — clears the path).
        let is_delete = change_type == "delete" || manifest_hash.is_none();
        let new_size = if is_delete { 0 } else { size_bytes };

        // The path's HEAD record — its newest row over ALL rows for the path,
        // ordered by seq. It answers two questions from one read (so dedupe and
        // metering can never disagree about which row is the head — a divergence
        // would reintroduce exactly the double-charge this dedupe closes):
        //   1. the content-idempotence check below, and
        //   2. the path's current live size for the delta.
        // Ordering over ALL rows (not `change_type != 'delete'`) is load-bearing:
        // a delete recorded after a create must be seen AS the head, or a
        // create→delete→recreate cycle reads the stale create row as prior and
        // charges a zero delta (unbounded storage-quota evasion). Superseded
        // rows are always strictly older than the head
        // (`supersede_sync_changes_for_path` only marks `seq < head_seq`), so
        // `ORDER BY seq DESC LIMIT 1` never reads one.
        #[allow(clippy::type_complexity)]
        let head: Option<(
            i64,
            Vec<u8>,
            Option<Vec<u8>>,
            String,
            Option<i64>,
            i64,
            Option<Vec<u8>>,
        )> = conn
            .query_row(
                "SELECT seq, actor_id, manifest_hash, change_type, content_key_version, size_bytes,
                        signature
                 FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![folder_id, path_hash.as_slice()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()
            .context("query head sync_change for path")?;
        let new_signature: Option<&[u8]> = signature.map(|s| s.signature);

        // ── Exactly-once by CONTENT (`federation.md` § Cross-nest…, contract
        // point (i)). A record whose (acting actor, path_hash,
        // manifest_hash, change_type, content_key_version) equals the path's
        // latest entry is already-applied — a reconnect / >L3-TTL redelivery /
        // crashed-ack retry, or a same-nest client double-send. Return the
        // existing seq and charge nothing, BEFORE the quota/cap checks and the
        // insert. The L3 idempotency cache is per-connection and cannot span a
        // reconnect, so this durable content check is the only exactly-once
        // guarantee the federated record relay may rely on — and the same-nest
        // path gets it identically (uniform shape). The head lookup above keys
        // on `path_hash` (path-sealing S2 re-keyed it off the plaintext column,
        // which stops being written at the major-gated flip); the two are 1:1
        // by blake3, so the identity of this check is unchanged.
        //
        // `path_sealed` is deliberately NOT part of the compare: a convergent
        // seal is a pure function of (root, path), so it carries no information
        // this tuple lacks — but it DOES change when the set's content key
        // rotates, and folding it in would turn a post-rotation re-record of
        // identical content into a non-replay, double-charging the owner and
        // breaking the exactly-once guarantee the federated relay depends on.
        //
        // The writer SIGNATURE is part of the compare, unlike the seal, and the
        // difference is the point: a genuine retry re-sends the same signature
        // (Ed25519 is deterministic over one statement), while a record that
        // differs only in its signature is a NEW statement — a re-record under
        // the owner's live set nonce (`mls-group-key-material.md` § M2 →
        // *Writer-signed change records*, custody (g): "a re-sign with no
        // re-seal and no chunk moved"), or a signed record over an unsigned
        // head. Folding it into the old row would leave the path headed by a
        // row every reader skips.
        if let Some((head_seq, head_actor, head_manifest, head_change_type, head_ckv, _, head_sig)) =
            &head
            && head_actor.as_slice() == actor_id.as_slice()
            && head_manifest.as_deref() == manifest_hash.as_ref().map(|h| h.as_slice())
            && *head_change_type == change_type
            && *head_ckv == content_key_version
            && head_sig.as_deref() == new_signature
        {
            return Ok(*head_seq);
        }

        // ── Nest-side idempotency guard for a repeated DELETE of an
        // already-tombstoned path (`delete-propagation.md` § Deletes
        // propagate the same way, guard (2)). Unlike the exactly-once
        // check above — deliberately keyed on the ACTING ACTOR, because two
        // actors authoring identical *content* are still distinct authored
        // versions — a delete carries no content to differ by author: two
        // actors deleting the same already-gone path converge on the
        // identical "nothing here" state. So this check is actor-independent
        // by design. Without it, a cross-actor echo-delete (a stale client
        // replaying a delete another device already recorded, or a member
        // and the owner racing the same delete) would append a redundant
        // tombstone row and re-fire the remote-change nudge at every other
        // device for a change that already landed — wasteful, and the exact
        // shape the client-side re-record guard already declines locally
        // (`libs/fauna-sync-engine`'s `handle_delete`); this is the same rule
        // enforced durably, against a non-conforming client that lacks it.
        //
        // A signed delete over an UNSIGNED tombstone is not an echo: it is the
        // one delete of that path any reader will ever verify, so it lands.
        // Nor is one over a tombstone signed under a nonce the set no longer
        // stores: that tombstone verifies nowhere as current
        // (`writer-signed-change-records.md` custody (e)), and this delete is
        // the owner's reconcile re-recording it under the live nonce (custody
        // (g)) — answering it with the old `seq` would leave every reader
        // folding whatever lies beneath. Two signed deletes under the live
        // nonce still dedupe whoever signed them: only the head's own
        // signature is asked about, never compared with the incoming one.
        if is_delete
            && let Some((head_seq, _, head_manifest, head_change_type, _, _, head_sig)) = &head
            && (head_change_type == "delete" || head_manifest.is_none())
            && (new_signature.is_none()
                || (head_sig.is_some()
                    && head_signed_under_stored_nonce_in_conn(conn, folder_id, *head_seq)?))
        {
            return Ok(*head_seq);
        }

        // Retained accounting (`file-versions.md` § Retention (4), slice 3):
        // the former head stays LISTABLE as a retained version — nothing is
        // released at record time — so a content record charges its FULL
        // size, and a delete charges nothing and refunds nothing (net zero by
        // design: the former head keeps charging until the pipeline in
        // § Retention (3) actually releases it). Consequence stated in the §:
        // a modify — shrink included — can now refuse at the quota; only a
        // delete and an idempotent re-record always succeed. The head read
        // above still feeds the two idempotence checks.
        let charge = new_size;
        // The one content record that adds no bytes: a record over the
        // manifest the path's head already holds — a re-sign under a new
        // nonce or by a successor, another writer recording identical content
        // (`writer-signed-change-records.md` ruling (11)(g)). The charge
        // follows the (path, manifest) pair, held by exactly one listable row,
        // so this record TAKES it from the row that holds it rather than
        // adding a second: that row's charge is released (its recorder's
        // member half with it), the new row is charged, and the refusals
        // below are asked about the NET move — zero for the owner when the
        // declared sizes agree, so a move is never refused at a full quota.
        // Skipping the charge instead is the refused shape: the former head,
        // pruned by retention, would credit bytes the live head still holds.
        let heads_manifest = match (&head, manifest_hash.as_ref()) {
            (Some((_, _, Some(head_manifest), head_change_type, ..)), Some(manifest))
                if !is_delete
                    && head_change_type != "delete"
                    && head_manifest.as_slice() == manifest.as_slice() =>
            {
                Some(manifest.as_slice())
            }
            _ => None,
        };
        let row_charge = vet_row_charge_in_conn(
            conn,
            folder_id,
            &path_hash,
            heads_manifest,
            &owner,
            &actor_id,
            member_channel.as_ref(),
            charge,
            max_storage_bytes,
        )?;

        conn.execute(
            "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path, content_key_version, thumbnail_hash, path_sealed, derived_through, is_resolution, signature, signer_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            rusqlite::params![
                actor_id.as_slice(),
                path_hash.as_slice(),
                manifest_hash.as_ref().map(|h| h.as_slice()),
                size_bytes,
                change_type,
                now,
                folder_id,
                device_id.as_slice(),
                path,
                content_key_version,
                thumbnail_hash,
                path_sealed.as_deref(),
                derived_through,
                is_resolution.map(i64::from),
                new_signature,
                signature.map(|s| s.signer_key),
            ],
        )
        .context("record sync change (metered)")?;
        let seq = conn.last_insert_rowid();

        settle_row_charge_in_conn(
            conn,
            folder_id,
            &owner,
            &actor_id,
            member_channel.as_ref(),
            &row_charge,
        )?;
        Ok(seq)
    }
}

/// What a content row about to be minted owes, as [`vet_row_charge_in_conn`]
/// vetted it: its declared size, and the listable row it takes that charge
/// from when it lands over the manifest the path's head already holds.
pub(crate) struct RowCharge {
    size_bytes: i64,
    held: Option<ChargedVersion>,
}

/// The metering refusals every charged `sync_changes` insert passes BEFORE it
/// writes (`file-versions.md` § Retention (4)), with the same-manifest
/// transfer of `writer-signed-change-records.md` ruling (11)(g) folded in.
/// Shared by [`CacheDb::record_sync_change_in_conn`] and the conflict doors
/// that mint charged rows (`report_conflict_signed`,
/// `resolve_conflict_choose_winner_signed`) so there is one set of refusals
/// and one transfer, not one per door; each caller refuses a negative
/// declaration itself first.
///
/// `heads_manifest` is the row's manifest when — and only when — it equals the
/// one the path's head holds (the caller's head read decides; `None`
/// otherwise). Such a row adds no bytes: it TAKES the charge from the listable
/// row of the (path, manifest) pair that holds it, and the refusals are asked
/// about the NET move — zero for the owner when the declared sizes agree, so a
/// move is never refused at a full quota, while a member recorder who did not
/// hold that charge is asked about the whole of it. Pass the result to
/// [`settle_row_charge_in_conn`] after the insert.
#[allow(clippy::too_many_arguments)]
pub(crate) fn vet_row_charge_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8],
    heads_manifest: Option<&[u8]>,
    owner: &[u8; 32],
    recorder: &[u8; 32],
    member_channel: Option<&[u8; 32]>,
    size_bytes: i64,
    max_storage_bytes: i64,
) -> Result<RowCharge, StorageQuotaError> {
    let held = match heads_manifest {
        Some(manifest) => charged_row_for_manifest_in_conn(conn, folder_id, path_hash, manifest)?,
        None => None,
    };
    let (owner_net, member_net) = match &held {
        Some(held) => (
            size_bytes.saturating_sub(held.size_bytes),
            if held.actor.as_slice() == recorder.as_slice() {
                size_bytes.saturating_sub(held.size_bytes)
            } else {
                size_bytes
            },
        ),
        None => (size_bytes, size_bytes),
    };
    check_net_charge_in_conn(
        conn,
        owner,
        recorder,
        member_channel,
        owner_net,
        member_net,
        max_storage_bytes,
    )?;
    Ok(RowCharge { size_bytes, held })
}

/// Moves the counters [`vet_row_charge_in_conn`] vetted, in the same
/// transaction as the insert it charges for. The transfer's release half goes
/// first, so the floor at zero never eats part of a move: the row that held
/// the pair's charge is flagged off and credited, exactly as if it had left
/// the population (its recorder's member half with it); then the new row is
/// charged in full.
pub(crate) fn settle_row_charge_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    owner: &[u8; 32],
    recorder: &[u8; 32],
    member_channel: Option<&[u8; 32]>,
    charge: &RowCharge,
) -> Result<()> {
    if let Some(held) = &charge.held {
        conn.execute(
            "UPDATE sync_changes SET charged = 0 WHERE seq = ?1",
            rusqlite::params![held.seq],
        )
        .context("clear the charged flag on the row a mint took its charge from")?;
        adjust_version_accounting_in_conn(
            conn,
            folder_id,
            &[(held.actor.clone(), held.size_bytes)],
            -1,
        )?;
    }
    apply_charge_in_conn(conn, owner, recorder, member_channel, charge.size_bytes)
}

/// The manifest the path's content head holds — the newest row of the path
/// over all rows, as the record door reads its head, and not a delete — for
/// the conflict doors' [`vet_row_charge_in_conn`]. Read it BEFORE the door
/// mints anything: a report's loser retention row lands ahead of its winner
/// and is no head (`conflicts.md` § *Retention rows are transparent to the
/// licence*), so the winner is asked against the head the path held when the
/// report arrived.
fn content_head_manifest_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    path_hash: &[u8],
) -> Result<Option<Vec<u8>>> {
    let head: Option<(Option<Vec<u8>>, String)> = conn
        .query_row(
            "SELECT manifest_hash, change_type FROM sync_changes
             WHERE folder_id = ?1 AND path_hash = ?2
             ORDER BY seq DESC LIMIT 1",
            rusqlite::params![folder_id, path_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read the path head for a conflict door's charge")?;
    Ok(match head {
        Some((Some(head_manifest), change_type)) if change_type != "delete" => Some(head_manifest),
        _ => None,
    })
}

/// The refusals behind [`vet_row_charge_in_conn`], with the two counters'
/// moves named apart: a same-manifest mint takes a charge the owner's meter
/// already carries (`writer-signed-change-records.md` ruling (11)(g)), so the
/// owner's ceiling is asked about the net move while a member recorder who did
/// not hold that charge is asked about the whole of it. A `charge` past the
/// OWNER's `max_storage_bytes` refuses [`StorageQuotaError::Exceeded`], and —
/// when the recorder is a member (`member_channel` names the set's derived
/// channel) — a `member_charge` past their `folder_member_access.byte_cap`
/// refuses [`StorageQuotaError::MemberCapExceeded`]. Only a positive charge
/// refuses.
fn check_net_charge_in_conn(
    conn: &rusqlite::Connection,
    owner: &[u8; 32],
    recorder: &[u8; 32],
    member_channel: Option<&[u8; 32]>,
    charge: i64,
    member_charge: i64,
    max_storage_bytes: i64,
) -> Result<(), StorageQuotaError> {
    let actor_id = *recorder;
    // Owner-pays: the ceiling check and the charge below both key on the
    // OWNER's row, whoever records.
    let used: i64 = conn
        .query_row(
            "SELECT storage_bytes_used FROM users WHERE actor_id = ?1",
            rusqlite::params![owner.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .context("read storage_bytes_used")?
        .unwrap_or(0);
    // `checked_add`, not `+`: the declaration IS the meter, and each caller's
    // negative-size guard only bounds it from BELOW. A declared size near `i64::MAX`
    // overflows this sum — which panics in a debug/test build but, in the
    // shipped RELEASE profile (`overflow-checks` defaults off), wraps to a
    // NEGATIVE value that sails through this very comparison. The record is
    // then accepted and charged, `users.storage_bytes_used + <huge>`
    // overflows SQLite's integer arithmetic too, and the column silently
    // becomes REAL — after which every `i64` read of it fails, taking out
    // this owner's whole record plane AND the nest-wide admin user listing,
    // with no client affordance able to write the column back. That is a
    // client-causable unrecoverable nest state (`nest/common.md`
    // § Client-state recoverability), so an overflowing charge is refused
    // here as what it is: over the ceiling. No new wire code — a sum that
    // cannot be represented is unambiguously `Exceeded`.
    if charge > 0
        && used
            .checked_add(charge)
            .is_none_or(|total| total > max_storage_bytes)
    {
        return Err(StorageQuotaError::Exceeded {
            used,
            requested: charge,
            max: max_storage_bytes,
        });
    }

    // Member cap (multi-writer Phase 1): refuse BEFORE any write, so a
    // refusal charges nothing anywhere. A missing role row means an
    // ungranted recorder — the write gate upstream should have refused, so
    // treat it as cap 0 (fail closed) rather than uncapped.
    if let Some(channel) = member_channel {
        let row: Option<(Option<i64>, i64)> = conn
            .query_row(
                "SELECT byte_cap, bytes_used FROM folder_member_access
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel.as_slice(), actor_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read member role for cap check")?;
        let (byte_cap, member_used) = match row {
            Some((cap, used)) => (cap, used),
            None => (Some(0), 0),
        };
        // `checked_add` for the same reason as the owner ceiling above: an
        // overflowing sum must read as over-cap, never wrap past the check.
        if member_charge > 0
            && let Some(cap) = byte_cap
            && member_used
                .checked_add(member_charge)
                .is_none_or(|total| total > cap)
        {
            return Err(StorageQuotaError::MemberCapExceeded {
                used: member_used,
                requested: member_charge,
                cap,
            });
        }
    }

    Ok(())
}

/// The charge half of [`settle_row_charge_in_conn`]: moves the OWNER's
/// `users.storage_bytes_used` by `charge` and, for a member recorder, their
/// `folder_member_access.bytes_used` (the abuse counter, not attribution) by
/// the same amount — both through [`saturating_counter_move_sql`]. The release
/// credits ([`adjust_version_accounting_in_conn`]) are what move them back
/// down.
///
/// The vet's `checked_add` refusals do not bound every move made here: an
/// uncapped member (`byte_cap` NULL, the default share) is asked about no cap,
/// and a same-manifest transfer asks the owner only about the NET move while
/// the member half takes the whole size. Once an undelete has saturated a
/// member's counter (its re-charge never refuses), a net-zero transfer would
/// add the full size past `i64::MAX` and turn the member's `bytes_used`, and
/// with it the folder's roster read, REAL.
fn apply_charge_in_conn(
    conn: &rusqlite::Connection,
    owner: &[u8; 32],
    recorder: &[u8; 32],
    member_channel: Option<&[u8; 32]>,
    charge: i64,
) -> Result<()> {
    if charge == 0 {
        return Ok(());
    }
    conn.execute(
        &format!(
            "UPDATE users SET storage_bytes_used = {} WHERE actor_id = ?2",
            saturating_counter_move_sql("storage_bytes_used")
        ),
        rusqlite::params![charge, owner.as_slice()],
    )
    .context("update storage_bytes_used")?;
    if let Some(channel) = member_channel {
        conn.execute(
            &format!(
                "UPDATE folder_member_access SET bytes_used = {}
                 WHERE channel_id = ?2 AND actor_id = ?3",
                saturating_counter_move_sql("bytes_used")
            ),
            rusqlite::params![charge, channel.as_slice(), recorder.as_slice()],
        )
        .context("update member bytes_used")?;
    }
    Ok(())
}

/// Who pays for a charged row a conflict door mints into `folder_id`, read
/// inside the door's own transaction: the set OWNER (the metered actor), the
/// member channel when `recorder` is not the owner, and the owner's tier
/// `max_storage_bytes` (a missing tier reads as uncapped — the same fail-open
/// the record door's `get_user_tier_max_storage_bytes` lookup documents).
/// Owner-pays + member-abuse, exactly the split `fauna.sync.changes.record`
/// meters under (`file-sync.md` § Multi-writer shared sets). Derived here
/// rather than passed by the handler so no caller can mint a conflict row
/// without the charge.
struct ConflictMeter {
    owner: [u8; 32],
    member_channel: Option<[u8; 32]>,
    max_storage_bytes: i64,
}

fn conflict_meter_in_conn(
    conn: &rusqlite::Connection,
    folder_id: i64,
    recorder: &[u8; 32],
) -> Result<ConflictMeter> {
    let (owner, mls_group_id): (Vec<u8>, Option<Vec<u8>>) = conn
        .query_row(
            "SELECT actor_id, mls_group_id FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("read folder owner for conflict metering")?;
    let owner: [u8; 32] = owner
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("folder owner id is not 32 bytes"))?;
    let member_channel = if owner == *recorder {
        None
    } else {
        // A non-owner reporter passed the writer gate, which only a
        // group-bound set can grant; fail closed rather than meter uncapped.
        let group_id = mls_group_id
            .ok_or_else(|| anyhow::anyhow!("writer-writable set has no mls_group_id"))?;
        Some(fauna_mls::types::ChannelId::from_group_id(&group_id).0)
    };
    let max_storage_bytes = conn
        .query_row(
            "SELECT t.max_storage_bytes FROM users u JOIN tiers t ON u.tier = t.name
             WHERE u.actor_id = ?1",
            rusqlite::params![owner.as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .context("read owner tier max_storage_bytes for conflict metering")?
        .unwrap_or(i64::MAX);
    Ok(ConflictMeter {
        owner,
        member_channel,
        max_storage_bytes,
    })
}

impl CacheDb {
    /// One actor's change rows since a sequence number, across every set —
    /// a test probe. No handler serves it: `fauna.sync.changes.list` reads a
    /// folder's feed, an item class's, or a cross-nest route's, and refuses a
    /// request naming none.
    ///
    /// Superseded rows (`superseded_at IS NOT NULL`) and state-entry rows are
    /// excluded, as on the folder feed.
    #[cfg(test)]
    pub(crate) async fn get_sync_changes(
        &self,
        actor_id: &[u8; 32],
        since_seq: i64,
    ) -> Result<Vec<SyncChangeRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SYNC_CHANGE_COLUMNS}
                 FROM sync_changes WHERE actor_id = ?1 AND seq > ?2 AND superseded_at IS NULL
                   AND (item_class IS NULL OR item_class != ?3) ORDER BY seq",
            ))
            .context("prepare get_sync_changes")?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    actor_id.as_slice(),
                    since_seq,
                    fauna_protocol::account_state::ItemClass::StateEntry.as_wire(),
                ],
                sync_change_row_from_sql,
            )
            .context("query sync_changes")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read sync change row")?);
        }
        Ok(results)
    }

    // ==================== Folders ====================

    pub async fn create_folder(&self, name: &str, actor_id: &[u8; 32]) -> Result<i64> {
        let actor_id = *actor_id;
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO folders (name, actor_id, created_at, name_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                name,
                actor_id.as_slice(),
                now,
                fauna_core::path_crypto::set_name_hash(&name).as_slice()
            ],
        )
        .context("create folder")?;
        Ok(conn.last_insert_rowid())
    }

    /// Test-only twin of the boot hash-companion pass
    /// (`reconcile_path_sealing_companions`, which stamps reserved `__` folders
    /// every boot): stamps `name_hash` on an existing row, arranging the shape a
    /// fresh internal mint doesn't have yet (`get_or_create_reserved_folder`
    /// leaves `name_hash` NULL). Lets a pin hash-address a reserved rail the way
    /// a booted production nest does.
    #[cfg(test)]
    pub async fn stamp_folder_name_hash_like_the_backfill(
        &self,
        name: &str,
        actor_id: &[u8; 32],
    ) -> Result<()> {
        let name = name.to_string();
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE folders SET name_hash = ?1 WHERE name = ?2 AND actor_id = ?3",
            rusqlite::params![
                fauna_core::path_crypto::set_name_hash(&name).to_vec(),
                name,
                actor_id.as_slice()
            ],
        )
        .context("stamp name_hash (test backfill twin)")?;
        Ok(())
    }

    /// Test-fixture convenience: the FIRST row named `name`, whichever actor
    /// owns it. `folders` is unique on `(name, actor_id)` — not `name` alone
    /// — so this lookup is ambiguous the moment two actors own same-named
    /// sets, and no production path may use it. Production lookups are
    /// actor-scoped: [`Self::get_folder_for_actor`] (one actor's row) or
    /// [`Self::get_folders_by_name`] (every actor's row, for the admin
    /// by-name kinds' honest ambiguity error). `#[cfg(test)]` keeps the
    /// misuse unrepresentable; in-crate unit tests (single-actor fixtures)
    /// are the only callers.
    #[cfg(test)]
    pub async fn get_folder(&self, name: &str) -> Result<Option<FolderRow>> {
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name = ?1"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![name], folder_row_from)?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// EVERY actor's row named `name`, ordered by `actor_id` for determinism.
    ///
    /// `folders` is unique on `(name, actor_id)`, so a name can match one row
    /// per actor. The admin by-name kinds
    /// (`fauna.admin.folders.{get,add_member,add_destination}`) use this to
    /// detect a cross-actor name collision and error honestly, instead of the
    /// name-only [`Self::get_folder`]'s failure mode of returning whichever
    /// actor's row happens to come first.
    /// EVERY actor's row whose `name_hash` matches, ordered by `actor_id` — the
    /// hash-keyed twin of [`Self::get_folders_by_name`] (S5,
    /// `file-sync.md` § Sealed names & paths).
    ///
    /// `UNIQUE(name_hash, actor_id)` means a hash matches at most one row per
    /// actor, exactly as a name does, so the caller's 0/1/many ambiguity handling
    /// is identical for both arms. This is how an admin addresses a set once the
    /// flip scrubs the plaintext they cannot read.
    pub async fn get_folders_by_name_hash(&self, name_hash: &[u8; 32]) -> Result<Vec<FolderRow>> {
        let name_hash = name_hash.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name_hash = ?1 ORDER BY actor_id"
        ))?;
        let rows = stmt.query_map(rusqlite::params![name_hash], folder_row_from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One actor's row addressed by `name_hash` — the hash-keyed twin of
    /// [`Self::get_folder_for_actor`]. Exact by `UNIQUE(name_hash, actor_id)`.
    pub async fn get_folder_for_actor_by_name_hash(
        &self,
        name_hash: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<FolderRow>> {
        let name_hash = name_hash.to_vec();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name_hash = ?1 AND actor_id = ?2"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![name_hash, actor_id], |row| {
            folder_row_from(row)
        })?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub async fn get_folders_by_name(&self, name: &str) -> Result<Vec<FolderRow>> {
        if name.is_empty() {
            return Ok(Vec::new());
        }
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name = ?1 ORDER BY actor_id"
        ))?;
        let rows = stmt.query_map(rusqlite::params![name], folder_row_from)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Like [`Self::get_folder`] but scoped to a specific owning `actor_id`.
    ///
    /// `folders` is unique on `(name, actor_id)`, not `name` alone —
    /// reserved sets (`__mls`, `__mail`, `__index`, …) are per-actor
    /// collections (file-sync.md § Reserved folders), so two actors on one
    /// nest can each own a row with the same `name`. Look a *reserved* set up
    /// with this, never the name-only [`Self::get_folder`], which would
    /// return whichever actor's row happens to sort first.
    ///
    /// **An empty name names no set** — here, in [`Self::get_folders_by_name`]
    /// and in [`Self::get_group_bound_folders_by_name`]. A sealed set is
    /// addressed by its `name_hash` and its plaintext name is not an address it
    /// keeps (`path-sealing.md` § the set-name plane), so `''` — a hash-addressed
    /// request's empty `name`, or a blanked row — must never fall through to
    /// whichever blank row matches first.
    pub async fn get_folder_for_actor(
        &self,
        name: &str,
        actor_id: &[u8; 32],
    ) -> Result<Option<FolderRow>> {
        if name.is_empty() {
            return Ok(None);
        }
        let name = name.to_string();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name = ?1 AND actor_id = ?2"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![name, actor_id], |row| {
            folder_row_from(row)
        })?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// All **group-bound** (`mls_group_id IS NOT NULL`) folders named `name` —
    /// the candidate rows a non-owner *member* might be authorized to read (S2-P3).
    ///
    /// `folders` is unique on `(name, actor_id)`, so a name can match several
    /// rows owned by different users. A member B addresses a shared set by name
    /// (B is not the owner, so the name-only [`Self::get_folder`] / the
    /// owner-scoped [`Self::get_folder_for_actor`] don't reach it); the read
    /// authz resolver walks these candidates and admits B iff B is on the derived
    /// `ChannelId::from_group_id(mls_group_id)` roster. Owner-only rows
    /// (`mls_group_id IS NULL`) are excluded — they are never member-readable.
    pub async fn get_group_bound_folders_by_name(&self, name: &str) -> Result<Vec<FolderRow>> {
        if name.is_empty() {
            return Ok(Vec::new());
        }
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name = ?1 AND mls_group_id IS NOT NULL"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![name], folder_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Like [`Self::get_group_bound_folders_by_name`] but addressed by
    /// `name_hash` — the hash-keyed twin for a member who cannot read the
    /// owner's sealed name once the plaintext scrubs (S5b,
    /// `file-sync.md` § Sealed names & paths). Same candidate-list shape (a
    /// hash can match several owners' rows, exactly as a name can); the caller
    /// walks it identically.
    pub async fn get_group_bound_folders_by_name_hash(
        &self,
        name_hash: &[u8; 32],
    ) -> Result<Vec<FolderRow>> {
        let name_hash = name_hash.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE name_hash = ?1 AND mls_group_id IS NOT NULL"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![name_hash], folder_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// **Every** group-bound (`mls_group_id IS NOT NULL`) folder, across all
    /// owners — the candidate rows for cross-set readable-set enumeration
    /// (`fauna.media.list`). The caller filters these by roster membership
    /// (`folder_authz::can_read_folder`); this query does NOT itself gate, so
    /// it must only feed an authz filter, never be returned raw. Owner-only rows
    /// (`mls_group_id IS NULL`) are reached via the per-owner
    /// [`Self::get_folders_for_actor_full`] instead.
    pub async fn get_group_bound_folders(&self) -> Result<Vec<FolderRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE mls_group_id IS NOT NULL"
        ))?;
        let rows = stmt
            .query_map([], folder_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub async fn get_folder_by_id(&self, id: i64) -> Result<Option<FolderRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![id], folder_row_from)?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub async fn add_folder_member(
        &self,
        folder_id: i64,
        device_id: &[u8; 32],
        flags: &PlaceFlags,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        upsert_folder_place(&conn, folder_id, device_id.as_slice(), flags)
    }

    pub async fn get_folder_members(&self, folder_id: i64) -> Result<Vec<FolderMemberRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT device_id, originates, accepts, applies_deletes
               FROM folder_members WHERE folder_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![folder_id], |row| {
            Ok(FolderMemberRow {
                device_id: row.get(0)?,
                flags: place_flags_from_row(row, 1)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Create a folder with an explicit `node_cache` (the admin create's
    /// optional knob; [`Self::create_folder`] leaves the column at its `0`
    /// default).
    pub async fn create_folder_with_node_cache(
        &self,
        name: &str,
        actor_id: &[u8; 32],
        node_cache: bool,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO folders (name, actor_id, created_at, node_cache, name_hash) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, actor_id.as_slice(), now, node_cache as i64, fauna_core::path_crypto::set_name_hash(&name).as_slice()],
        ).context("create folder with node_cache")?;
        Ok(conn.last_insert_rowid())
    }

    // The `folder_destinations` rail's five accessors lived here — deleted
    // with the phantom rail (folders re-model plan § Migration, executed
    // 2026-08-18): its only writer was an admin verb no app or daemon ever
    // called, so every production read saw an empty list.

    pub async fn get_sync_changes_for_folder(
        &self,
        folder_id: i64,
        since_seq: i64,
        exclude_device_id: Option<&[u8; 32]>,
    ) -> Result<Vec<SyncChangeRow>> {
        let exclude = exclude_device_id.copied();
        let conn = self.conn.lock().await;

        let state_entry = fauna_protocol::account_state::ItemClass::StateEntry.as_wire();
        if let Some(ref excl) = exclude {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SYNC_CHANGE_COLUMNS}
                 FROM sync_changes
                 WHERE folder_id = ?1 AND seq > ?2 AND (device_id IS NULL OR device_id != ?3)
                   AND superseded_at IS NULL
                   AND (item_class IS NULL OR item_class != ?4)
                 ORDER BY seq",
            ))?;
            let rows = stmt.query_map(
                rusqlite::params![folder_id, since_seq, excl.as_slice(), state_entry],
                sync_change_row_from_sql,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        } else {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SYNC_CHANGE_COLUMNS}
                 FROM sync_changes
                 WHERE folder_id = ?1 AND seq > ?2 AND superseded_at IS NULL
                   AND (item_class IS NULL OR item_class != ?3)
                 ORDER BY seq",
            ))?;
            let rows = stmt.query_map(
                rusqlite::params![folder_id, since_seq, state_entry],
                sync_change_row_from_sql,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        }
    }

    /// The set's unsigned rows under `device_id` — the WebDAV pseudo-device
    /// rows the served-era adoption owes (`writer-signed-change-records.md`
    /// ruling (7)(b)), over the population `changes.list` serves (file rows,
    /// not terminally superseded) so the count the flip refuses with is over
    /// exactly the rows the owner's sweep can see. The adoptable test is the
    /// caller's (`sync_writer_sig::served_row_adoptable`, applied in Rust —
    /// never mirrored in SQL).
    pub async fn unsigned_sync_changes_for_device(
        &self,
        folder_id: i64,
        device_id: &[u8; 32],
    ) -> Result<Vec<SyncChangeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SYNC_CHANGE_COLUMNS}
             FROM sync_changes
             WHERE folder_id = ?1 AND device_id = ?2 AND signature IS NULL
               AND superseded_at IS NULL AND item_class IS NULL
             ORDER BY seq",
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![folder_id, device_id.as_slice()],
            sync_change_row_from_sql,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The rows of `folder_id` with these `seq`s (a seq naming another set's
    /// row, or none, is simply absent from the result).
    pub async fn sync_changes_by_seq(
        &self,
        folder_id: i64,
        seqs: &[i64],
    ) -> Result<Vec<SyncChangeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SYNC_CHANGE_COLUMNS} FROM sync_changes WHERE folder_id = ?1 AND seq = ?2",
        ))?;
        let mut out = Vec::with_capacity(seqs.len());
        for seq in seqs {
            if let Some(row) = stmt
                .query_row(rusqlite::params![folder_id, seq], sync_change_row_from_sql)
                .optional()?
            {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// Fill a page of rows' `signature` / `signer_key` in place — the
    /// served-era adoption's write (ruling (7)(b)): one transaction, no row
    /// minted, no seq moved, and only where the row is still unsigned, so a
    /// repeated page is a no-op. Returns how many rows it signed.
    pub async fn fill_sync_change_signatures(
        &self,
        folder_id: i64,
        page: &[(i64, RowSignature<'_>)],
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin fill_sync_change_signatures transaction")?;
        let mut filled = 0u64;
        for (seq, sig) in page {
            filled += tx.execute(
                "UPDATE sync_changes SET signature = ?1, signer_key = ?2
                 WHERE folder_id = ?3 AND seq = ?4 AND signature IS NULL",
                rusqlite::params![sig.signature, sig.signer_key, folder_id, seq],
            )? as u64;
        }
        tx.commit()?;
        Ok(filled)
    }

    /// The item-class-less folder branch excludes `state-entry` rows.
    /// `resolve_readable_folder` does **not** refuse reserved names — it
    /// resolves `(name, actor_id)` for any name the owner passes — so a client
    /// naming `__state` in the shipped request shape would otherwise reach this
    /// query and be handed class-2 rows it cannot parse. The reserved rails are
    /// unaffected by that today only because their rows are ordinary file rows;
    /// a new item class makes the difference load-bearing. So the rule is
    /// uniform: the generalized feed is reachable through `item_class` routing
    /// and through nothing else.
    ///
    /// Return the actor's **user** folders, each with the timestamp of the
    /// most recent sync_change (or `None` if no changes exist yet).
    ///
    /// This is the backups projection (sole consumer is `fauna.sync.backup_status` +
    /// its HTTP twin), so reserved internal `__<kind>` sets are excluded —
    /// they are never user backup targets and, having zero snapshots, would
    /// otherwise poison the backups dropdown. The
    /// `__` convention is owned by [`crate::db::snapshots::is_reserved_folder_name`].
    pub async fn get_folders_for_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<(SetLabel, Option<i64>)>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        // Ordered by row id, not name: a sealed set's plaintext is not the
        // nest's to sort by, and the reader orders what it renders.
        let mut stmt = conn.prepare(
            "SELECT fs.name, fs.name_hash, fs.name_sealed, MAX(sc.created_at)
             FROM folders fs
             LEFT JOIN sync_changes sc ON sc.folder_id = fs.id
             WHERE fs.actor_id = ?1
             GROUP BY fs.id
             ORDER BY fs.id",
        )?;
        let rows = stmt.query_map(rusqlite::params![actor_id.as_slice()], |row| {
            Ok((SetLabel::from_row(row, 0)?, row.get::<_, Option<i64>>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (label, last_change) = row?;
            if crate::db::snapshots::is_reserved_folder_name(&label.name) {
                continue;
            }
            out.push((label, last_change));
        }
        Ok(out)
    }

    /// Return all folders owned by the given actor with full row data.
    pub async fn get_folders_for_actor_full(&self, actor_id: &[u8; 32]) -> Result<Vec<FolderRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders WHERE actor_id = ?1 ORDER BY name"
        ))?;
        let rows = stmt.query_map(rusqlite::params![actor_id.as_slice()], |row| {
            folder_row_from(row)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// List all folders.
    pub async fn list_folders(&self) -> Result<Vec<FolderRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(concat!(
            "SELECT ",
            folder_columns!(),
            " FROM folders ORDER BY id"
        ))?;
        let rows = stmt
            .query_map([], folder_row_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Check if a folder has new recorded changes since its last snapshot
    /// and the latest change is older than `quiet_secs` ago.
    ///
    /// **One change source** since the phase 3 head unification (2026-08-17,
    /// `file-sync.md` § Membership → *Target state — head
    /// unification*): every snapshottable folder's changes live in
    /// `sync_changes`, checked against the single `max_change_seq` watermark.
    /// A custody copy is a cross-location backup
    /// **destination** (custodian-held latest-per-path custody) and is never
    /// snapshotted here — snapshot pins would defeat the custodian's
    /// reclamation contract (`message-segment-store.md` § GC-safety).
    pub async fn folder_needs_snapshot(&self, folder_id: i64, quiet_secs: i64) -> Result<bool> {
        let conn = self.conn.lock().await;

        let (custody_copy, name, keeps_snapshots, own_quiet): (
            bool,
            String,
            Option<i64>,
            Option<i64>,
        ) = match conn
            .query_row(
                "SELECT custody_copy, COALESCE(name, ''), nest_snapshots, nest_snapshot_quiet_secs \
                 FROM folders WHERE id = ?1",
                rusqlite::params![folder_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?
        {
            Some(m) => m,
            None => return Ok(false),
        };

        // The nest place's own policy, resolved before any per-mode work
        // (v37, folders re-model phase 2 § Places). Both columns are
        // three-state: NULL is "unset", which is every
        // folder whose owner never chose, and it resolves to the nest-wide
        // behavior this method had before the columns existed.
        //
        // The owner's `false` is checked first and unconditionally — it is the
        // one verdict no amount of change detection can overturn. The
        // structural refusals below are the opposite direction (they can only
        // ever say *no*), so the two never fight: a folder is snapshotted when
        // the owner has not said no AND nothing structural says no.
        if keeps_snapshots == Some(0) {
            return Ok(false);
        }
        let quiet_secs = own_quiet.unwrap_or(quiet_secs);

        if crate::db::snapshots::is_reserved_custody_copy(custody_copy, &name) {
            return Ok(false);
        }

        // IMPORTANT: sync_changes.created_at uses now_epoch_millis(), so we must
        // compare in milliseconds. Convert quiet_secs to millis for the comparison.
        let now_millis = now_epoch_millis();
        let quiet_millis = quiet_secs * 1000;

        // Get max seq and latest change time from sync_changes
        let change_stats: Option<(i64, i64)> = conn.query_row(
            "SELECT MAX(seq), MAX(created_at) FROM sync_changes WHERE folder_id = ?1 AND (path IS NOT NULL OR path_sealed IS NOT NULL)",
            rusqlite::params![folder_id],
            |row| {
                let seq: Option<i64> = row.get(0)?;
                let time: Option<i64> = row.get(1)?;
                Ok(seq.zip(time))
            },
        )?;

        let (latest_seq, latest_change_time) = match change_stats {
            Some(s) => s,
            None => return Ok(false), // No changes at all
        };

        // Check quiet period (both values in milliseconds)
        if now_millis - latest_change_time < quiet_millis {
            return Ok(false);
        }

        // Get max_change_seq from the most recent snapshot
        let snapshot_seq: Option<i64> = conn.query_row(
            "SELECT max_change_seq FROM snapshots WHERE folder_id = ?1 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![folder_id],
            |row| row.get(0),
        ).unwrap_or(None);

        // NULL or missing snapshot_seq treated as 0
        let snapshot_seq = snapshot_seq.unwrap_or(0);

        Ok(latest_seq > snapshot_seq)
    }

    /// List the latest state of each file in a folder (excluding deleted files).
    ///
    /// **The outer `sync_changes sc` scan carries no `folder_id` predicate of
    /// its own, and does not need one** (asked and answered 2026-07-23 — a
    /// standing residual claimed a path+seq collision could surface another
    /// set's file here). `seq` is `INTEGER PRIMARY KEY AUTOINCREMENT`
    /// (`migrations.rs`), i.e. globally unique across every set, so
    /// `sc.seq = latest.max_seq` pins *exactly* the row the inner subquery
    /// selected — and that subquery is `folder_id`-scoped. The
    /// `sc.path_hash = latest.path_hash` conjunct is therefore
    /// redundant-but-consistent, not the thing doing the scoping. A future
    /// change that makes `seq` per-set (or joins on anything weaker than the
    /// PK) MUST add the predicate.
    ///
    /// **The fold keys on `path_hash`, not the plaintext `path`** (path-sealing
    /// S1, `file-sync.md` § Sealed names & paths): the two are 1:1, and
    /// `path_hash` is `NOT NULL` on every row while `path` is the additive
    /// column that stops being written at the major-gated flip. Only the
    /// `ORDER BY sc.path` remains plaintext — it is the `fauna.media.list` v1
    /// keyset cursor contract, retired together with the plaintext column by
    /// the hash-ordered v2 cursor. The `path IS NOT NULL` filter stays inside
    /// the subquery, so the fold still sees exactly the listable rows.
    pub async fn get_files_for_folder(&self, folder_id: i64) -> Result<Vec<SyncFileInfo>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT sc.path, sc.manifest_hash, sc.size_bytes, sc.created_at, sc.thumbnail_hash, sc.content_key_version, sc.path_sealed, sc.path_hash,
                    sc.device_id, sc.actor_id, sc.change_type, sc.derived_through, sc.is_resolution, sc.signature, sc.signer_key
             FROM sync_changes sc
             INNER JOIN (
                 SELECT path_hash, MAX(seq) as max_seq
                 FROM sync_changes
                 WHERE folder_id = ?1 AND (path IS NOT NULL OR path_sealed IS NOT NULL)
                 GROUP BY path_hash
             ) latest ON sc.path_hash = latest.path_hash AND sc.seq = latest.max_seq
             WHERE sc.change_type != 'delete' AND sc.manifest_hash IS NOT NULL
             ORDER BY sc.path_hash",
        )?;
        let rows = stmt.query_map(rusqlite::params![folder_id], |row| {
            Ok(SyncFileInfo {
                path: row.get(0)?,
                manifest_hash: row.get(1)?,
                size_bytes: row.get(2)?,
                updated_at: row.get(3)?,
                thumbnail_hash: row.get(4)?,
                content_key_version: row.get(5)?,
                path_sealed: row.get(6)?,
                path_hash: row.get(7)?,
                device_id: row.get(8)?,
                actor_id: row.get(9)?,
                change_type: row.get(10)?,
                derived_through: row.get(11)?,
                is_resolution: row.get::<_, Option<i64>>(12)?.map(|v| v != 0),
                signature: row.get(13)?,
                signer_key: row.get(14)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The current (latest, live) manifest hash for one path in a folder, or
    /// `None` if the path never existed or its latest change is a delete
    /// (tombstone). Used by `webdav_record_change` to evaluate a WebDAV
    /// `If-Match` / `If-None-Match` conditional against the path's ETag (the
    /// manifest hash) at the nest, the write serialization point
    /// (`webdav-server.md` § Protocol surface — a lost race is `412`).
    ///
    /// **Addressed by `path_hash`, never the plaintext** (path-sealing S1): the
    /// column is `NOT NULL` on every row, and the one caller already derives the
    /// hash for the change it is about to record — so the ETag head and the
    /// record it guards address the row identically, with nothing left to
    /// re-key at the flip.
    pub async fn get_file_head_manifest(
        &self,
        folder_id: i64,
        path_hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let path_hash = path_hash.to_vec();
        let conn = self.conn.lock().await;
        let latest: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT manifest_hash FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![folder_id, path_hash],
                |row| row.get(0),
            )
            .optional()
            .context("query file head manifest")?;
        // No row → never existed; latest row's manifest NULL → deleted. Both are
        // "no live version".
        Ok(latest.flatten())
    }

    /// Create a folder with the optional per-set settings in [`FolderOptions`].
    ///
    /// The two positional args are the set's identity; everything optional
    /// rides in `opts` so a new option is an additive field, never a caller
    /// break.
    pub async fn create_folder_with_options(
        &self,
        name: &str,
        actor_id: &[u8; 32],
        opts: FolderOptions,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        Self::create_folder_with_options_in_conn(&conn, name, actor_id, opts)
    }

    /// [`Self::create_folder_with_options`] over a **caller-held** connection.
    ///
    /// Extracted for the same reason as
    /// [`Self::record_sync_change_in_conn`]: the covered-folder materialize arm
    /// mints its target set and re-homes its rows in ONE transaction, so both
    /// writes have to compose into a transaction this type does not own — and a
    /// second `INSERT INTO folders` would be a second answer to "what does a
    /// freshly created set look like" (`name_hash`, the reserved-name seal drop,
    /// the `conflict_policy` COALESCE).
    pub(crate) fn create_folder_with_options_in_conn(
        conn: &rusqlite::Connection,
        name: &str,
        actor_id: &[u8; 32],
        opts: FolderOptions,
    ) -> Result<i64> {
        let name_hash = fauna_core::path_crypto::set_name_hash(name);
        Self::insert_folder_in_conn(conn, Some(name), name_hash, actor_id, opts)
    }

    /// Mint a **sealed** set the request named by its hash alone
    /// (`fauna.folders.create` with no plaintext name — `path-sealing.md`
    /// § the set-name plane): the row rests `name_hash` + `name_sealed` and a
    /// NULL name from its first write, so the nest never learns the name at
    /// all. Refused without a seal (nothing would ever name the row) and for a
    /// `public` set (its name is the URL segment, so it must ride plaintext).
    /// A duplicate is the same `UNIQUE(name_hash, actor_id)` conflict a by-name
    /// create meets.
    pub async fn create_sealed_folder_by_hash(
        &self,
        name_hash: [u8; 32],
        actor_id: &[u8; 32],
        opts: FolderOptions,
    ) -> Result<i64> {
        anyhow::ensure!(
            opts.name_sealed.is_some(),
            "a create by hash alone must carry the sealed name"
        );
        anyhow::ensure!(
            opts.audience.as_deref() != Some("public"),
            "a public folder's name is its URL segment and is created by name"
        );
        let conn = self.conn.lock().await;
        Self::insert_folder_in_conn(&conn, None, name_hash, actor_id, opts)
    }

    /// The one `INSERT INTO folders` — `name` is `None` only for a sealed
    /// create by hash ([`Self::create_sealed_folder_by_hash`]); otherwise
    /// `name_hash` is its hash.
    fn insert_folder_in_conn(
        conn: &rusqlite::Connection,
        name: Option<&str>,
        name_hash: [u8; 32],
        actor_id: &[u8; 32],
        opts: FolderOptions,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let name = name.unwrap_or_default().to_string();
        let FolderOptions {
            retention_policy,
            include_paths,
            exclude_paths,
            conflict_policy,
            name_sealed,
            retention_policy_sealed,
            audience,
            set_nonce,
            custody_copy,
        } = opts;
        // A reserved rail's name is a routing constant and never seals — drop a
        // seal for one rather than store it, mirroring the `update` handler's
        // explicit refusal. Reserved sets are minted by the nest's own internal
        // `get_or_create_*` paths, which never pass one, so this is a backstop.
        let name_sealed = name_sealed.filter(|_| !fauna_core::sync::is_reserved_folder_name(&name));
        // A sealed set's name rests only sealed — but a public folder's, whose
        // name is its URL segment (`path-sealing.md` § the set-name plane). The
        // row rests its hash either way, so it stays addressable; a create by
        // hash alone carries no name to rest. Mirrors `migrations::BLANK_SEALED_FOLDER_NAME`, which
        // every later write runs.
        let resting_name = (!name.is_empty()
            && (name_sealed.is_none() || audience.as_deref() == Some("public")))
        .then_some(name.as_str());
        let now = now_epoch_millis();
        conn.execute(
            // COALESCE keeps the column's `'auto'` default authoritative when the
            // (wire-additive) create-time policy is absent.
            "INSERT INTO folders (name, actor_id, created_at, custody_copy, retention_policy, include_paths, exclude_paths, conflict_policy, name_hash, name_sealed, retention_policy_sealed, audience, set_nonce) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, COALESCE(?8, 'auto'), ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![resting_name, actor_id.as_slice(), now, custody_copy as i64, retention_policy, include_paths, exclude_paths, conflict_policy, name_hash.as_slice(), name_sealed, retention_policy_sealed, audience, set_nonce],
        ).context("create folder")?;
        Ok(conn.last_insert_rowid())
    }

    /// [`Self::update_folder_by_id`] on the set the given actor owns under
    /// `name` — a by-name convenience for a caller that holds a literal
    /// (reserved) name, never for a handler that has already resolved its row.
    /// Returns false when no such set exists; an empty name names none
    /// ([`Self::get_folder_for_actor`]).
    pub async fn update_folder_for_user(
        &self,
        name: &str,
        actor_id: &[u8; 32],
        update: FolderUpdate<'_>,
    ) -> Result<bool> {
        match self.get_folder_for_actor(name, actor_id).await? {
            Some(fs) => self.update_folder_by_id(fs.id, update).await,
            None => Ok(false),
        }
    }

    /// Apply a partial [`FolderUpdate`] to the folder row `folder_id`.
    /// Returns true if a row was updated, false if no such row exists.
    ///
    /// Keyed by the row id, never `(name, actor_id)`: the handler resolves the
    /// row first (hash-first, owner-scoped), and the plaintext name is not an
    /// address a sealed set keeps (`path-sealing.md` § the set-name plane).
    pub async fn update_folder_by_id(
        &self,
        folder_id: i64,
        update: FolderUpdate<'_>,
    ) -> Result<bool> {
        let FolderUpdate {
            retention_policy,
            include_paths,
            exclude_paths,
            webdav_enabled,
            conflict_policy,
            name_sealed,
            name,
            include_paths_sealed,
            exclude_paths_sealed,
            retention_policy_sealed,
            nest_snapshots,
            nest_snapshot_quiet_secs,
            version_retention,
            audience,
            website_enabled,
            residency,
            exclusive_editing,
            audience_attestation,
            set_nonce,
        } = update;
        let name = name.map(|s| s.to_string());
        let retention_policy = retention_policy.map(|opt| opt.map(|s| s.to_string()));
        let version_retention = version_retention.map(|opt| opt.map(|s| s.to_string()));
        let include_paths = include_paths.map(|opt| opt.map(|s| s.to_string()));
        let exclude_paths = exclude_paths.map(|opt| opt.map(|s| s.to_string()));
        let conflict_policy = conflict_policy.map(|s| s.to_string());
        let name_sealed = name_sealed.map(|b| b.to_vec());
        let include_paths_sealed = include_paths_sealed.map(|opt| opt.map(|b| b.to_vec()));
        let exclude_paths_sealed = exclude_paths_sealed.map(|opt| opt.map(|b| b.to_vec()));
        let retention_policy_sealed = retention_policy_sealed.map(|opt| opt.map(|b| b.to_vec()));
        let conn = self.conn.lock().await;

        // Build dynamic SET clause
        let mut sets = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(ref rp) = retention_policy {
            sets.push(format!("retention_policy = ?{idx}"));
            params.push(Box::new(rp.clone()));
            idx += 1;
        }
        if let Some(ref ip) = include_paths {
            sets.push(format!("include_paths = ?{idx}"));
            params.push(Box::new(ip.clone()));
            idx += 1;
        }
        if let Some(ref ep) = exclude_paths {
            sets.push(format!("exclude_paths = ?{idx}"));
            params.push(Box::new(ep.clone()));
            idx += 1;
        }
        if let Some(we) = webdav_enabled {
            sets.push(format!("webdav_enabled = ?{idx}"));
            params.push(Box::new(we as i64));
            idx += 1;
        }
        if let Some(ref cp) = conflict_policy {
            sets.push(format!("conflict_policy = ?{idx}"));
            params.push(Box::new(cp.clone()));
            idx += 1;
        }
        if let Some(ref ns) = name_sealed {
            sets.push(format!("name_sealed = ?{idx}"));
            params.push(Box::new(ns.clone()));
            idx += 1;
        }
        if let Some(ref n) = name {
            sets.push(format!("name = ?{idx}"));
            params.push(Box::new(n.clone()));
            idx += 1;
        }
        // The sealed path pair binds its *inner* option, so `Some(None)` writes
        // SQL NULL — the deliberate clear a keyless writer performs alongside a
        // plaintext save (see `FolderUpdate::include_paths_sealed`). This is why
        // these two arms differ in shape from `name_sealed` above, whose stamp
        // has no clearing writer.
        if let Some(ref ips) = include_paths_sealed {
            sets.push(format!("include_paths_sealed = ?{idx}"));
            params.push(Box::new(ips.clone()));
            idx += 1;
        }
        if let Some(ref eps) = exclude_paths_sealed {
            sets.push(format!("exclude_paths_sealed = ?{idx}"));
            params.push(Box::new(eps.clone()));
            idx += 1;
        }
        // Same inner-option binding, same reason: `Some(None)` clears, which is
        // what a keyless retention save must do rather than leave a seal that
        // opens to the policy it replaced (`FolderUpdate::retention_policy_sealed`).
        if let Some(ref rps) = retention_policy_sealed {
            sets.push(format!("retention_policy_sealed = ?{idx}"));
            params.push(Box::new(rps.clone()));
            idx += 1;
        }
        // The nest place's two knobs bind their *inner* option so `Some(None)`
        // writes SQL NULL — here the clear is the user asking for the nest-wide
        // default back, not a seal-pair chore (`FolderUpdate::nest_snapshots`).
        if let Some(ns) = nest_snapshots {
            sets.push(format!("nest_snapshots = ?{idx}"));
            params.push(Box::new(ns.map(|b| b as i64)));
            idx += 1;
        }
        if let Some(nq) = nest_snapshot_quiet_secs {
            sets.push(format!("nest_snapshot_quiet_secs = ?{idx}"));
            params.push(Box::new(nq));
            idx += 1;
        }
        // Version retention binds its inner option like the nest-place knobs:
        // `Some(None)` writes SQL NULL — the handler's mapping of a
        // binds-nothing policy back to the honest keep-everything resting value
        // (`FolderUpdate::version_retention`).
        if let Some(ref vr) = version_retention {
            sets.push(format!("version_retention = ?{idx}"));
            params.push(Box::new(vr.clone()));
            idx += 1;
        }
        // Audience binds its inner option: `Some(None)` writes SQL NULL — the
        // flip-back that returns the folder to its derived private/shared
        // state (`FolderUpdate::audience`).
        if let Some(aud) = audience {
            sets.push(format!("audience = ?{idx}"));
            params.push(Box::new(aud.map(|s| s.to_string())));
            idx += 1;
            // THE PUBLIC FLOOR (v43, phase 4 slice 4f-i — `folders.md`
            // § Publicly-synced follow). Every →`public` transition stamps the
            // folder's current head seq, in THIS statement rather than a
            // read-then-write pair: a change recorded between a separate read
            // and this update would land *below* the floor and be served on the
            // public plane — a leak of a privately-recorded row. Under the one
            // connection lock the sub-select and the flip are atomic.
            //
            // Guarded on `audience IS NOT 'public'`, which SQLite evaluates
            // against the **pre-update** row: only an actual transition stamps.
            // A re-assert of `public` on an already-public folder must NOT
            // re-stamp — that would raise the floor over content recorded
            // during the public window and silently un-serve it. `IS NOT`
            // (never `!=`) so the NULL resting value compares true.
            if aud == Some("public") {
                sets.push(
                    "public_floor_seq = CASE WHEN audience IS NOT 'public' \
                     THEN (SELECT COALESCE(MAX(seq), 0) FROM sync_changes \
                           WHERE folder_id = folders.id) \
                     ELSE public_floor_seq END"
                        .to_string(),
                );
            }
        }
        if let Some(we) = website_enabled {
            sets.push(format!("website_enabled = ?{idx}"));
            params.push(Box::new(we as i64));
            idx += 1;
        }
        // Residency binds its inner option: `Some(None)` writes SQL NULL — the
        // flip back to full (`FolderUpdate::residency`). The consent gate, the
        // value set, and the serving-toggle refusals are all the handler's;
        // the chunk-byte drop a →metadata_only flip owes is the handler's
        // spawned pass, keyed off this committed column (single decision
        // point + GC reconcile — never a second stored signal).
        if let Some(res) = residency {
            sets.push(format!("nest_content_residency = ?{idx}"));
            params.push(Box::new(res.map(|s| s.to_string())));
            idx += 1;
        }
        // Exclusive editing — the owner's standing choice only. Turning it
        // OFF deliberately does NOT release a lease some device is holding:
        // the holder's own release (or the 300 s TTL) ends a lease, and
        // clearing one from under a device mid-upload is exactly the
        // grief-grab `release_upload_lease`'s holder scoping exists to stop.
        // The next pass simply does not take a new one.
        if let Some(ee) = exclusive_editing {
            sets.push(format!("exclusive_editing = ?{idx}"));
            params.push(Box::new(ee as i64));
            idx += 1;
        }
        // Opaque store — see `FolderRow::audience_attestation`.
        if let Some(att) = audience_attestation {
            sets.push(format!("audience_attestation = ?{idx}"));
            params.push(Box::new(att.to_vec()));
            idx += 1;
        }
        // The owner's overwrite of the stored set nonce — the nest's copy
        // trails the client's custody (`FolderRow::set_nonce`).
        if let Some(nonce) = set_nonce {
            sets.push(format!("set_nonce = ?{idx}"));
            params.push(Box::new(nonce.to_vec()));
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(false);
        }

        let sql = format!("UPDATE folders SET {} WHERE id = ?{}", sets.join(", "), idx,);
        params.push(Box::new(folder_id));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        // A SERVING transition — the website toggle changing, or the audience
        // moving — can take this folder's templates out of the owner's
        // rendered site, and the render that drops their pages runs after this
        // commit. So the update and the owed-render marker are one
        // transaction, decided here off the pre-update row
        // (`web-content-hosting.md` § Routing, render, serving → *A revoke is
        // durable*). Both directions mark: the handler fires the same render
        // for either, and the marker is what lets its retry still render.
        let tx = conn
            .unchecked_transaction()
            .context("begin update_folder_by_id transaction")?;
        // Read the gate's OWN answer either side of the UPDATE rather than
        // watching two named columns: any column `rests_plaintext_paths`
        // grows moves it too (the retired `mode = "web"` leg was the lesson).
        // The owner rides along: the owed render is the owner's site.
        let gate = |tx: &rusqlite::Transaction<'_>| -> Result<Option<(bool, Vec<u8>)>> {
            tx.query_row(
                "SELECT website_enabled, audience, actor_id FROM folders WHERE id = ?1",
                rusqlite::params![folder_id],
                |r| {
                    let website_enabled: bool = r.get(0)?;
                    let audience: Option<String> = r.get(1)?;
                    Ok((
                        super::serves_plaintext_web_files(website_enabled, audience.as_deref()),
                        r.get(2)?,
                    ))
                },
            )
            .optional()
            .context("read folder serving gate around update")
        };
        let before = gate(&tx)?;
        let changed = tx
            .execute(&sql, param_refs.as_slice())
            .context("update folder by id")?;
        // Whatever the write left — a seal stamped, a flip back from public — a
        // sealed, non-public set's name rests only sealed, in this transaction.
        tx.execute(
            super::migrations::BLANK_SEALED_FOLDER_NAME,
            rusqlite::params![folder_id],
        )
        .context("rest a sealed folder's name sealed only")?;
        let after = gate(&tx)?;
        if let (Some((served_before, owner)), Some((served_after, _))) = (&before, &after)
            && changed > 0
            && served_before != served_after
        {
            let owner: [u8; 32] = owner
                .as_slice()
                .try_into()
                .context("folder owner is not a 32-byte actor id")?;
            super::web::mark_web_render_owed(&tx, &owner)?;
        }
        tx.commit().context("commit update_folder_by_id")?;
        Ok(changed > 0)
    }

    /// Set or clear a website-enabled folder's paywall tier (`web_paywall_tier` —
    /// monetization.md § Pillar 2, the folder half). `Some(tier)` paywalls,
    /// `None` clears. Mode/tier-existence validation is the handler's job
    /// (`fauna.folders.set_web_paywall`); this is the bare column write.
    /// Keyed by the row id the handler resolved ([`Self::update_folder_by_id`]).
    /// Returns false when no such row exists.
    pub async fn set_folder_web_paywall_tier_by_id(
        &self,
        folder_id: i64,
        tier: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE folders SET web_paywall_tier = ?1 WHERE id = ?2",
                rusqlite::params![tier, folder_id],
            )
            .context("set folder web paywall tier")?;
        Ok(changed > 0)
    }

    /// Delete a folder owned by the given actor, cascading to all of its
    /// children. Returns true if the set existed and was deleted, false if no
    /// matching row was found.
    ///
    /// `foreign_keys` is ON (`db/mod.rs`), and several children reference
    /// `folders(id)` / `snapshots(id)` **without** `ON DELETE CASCADE`, so a
    /// bare `DELETE FROM folders` violates a FOREIGN KEY constraint once the
    /// set has any snapshot, member, or destination (it surfaced to the client
    /// as `fauna.folders.internal`). We delete the
    /// children explicitly, in dependency order, inside one transaction:
    /// - `snapshots` children that don't cascade: `bridge_restore_divergence`,
    ///   `restore_history` (`snapshot_files` cascades from `snapshots`);
    /// - the `snapshots.parent_id` self-FK (NO ACTION) — null it first so the
    ///   bulk snapshot delete can't reference a just-deleted parent;
    /// - `snapshots`, then the direct non-cascading `folders` children
    ///   `folder_members`;
    /// - the non-FK `folder_id` rows (`sync_conflicts` cascades, but the rest
    ///   — `operation_locks`, `upload_leases`, `sync_changes` — carry the column
    ///   with no FK, so we clear them to avoid orphans).
    ///
    /// Keyed by the row id the handler resolved ([`Self::update_folder_by_id`]).
    pub async fn delete_folder_by_id(&self, fs_id: i64) -> Result<bool> {
        let conn = self.conn.lock().await;

        let row: Option<(String, Vec<u8>, bool, bool)> = conn
            .query_row(
                "SELECT COALESCE(name, ''), actor_id, custody_copy, website_enabled FROM folders WHERE id = ?1",
                rusqlite::params![fs_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?)),
            )
            .optional()
            .context("look up folder for delete")?;
        let Some((name, actor_id, custody_copy, website_enabled)) = row else {
            return Ok(false);
        };
        let actor_id: [u8; 32] = actor_id
            .as_slice()
            .try_into()
            .context("folder owner is not a 32-byte actor id")?;

        // Backstop of the delete handler's reserved-rail guard (defence in
        // depth for future callers): a reserved set that is NOT a custody copy
        // is a live rail whose `sync_changes` rows are the only reach to its
        // sealed irrecoverable material — refuse loudly, never delete or
        // `Ok(false)`. A custody copy stays deletable (the destination-removal
        // handshake). Bulk user deletion goes through
        // `delete_all_folders_for_actor`, which reclaims rails on purpose.
        if crate::db::snapshots::is_reserved_folder_name(&name)
            && !crate::db::snapshots::is_reserved_custody_copy(custody_copy, &name)
        {
            anyhow::bail!(
                "refusing to delete reserved folder {name:?}: it is a live internal \
                 rail, not a backup custody copy"
            );
        }

        let tx = conn
            .unchecked_transaction()
            .context("begin delete_folder_by_id transaction")?;
        let changed = Self::delete_folder_rows_in_tx(&tx, fs_id)?;
        // Deleting a website-capable folder takes its templates out of the
        // owner's rendered site; the render that drops their pages runs after
        // this commit, so the owed-render marker rides it
        // (`web-content-hosting.md` § Routing, render, serving → *A revoke is
        // durable*). The same website-capable test the delete handler's render
        // used to gate on.
        if changed > 0 && website_enabled {
            super::web::mark_web_render_owed(&tx, &actor_id)?;
        }
        tx.commit().context("commit delete_folder_by_id")?;
        Ok(changed > 0)
    }

    /// [`Self::delete_folder_by_id`] on the set the given actor owns under
    /// `name` — the by-name convenience [`Self::update_folder_for_user`]
    /// describes. Returns false when no such set exists.
    pub async fn delete_folder_for_user(&self, name: &str, actor_id: &[u8; 32]) -> Result<bool> {
        match self.get_folder_for_actor(name, actor_id).await? {
            Some(fs) => self.delete_folder_by_id(fs.id).await,
            None => Ok(false),
        }
    }

    /// Delete EVERY folder owned by `actor_id` (each set's non-cascading
    /// children + `backup_custody` included), in one transaction. Returns the
    /// number of folders removed.
    ///
    /// This is the reclaim half of holder-side "stop hosting" for a
    /// held-for-friends backup guest (`docs/goal/behavior/backup-destinations.md` § Held-for-friends
    /// enrollment → "the guest's reserved sets + chunks are **reclaimed** by the
    /// holding nest's GC/retention"): dropping the guest's `backup_custody` rows
    /// lets the next GC reclaim their chunks. Called from every user-deletion
    /// path (`pending_actions::finalize_user_deletion` — eviction finalize +
    /// self-delete), which previously cleaned inbox / devices / groups / tokens
    /// but left a deleted user's folders + custody orphaned, so the GC
    /// reference walk protected their chunks forever (no space ever reclaimed).
    pub async fn delete_all_folders_for_actor(&self, actor_id: &[u8; 32]) -> Result<u64> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;

        let ids: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM folders WHERE actor_id = ?1")
                .context("prepare list folders for actor")?;
            let rows = stmt
                .query_map(rusqlite::params![actor_id.as_slice()], |r| {
                    r.get::<_, i64>(0)
                })
                .context("query folders for actor")?;
            rows.collect::<rusqlite::Result<Vec<i64>>>()
                .context("collect folder ids for actor")?
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let tx = conn
            .unchecked_transaction()
            .context("begin delete_all_folders_for_actor transaction")?;
        let mut removed = 0u64;
        for fs_id in ids {
            removed += Self::delete_folder_rows_in_tx(&tx, fs_id)?;
        }
        tx.commit().context("commit delete_all_folders_for_actor")?;
        Ok(removed)
    }

    /// Delete one folder (resolved `fs_id`) and all its non-cascading children
    /// within `tx`. Shared by [`Self::delete_folder_for_user`] (one named set)
    /// and [`Self::delete_all_folders_for_actor`] (every set an actor owns).
    /// Returns 1 if the `folders` row was removed, else 0.
    ///
    /// `backup_custody` is the cross-location backup liveness projection the GC
    /// reference walk reads (`backup/gc.rs`); dropping a custody-copy set's
    /// custody IS the destination-removal handshake (`docs/goal/behavior/backup-destinations.md`
    /// § Destination-removal / supersede handshake → "drops the reserved backup
    /// folder → all its custody entries drop → all chunks reclaim"), so the
    /// next GC reclaims the set's chunks. A no-op for non-backup sets (no custody
    /// rows). (`sync_conflicts` has ON DELETE CASCADE but is cheap to clear
    /// explicitly.)
    fn delete_folder_rows_in_tx(tx: &rusqlite::Transaction<'_>, fs_id: i64) -> Result<u64> {
        // Per-actor storage-quota reclaim: before dropping the set's rows, credit
        // the owner back the logical bytes the set currently holds (latest live
        // `sync_changes` per path + live `backup_custody`). Mirrors the
        // supersede/tombstone decrement so `users.storage_bytes_used` stays a
        // true running sum across destination-removal / "stop hosting" eviction
        // (`docs/goal/behavior/backup-destinations.md` § Held-for-friends enrollment). Harmless on
        // the full-user-deletion path (the owner row is dropped right after).
        let owner_and_group: Option<(Vec<u8>, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT actor_id, mls_group_id FROM folders WHERE id = ?1",
                rusqlite::params![fs_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read folder owner + bound group for quota reclaim / channel cascade")?;
        let (owner, mls_group_id) = match owner_and_group {
            Some((owner, group)) => (Some(owner), group),
            None => (None, None),
        };
        if let Some(owner) = owner {
            // Retained accounting (§ Retention (4)): every manifest-bearing
            // row not yet superseded or soft-pruned is charged — heads AND
            // retained versions — so the reclaim sums exactly that population
            // (the head-only sum would leak the retained charges). An
            // uncharged row of a same-manifest pair (`charged = 0`, ruling
            // (11)(g)) was never charged, so it is not credited either.
            let sync_live: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(size_bytes), 0) FROM sync_changes
                     WHERE folder_id = ?1 AND manifest_hash IS NOT NULL
                       AND superseded_at IS NULL AND pruned_at IS NULL
                       AND charged = 1",
                    rusqlite::params![fs_id],
                    |r| r.get(0),
                )
                .context("sum charged sync_changes bytes for quota reclaim")?;
            let custody_live: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(size_bytes), 0) FROM backup_custody
                     WHERE folder_id = ?1 AND manifest_hash IS NOT NULL",
                    rusqlite::params![fs_id],
                    |r| r.get(0),
                )
                .context("sum live backup_custody bytes for quota reclaim")?;
            // Retained superseded generations are charged for the whole grace
            // window, so deleting the set must credit them back too — otherwise
            // removing a destination leaks quota that no later sweep can free
            // (the rows are gone with the set).
            let custody_retained: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(size_bytes), 0) FROM backup_custody_generations
                     WHERE folder_id = ?1",
                    rusqlite::params![fs_id],
                    |r| r.get(0),
                )
                .context("sum retained custody generation bytes for quota reclaim")?;
            let total = sync_live + custody_live + custody_retained;
            if total != 0 {
                tx.execute(
                    "UPDATE users SET storage_bytes_used = MAX(0, storage_bytes_used - ?1)
                     WHERE actor_id = ?2",
                    rusqlite::params![total, owner.as_slice()],
                )
                .context("reclaim storage_bytes_used on folder delete")?;
            }
        }

        // A bound (`mls_group_id` set) folder is the ONLY thing a claimed
        // channel ever serves (a set never re-binds to a
        // different group, so this row's owner is always the channel's
        // first-binder claimant — `db::channels::claim_folder_channel`).
        // `content_key.put` / `members.evict` / posting an MLS Commit all
        // require caller == claimant, so once this owner's folders row is
        // gone — whether via one `fauna.folders.delete` call or the whole
        // account being deleted — NOBODY can ever manage this channel again.
        // Leaving the claim, roster and satellite rows behind doesn't just
        // orphan dead residue: every OTHER member's own `actor_channels` row
        // (added by the owner's `welcome.deliver` share) survives untouched,
        // permanently stranding them in a contentless, un-rotatable,
        // un-leavable channel with no owner left to evict
        // them. So the channel itself — not just this set —
        // is torn down for every actor on it, not only this owner.
        if let Some(group_id) = mls_group_id {
            let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
            for table in [
                "folder_channel_claims",
                "actor_channels",
                "folder_member_access",
                "folder_content_keys",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE channel_id = ?1"),
                    rusqlite::params![channel_id.as_slice()],
                )
                .with_context(|| format!("delete {table} for the set's bound channel"))?;
            }
        }

        // Non-cascading children of this set's snapshots.
        tx.execute(
            "DELETE FROM bridge_restore_divergence
             WHERE snapshot_id IN (SELECT id FROM snapshots WHERE folder_id = ?1)",
            rusqlite::params![fs_id],
        )
        .context("delete bridge_restore_divergence for folder")?;
        tx.execute(
            "DELETE FROM restore_history
             WHERE snapshot_id IN (SELECT id FROM snapshots WHERE folder_id = ?1)",
            rusqlite::params![fs_id],
        )
        .context("delete restore_history for folder")?;

        // Break the snapshots.parent_id self-FK before the bulk snapshot delete.
        tx.execute(
            "UPDATE snapshots SET parent_id = NULL WHERE folder_id = ?1",
            rusqlite::params![fs_id],
        )
        .context("null snapshot parent_id for folder")?;

        // Snapshots (snapshot_files cascades from here).
        tx.execute(
            "DELETE FROM snapshots WHERE folder_id = ?1",
            rusqlite::params![fs_id],
        )
        .context("delete snapshots for folder")?;

        // Direct non-cascading folders children.
        tx.execute(
            "DELETE FROM folder_members WHERE folder_id = ?1",
            rusqlite::params![fs_id],
        )
        .context("delete folder_members for folder")?;

        // Rows carrying folder_id without an FK — clear to avoid orphans.
        // `web_files`: a derived projection of the `sync_changes`
        // rows dying in this loop (re-created by `route_web_file_change` on
        // any future sync); once the folder row is gone the serve/render gate
        // refuses each row forever, so keeping them would only orphan dead
        // rows behind the gate.
        for table in [
            "sync_conflicts",
            "operation_locks",
            "upload_leases",
            "sync_changes",
            "backup_custody",
            "backup_custody_generations",
            "web_files",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE folder_id = ?1"),
                rusqlite::params![fs_id],
            )
            .with_context(|| format!("delete {table} for folder"))?;
        }

        let changed = tx
            .execute(
                "DELETE FROM folders WHERE id = ?1",
                rusqlite::params![fs_id],
            )
            .context("delete folder rows")?;
        Ok(changed as u64)
    }

    /// Get device activity summary for a folder.
    ///
    /// The label joins the READER's own registration of each device only
    /// (`sync_devices` is keyed `(actor_id, device_id)`): another account's
    /// device on a shared set's rows reads nameless, never with that account's
    /// label (`path-sealing.md` § device label, gap (a)).
    pub async fn get_folder_devices(
        &self,
        folder_id: i64,
        reader_actor_id: &[u8],
    ) -> Result<Vec<DeviceSummary>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT sc.device_id, COALESCE(sd.label, '') as label, MAX(sc.created_at) as last_change_at, COUNT(*) as change_count
             FROM sync_changes sc
             LEFT JOIN sync_devices sd ON sc.device_id = sd.device_id AND sd.actor_id = ?2
             WHERE sc.folder_id = ?1 AND sc.device_id IS NOT NULL
             GROUP BY sc.device_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![folder_id, reader_actor_id], |row| {
            Ok(DeviceSummary {
                device_id: row.get(0)?,
                label: row.get(1)?,
                last_change_at: row.get(2)?,
                change_count: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Recompute and update the cached snapshot stats for a folder.
    pub async fn update_folder_cache(&self, folder_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE folders SET
                cached_snapshot_count = (SELECT COUNT(*) FROM snapshots WHERE folder_id = ?1),
                cached_total_bytes = COALESCE((SELECT SUM(total_bytes) FROM snapshots WHERE folder_id = ?1), 0),
                cached_last_snapshot_at = (SELECT MAX(created_at) FROM snapshots WHERE folder_id = ?1)
             WHERE id = ?1",
            rusqlite::params![folder_id],
        ).context("update folder cache")?;
        Ok(())
    }

    // ==================== Snapshots ====================

    /// The folder's live **plaintext-resting** heads — the latest-per-path fold
    /// snapshot-create uses, restricted to rows whose plaintext `path` rests:
    /// the enable-time `web_files` backfill's input (`web-content-hosting.md`
    /// § Content model — `web_files` is a projection of these heads). A sealed
    /// head rests no name this nest could serve as a URL, so it is structurally
    /// outside — sealed back-catalogues reach `web_files` only via client-driven
    /// re-records. The inner fold takes every row per
    /// `path_hash` so a path whose LATEST head is a delete (or rests sealed) is
    /// correctly absent rather than represented by a stale earlier head.
    /// Returns `(path, manifest_hash, content_key_version)` per live head.
    pub async fn live_plaintext_heads_for_folder(
        &self,
        folder_id: i64,
    ) -> Result<Vec<(String, [u8; 32], Option<i64>)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT sc.path, sc.manifest_hash, sc.content_key_version
             FROM sync_changes sc
             INNER JOIN (
                 SELECT path_hash, MAX(seq) as max_seq
                 FROM sync_changes
                 WHERE folder_id = ?1
                 GROUP BY path_hash
             ) latest ON sc.path_hash = latest.path_hash AND sc.seq = latest.max_seq
             WHERE sc.change_type != 'delete' AND sc.manifest_hash IS NOT NULL
               AND sc.path IS NOT NULL AND sc.path != ''
             ORDER BY sc.path",
        )?;
        let heads = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                let path: String = row.get(0)?;
                let manifest: Vec<u8> = row.get(1)?;
                let ckv: Option<i64> = row.get(2)?;
                Ok((path, manifest, ckv))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(heads
            .into_iter()
            .filter_map(|(path, manifest, ckv)| {
                <[u8; 32]>::try_from(manifest.as_slice())
                    .ok()
                    .map(|m| (path, m, ckv))
            })
            .collect())
    }

    /// Atomically capture the current files in a folder as a snapshot.
    pub async fn create_snapshot(&self, folder_id: i64) -> Result<SnapshotRow> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        let files = latest_live_snapshot_files(&conn, folder_id)?;

        let file_count = files.len() as i64;
        let total_bytes: i64 = files.iter().map(|f| f.size_bytes).sum();

        // Capture max_change_seq so the scheduler knows this snapshot covers all current changes
        let max_seq: Option<i64> = conn.query_row(
            "SELECT MAX(seq) FROM sync_changes WHERE folder_id = ?1",
            rusqlite::params![folder_id],
            |row| row.get(0),
        )?;

        conn.execute(
            "INSERT INTO snapshots (folder_id, created_at, file_count, total_bytes, max_change_seq)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![folder_id, now, file_count, total_bytes, max_seq],
        )?;
        let snapshot_id = conn.last_insert_rowid();

        let mut insert_stmt = conn.prepare(
            "INSERT INTO snapshot_files (snapshot_id, manifest_hash, size_bytes, mtime, path_hash, path_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for f in &files {
            insert_stmt.execute(rusqlite::params![
                snapshot_id,
                f.manifest_hash,
                f.size_bytes,
                f.mtime,
                f.path_hash,
                f.path_sealed
            ])?;
        }

        Ok(SnapshotRow {
            id: snapshot_id,
            folder_id,
            created_at: now,
            file_count,
            total_bytes,
            parent_id: None,
            deletion_pending: false,
            soft_deleted: false,
            purge_after: None,
            device_id: None,
            max_change_seq: max_seq,
            message_kind: None,
            message_manifest: None,
            placement_manifest: None,
            tag_hashes: None,
            // This legacy create writes no `tags` at all, so there is nothing to
            // seal a display copy of.
            tags_sealed: None,
        })
    }

    /// Create a snapshot with extended metadata (parent, tags, device_id).
    ///
    /// **One snapshot source** (`backup-restore.md` § 1): every snapshottable
    /// folder snapshots the latest-per-path projection over `sync_changes`, and
    /// the resulting `snapshot_files` rows are what pin superseded manifests
    /// against latest-per-path GC reclamation (point-in-time recovery at
    /// snapshot granularity). A custody copy is a cross-location backup
    /// **destination** and refuses snapshot create, like the other pure-backup
    /// destination gates (`message-segment-store.md` § Destination capability).
    /// `tags` never rest: the nest hashes them into `tag_hashes` (the
    /// pruner's key) and keeps nothing else of them. `tags_sealed` is the
    /// creating client's sealed display copy (path-sealing S6-d) — stored
    /// opaquely and never opened here; this nest holds no key for it. It is
    /// deliberately **not** derived from `tags`: only a keyed client can mint
    /// one, so a `None` here is a sealless snapshot rather than something to
    /// synthesize. Nest-minted snapshots (scheduler, restore, diff) pass
    /// `None`.
    pub async fn create_snapshot_v2(
        &self,
        folder_id: i64,
        device_id: Option<&[u8; 32]>,
        tags: &[String],
        tags_sealed: Option<&[u8]>,
        parent_id: Option<i64>,
    ) -> Result<SnapshotRow> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        let (custody_copy, set_name): (bool, String) = conn
            .query_row(
                "SELECT custody_copy, COALESCE(name, '') FROM folders WHERE id = ?1",
                rusqlite::params![folder_id],
                |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("folder {folder_id} not found"))?;
        if crate::db::snapshots::is_reserved_custody_copy(custody_copy, &set_name) {
            anyhow::bail!(
                "folder '{set_name}' is a pure backup destination — custodian-held \
                 custody is latest-per-path by contract and refuses snapshot create"
            );
        }

        // One change source for every snapshottable folder since the phase 3
        // head unification (2026-08-17): the `sync_changes` head plane —
        // ordinary Backup folders included.
        let files = latest_live_snapshot_files(&conn, folder_id)?;

        let file_count = files.len() as i64;
        let total_bytes: i64 = files.iter().map(|f| f.size_bytes).sum();
        // The wire still carries plaintext tags (this fn's `tags` param)
        // because the nest computes the retention pruner's matching key from
        // them (path-sealing S1): a JSON array of hex per-tag digests
        // positionally 1:1 with `tags`. The resting display copy is
        // `tags_sealed` alone (`file-sync.md` § Sealed names & paths; a
        // sealless create simply has no display copy — the ratified Omit
        // degrade).
        let tag_hashes_json = if tags.is_empty() {
            None
        } else {
            let hashes: Vec<String> = tags
                .iter()
                .map(|t| hex::encode(fauna_core::path_crypto::snapshot_tag_hash(t)))
                .collect();
            Some(serde_json::to_string(&hashes).unwrap_or_default())
        };
        let device_id_bytes = device_id.map(|d| d.as_slice());

        // The single watermark: `max_change_seq`, the `sync_changes` seq — one
        // plane, one clock, every mode (the retired `max_custody_updated_at`
        // twin left with the phase 3 head unification, schema v39).
        let max_seq: Option<i64> = conn.query_row(
            "SELECT MAX(seq) FROM sync_changes WHERE folder_id = ?1",
            rusqlite::params![folder_id],
            |row| row.get(0),
        )?;

        let insert = conn.execute(
            "INSERT INTO snapshots (folder_id, created_at, file_count, total_bytes, parent_id, device_id, max_change_seq, tag_hashes, tags_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![folder_id, now, file_count, total_bytes, parent_id, device_id_bytes, max_seq, tag_hashes_json, tags_sealed],
        );
        match insert {
            Ok(_) => {}
            // A snapshot for (folder_id, now) already exists — a concurrent
            // create landed in the same wall-clock second (the 60s auto-scheduler
            // racing a client's manual create, or two manual creates). The
            // `UNIQUE(folder_id, created_at)` constraint dedups to one snapshot
            // per folder per second; both creates see the same `sync_changes`
            // state, so the snapshot is logically identical. Return the existing
            // row rather than surface a spurious `snapshot.internal` — keeps the
            // op idempotent and preserves the "no client-causable error state"
            // invariant. (created_at is seconds-granularity, so this is the only
            // collision the constraint can produce.)
            Err(rusqlite::Error::SqliteFailure(f, _))
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return conn
                    .query_row(
                        &format!(
                            "SELECT {SNAPSHOT_COLS} FROM snapshots \
                             WHERE folder_id = ?1 AND created_at = ?2 AND message_kind IS NULL"
                        ),
                        rusqlite::params![folder_id, now],
                        row_to_snapshot_row,
                    )
                    .context("fetch existing same-second snapshot after dedup");
            }
            Err(e) => return Err(e.into()),
        }
        let snapshot_id = conn.last_insert_rowid();

        let mut insert_stmt = conn.prepare(
            "INSERT INTO snapshot_files (snapshot_id, manifest_hash, size_bytes, mtime, mode, file_type, symlink_target, path_hash, path_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for f in &files {
            insert_stmt.execute(rusqlite::params![
                snapshot_id,
                f.manifest_hash,
                f.size_bytes,
                f.mtime,
                0i64,
                "regular",
                Option::<String>::None,
                f.path_hash,
                f.path_sealed
            ])?;
        }

        Ok(SnapshotRow {
            id: snapshot_id,
            folder_id,
            created_at: now,
            file_count,
            total_bytes,
            parent_id,
            device_id: device_id.map(|d| d.to_vec()),
            max_change_seq: max_seq,
            deletion_pending: false,
            soft_deleted: false,
            purge_after: None,
            message_kind: None,
            message_manifest: None,
            placement_manifest: None,
            tag_hashes: tag_hashes_json,
            tags_sealed: tags_sealed.map(<[u8]>::to_vec),
        })
    }

    /// List all snapshots for a folder, newest first.
    /// Only returns folder snapshots (message_kind IS NULL) to preserve legacy
    /// behavior for existing callers; message-kind snapshots are fetched via
    /// dedicated DAO methods.
    pub async fn list_snapshots(&self, folder_id: i64) -> Result<Vec<SnapshotRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SNAPSHOT_COLS} FROM snapshots \
             WHERE folder_id = ?1 AND message_kind IS NULL \
             ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map(rusqlite::params![folder_id], row_to_snapshot_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Get a single snapshot by ID.
    pub async fn get_snapshot(&self, snapshot_id: i64) -> Result<Option<SnapshotRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SNAPSHOT_COLS} FROM snapshots WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![snapshot_id], row_to_snapshot_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List all files in a snapshot.
    pub async fn get_snapshot_files(&self, snapshot_id: i64) -> Result<Vec<SnapshotFileRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT manifest_hash, size_bytes, mtime, mode, file_type, symlink_target, path_hash, path_sealed
             FROM snapshot_files WHERE snapshot_id = ?1
             ORDER BY path_hash",
        )?;
        let rows = stmt.query_map(rusqlite::params![snapshot_id], |row| {
            Ok(SnapshotFileRow {
                manifest_hash: row.get(0)?,
                size_bytes: row.get(1)?,
                mtime: row.get(2)?,
                mode: row.get(3)?,
                file_type: row.get(4)?,
                symlink_target: row.get(5)?,
                path_hash: row.get(6)?,
                path_sealed: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Get a single file from a snapshot by path.
    ///
    /// **Addressed by `path_hash` alone** (post-flip PK): the HTTP restore
    /// route still receives a plaintext path, so the hash is derived here
    /// through `fauna_core::sync::path_hash` — the single owner of that
    /// digest. The pre-flip `path_hash IS NULL` plaintext fallback arm died
    /// with the plaintext column at the v32 rebuild.
    pub async fn get_snapshot_file(
        &self,
        snapshot_id: i64,
        path: &str,
    ) -> Result<Option<SnapshotFileRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT manifest_hash, size_bytes, mtime, mode, file_type, symlink_target, path_hash, path_sealed
             FROM snapshot_files WHERE snapshot_id = ?1 AND path_hash = ?2",
        )?;
        let mut rows = stmt.query_map(
            rusqlite::params![snapshot_id, fauna_core::sync::path_hash(path).to_vec()],
            |row| {
                Ok(SnapshotFileRow {
                    manifest_hash: row.get(0)?,
                    size_bytes: row.get(1)?,
                    mtime: row.get(2)?,
                    mode: row.get(3)?,
                    file_type: row.get(4)?,
                    symlink_target: row.get(5)?,
                    path_hash: row.get(6)?,
                    path_sealed: row.get(7)?,
                })
            },
        )?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Delete one or more snapshots and their files.
    ///
    /// NOTE: SQLite foreign_keys pragma is not enabled in this codebase,
    /// so ON DELETE CASCADE does not fire. We explicitly delete snapshot_files first.
    pub async fn delete_snapshots(&self, snapshot_ids: &[i64]) -> Result<u64> {
        if snapshot_ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        let mut count = 0u64;
        for &id in snapshot_ids {
            tx.execute(
                "DELETE FROM snapshot_files WHERE snapshot_id = ?1",
                rusqlite::params![id],
            )?;
            count +=
                tx.execute("DELETE FROM snapshots WHERE id = ?1", rusqlite::params![id])? as u64;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Collect all manifest hashes referenced by the given snapshot IDs.
    pub async fn snapshot_manifest_hashes(&self, snapshot_ids: &[i64]) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .snapshot_manifest_refs(snapshot_ids)
            .await?
            .into_iter()
            .map(|(h, _)| h)
            .collect())
    }

    /// [`Self::snapshot_manifest_hashes`], classified for the GC reference
    /// walk: each hash is paired with `direct_blob = true` when **every**
    /// referencing snapshot belongs to a **reserved** (`__`-prefixed) folder.
    /// Reserved sets are the WS-RPC direct-blob rails (`__drafts`,
    /// `__mls`, the `__index`/`__conv` segment stores, …): their
    /// `manifest_hash` IS the stored blob — client-sealed or segment bytes,
    /// never a decodable `ChunkManifest` — so GC pins the blob itself and must
    /// not try to walk chunks out of it (`docs/goal/behavior/backup-restore.md`
    /// § 9 Garbage Collection). A hash also referenced from an ordinary set
    /// (or from a row whose folder no longer resolves) classifies as a
    /// manifest — decode-required, the fail-closed side.
    pub async fn snapshot_manifest_refs(
        &self,
        snapshot_ids: &[i64],
    ) -> Result<Vec<(Vec<u8>, bool)>> {
        if snapshot_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let placeholders: Vec<String> = snapshot_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT DISTINCT sf.manifest_hash, COALESCE(f.name, '')
             FROM snapshot_files sf
             JOIN snapshots s ON s.id = sf.snapshot_id
             LEFT JOIN folders f ON f.id = s.folder_id
             WHERE sf.snapshot_id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = snapshot_ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pairs = Vec::new();
        for row in rows {
            pairs.push(row?);
        }
        Ok(fold_direct_blob_refs(pairs))
    }

    /// Collect all distinct non-null manifest hashes from **live** (not
    /// superseded) sync_changes rows. These are sync chunks that must not be
    /// garbage-collected.
    ///
    /// A manifest superseded via `supersede_sync_changes_for_path` drops out of
    /// this set — but only once **no** live row anywhere still references it
    /// (`DISTINCT` over the remaining live rows), so a manifest shared by
    /// another path / set / actor stays pinned by that other row. Snapshot pins
    /// (`snapshot_manifest_hashes`) and backup-custody pins are separate and
    /// unaffected.
    ///
    /// `superseded_before_millis` buffers in-flight readers: a row superseded
    /// strictly after the cutoff stays pinned, so a member mid-download of a
    /// just-superseded manifest gets the same grace window GC gives fresh blobs
    /// (blob `created_at` grace can't cover this — an old chunk's creation time
    /// long predates its supersede). Pass the GC grace cutoff; `i64::MAX`
    /// unpins every superseded row (no buffer).
    pub async fn sync_change_manifest_hashes(
        &self,
        superseded_before_millis: i64,
    ) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .sync_change_manifest_refs(superseded_before_millis, &[])
            .await?
            .into_iter()
            .map(|(h, _)| h)
            .collect())
    }

    /// [`Self::sync_change_manifest_hashes`], classified for the GC reference
    /// walk exactly like [`Self::snapshot_manifest_refs`]: `direct_blob = true`
    /// iff every live row referencing the hash belongs to a reserved
    /// (`__`-prefixed) folder — those rows' `manifest_hash` is the stored
    /// blob itself (a client-sealed rail blob or segment bytes), never a
    /// decodable `ChunkManifest`.
    /// `exclude_folder_ids` drops the named sets' rows from the reference
    /// scan — the `__index` boot purge passes its own sets (their references
    /// are precisely what it is deleting; nothing else may lose a pin), the
    /// GC passes none. Ids are our own row ids, inlined.
    pub async fn sync_change_manifest_refs(
        &self,
        superseded_before_millis: i64,
        exclude_folder_ids: &[i64],
    ) -> Result<Vec<(Vec<u8>, bool)>> {
        let exclude_clause = if exclude_folder_ids.is_empty() {
            String::new()
        } else {
            let csv = exclude_folder_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("AND (sc.folder_id IS NULL OR sc.folder_id NOT IN ({csv}))")
        };
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT sc.manifest_hash, COALESCE(f.name, '')
             FROM sync_changes sc
             LEFT JOIN folders f ON f.id = sc.folder_id
             WHERE sc.manifest_hash IS NOT NULL
               AND (sc.superseded_at IS NULL OR sc.superseded_at > ?1)
               {exclude_clause}"
        ))?;
        let rows = stmt.query_map(rusqlite::params![superseded_before_millis], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pairs = Vec::new();
        for row in rows {
            pairs.push(row?);
        }
        Ok(fold_direct_blob_refs(pairs))
    }

    /// Ids of every folder whose owner opted into metadata-only content
    /// residency (phase 5 — `file-sync.md` § Content residency). The GC's
    /// residency arm and the flip-time chunk drop key on this set: these
    /// folders' manifests stay pinned (they ARE the metadata) while their
    /// chunk store keys are deliberately not reachable through them.
    pub async fn metadata_only_folder_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT id FROM folders WHERE nest_content_residency = 'metadata_only'")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Does this actor own **any** folder at metadata-only residency?
    ///
    /// The relay read path's cache gate (`file-sync.md` § Content residency,
    /// enforcement gate 3). The by-hash chunk rails carry
    /// no folder attribution of their own, and the route reads its cache
    /// policy from the *hinted* folder. If the owner has a metadata-only folder
    /// anywhere, an answer not attributed to the hinted folder may be that
    /// folder's — and writing it to the blob store would break the
    /// exact promise the owner was given a UI switch for.
    ///
    /// So this is the cheap, exact discriminator for "is there anything to
    /// protect": one indexed query, `false` for every owner who has not opted
    /// in (which is the overwhelming majority, and the caching path they have
    /// today is unchanged). It deliberately answers per-OWNER rather than
    /// per-chunk: a per-chunk answer would need the attribution the rails do
    /// not carry — see the caller for the trade that buys and what closes it.
    pub async fn actor_has_metadata_only_folder(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM folders
                 WHERE actor_id = ?1 AND nest_content_residency = 'metadata_only'
                 LIMIT 1",
                rusqlite::params![actor],
                |row| row.get(0),
            )
            .optional()
            .context("checking the owner for a metadata-only folder")?;
        Ok(found.is_some())
    }

    /// EVERY distinct manifest hash the named folders' `sync_changes` rows
    /// reference — live AND superseded alike. The phase-5 chunk drop and the
    /// GC's pin-only arm walk these: a metadata-only flip must clear the
    /// bytes behind superseded generations too, not only the head
    /// (`file-sync.md` § Content residency — the nest's copy of the folder's
    /// content is deleted, whole), and a superseded manifest blob is still
    /// metadata to keep.
    pub async fn all_sync_change_manifest_hashes_for_folders(
        &self,
        folder_ids: &[i64],
    ) -> Result<Vec<Vec<u8>>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let csv = folder_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT manifest_hash FROM sync_changes
             WHERE manifest_hash IS NOT NULL AND folder_id IN ({csv})"
        ))?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut hashes = Vec::new();
        for row in rows {
            hashes.push(row?);
        }
        Ok(hashes)
    }

    /// Snapshot ids belonging to the named folders — the snapshot half of the
    /// GC residency partition ([`Self::metadata_only_folder_ids`]): a
    /// metadata-only folder's snapshots stay (manifest-level pointer rows,
    /// complete in metadata) but must not chunk-walk.
    pub async fn list_snapshot_ids_for_folders(&self, folder_ids: &[i64]) -> Result<Vec<i64>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let csv = folder_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM snapshots WHERE folder_id IN ({csv})"
        ))?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Every live row referencing a manifest hash, formatted for GC's
    /// diagnostics: `"<source> set '<name>' actor <hex-prefix> path '<path>'"`.
    /// Called only on GC's rare failure paths (an undecodable or hash-mismatched
    /// reference), so a fail-close names the rows an admin or the recording
    /// client must act on instead of a bare blob hash (`backup-restore.md` § 9
    /// — recovery must be reachable without off-box DB surgery, and that starts
    /// with knowing which set/path holds the poisoned reference). Capped, since
    /// a hash may be referenced by arbitrarily many rows.
    /// A GC-debug diagnostic only — never fetches the plaintext `name`/`path`
    /// (only their hash companions), so the redaction is defense in depth: even
    /// a future caller that dumps this function's own SQL cannot recover
    /// plaintext through it (`docs/goal/behavior/file-sync.md` § Sealed names
    /// & paths, S7 the log + error-string scrub).
    pub async fn manifest_reference_sources(&self, manifest_hash: &[u8]) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT src, name_hash, actor_hex, path_hash FROM (
                 SELECT 'sync_changes' AS src, f.name_hash AS name_hash,
                        hex(sc.actor_id) AS actor_hex, sc.path_hash AS path_hash
                 FROM sync_changes sc LEFT JOIN folders f ON f.id = sc.folder_id
                 WHERE sc.manifest_hash = ?1 AND sc.superseded_at IS NULL
                 UNION ALL
                 SELECT 'backup_custody', f.name_hash,
                        hex(bc.uploader_actor), bc.path_hash
                 FROM backup_custody bc LEFT JOIN folders f ON f.id = bc.folder_id
                 WHERE bc.manifest_hash = ?1
                 UNION ALL
                 SELECT 'snapshot_files', f.name_hash,
                        '', sf.path_hash
                 FROM snapshot_files sf
                 JOIN snapshots s ON s.id = sf.snapshot_id
                 LEFT JOIN folders f ON f.id = s.folder_id
                 WHERE sf.manifest_hash = ?1
             ) LIMIT 20",
        )?;
        let rows = stmt.query_map(rusqlite::params![manifest_hash], |row| {
            let src: String = row.get(0)?;
            let name_hash: Option<Vec<u8>> = row.get(1)?;
            let actor: String = row.get(2)?;
            let path_hash: Option<Vec<u8>> = row.get(3)?;
            let name = match name_hash {
                Some(h) => fauna_core::log_redact::log_hash_prefix("name", &h),
                None => "?".to_string(),
            };
            let path = match path_hash {
                Some(h) => fauna_core::log_redact::log_hash_prefix("path", &h),
                None => "?".to_string(),
            };
            let actor = if actor.is_empty() {
                String::new()
            } else {
                format!(" actor {}", &actor[..actor.len().min(16)])
            };
            Ok(format!("{src} set {name}{actor} {path}"))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Async wrapper over [`collapse_reserved_rail_history_in_conn`] for a rail
    /// whose INSERT does not already hold the `conn` (the `__drafts` recorder,
    /// which appends through [`Self::record_sync_change`]). Taking the lock a
    /// second time is safe: the collapse marks only rows strictly below the seq
    /// its own INSERT returned, so an interleaved writer's newer row is never
    /// marked and its own collapse mops up ours.
    pub async fn collapse_reserved_rail_history(
        &self,
        folder_id: i64,
        path_hash: &[u8; 32],
        head_seq: i64,
    ) -> Result<u64> {
        let path_hash = *path_hash;
        let conn = self.conn.lock().await;
        collapse_reserved_rail_history_in_conn(&conn, folder_id, &path_hash, head_seq)
    }

    /// Mark one path's pre-head manifest rows superseded, so their
    /// now-unreferenced chunks become GC-eligible (the destructive half of the
    /// M2 pre-bind re-seal migration — `mls-group-key-material.md` § M2
    /// *Pre-bind re-seal migration* bullet B; ordering bound by
    /// `webdav-server.md` § Architectural rules "deletes nothing until the
    /// re-sealed copy is verified").
    ///
    /// `verified_manifest_hash` is the manifest the caller has verified
    /// retrievable + decryptable end-to-end. The mark happens only if it equals
    /// the path's current live head (newest non-superseded manifest row), and
    /// only rows **strictly below** the head are marked — so the head is
    /// structurally unmarkable, every superseded row always has a strictly-later
    /// live manifest row for its `(folder, path)`, and a stale or wrong verify
    /// can never unpin live data. Rows are marked, never deleted (append-only
    /// history; the `backup_custody` tombstone precedent). Idempotent: a re-run
    /// marks 0 rows. Quota needs no adjustment — the live byte sum is already
    /// latest-per-path (`record_sync_change_metered` delta arithmetic).
    pub async fn supersede_sync_changes_for_path(
        &self,
        folder_id: i64,
        path_hash: &[u8; 32],
        verified_manifest_hash: &[u8; 32],
    ) -> Result<SupersedeOutcome> {
        let path_hash = *path_hash;
        let verified = *verified_manifest_hash;
        // One held conn = the atomicity unit (the upsert_backup_custody
        // precedent): the head read and the mark cannot interleave with a
        // concurrent record or supersede.
        let conn = self.conn.lock().await;
        // `change_type != 'delete'` aligns the head definition with
        // `get_files_for_folder`'s notion of "live": every current delete
        // caller nulls the manifest, but the data plane forwards the wire
        // manifest regardless, so a delete-with-manifest row must never be
        // resolvable as the head (defense-in-depth, adversarial review 2026-07-07).
        let head: Option<(i64, Vec<u8>)> = conn
            .query_row(
                "SELECT seq, manifest_hash FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2
                   AND manifest_hash IS NOT NULL AND superseded_at IS NULL
                   AND change_type != 'delete'
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![folder_id, path_hash.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("query supersede head")?;
        let Some((head_seq, head_manifest)) = head else {
            return Ok(SupersedeOutcome::HeadMismatch);
        };
        if head_manifest != verified.as_slice() {
            return Ok(SupersedeOutcome::HeadMismatch);
        }
        let now = now_epoch_millis();
        // The rows this call NEWLY releases from the charged population —
        // read before the mark. A row the retention pipeline already
        // soft-pruned (`pruned_at IS NOT NULL`) was credited at that hop:
        // the mark below still terminates it (the owner's floorless privacy
        // instrument overrides the recovery window by design), but it must
        // not credit twice (§ Retention (4)). Nor does a row that never held
        // the charge (`charged = 0`, ruling (11)(g)) — and one that does hands
        // it to the head when the head is an uncharged row of its manifest.
        let released: Vec<ChargedVersion> = {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, manifest_hash, actor_id, size_bytes FROM sync_changes
                     WHERE folder_id = ?1 AND path_hash = ?2 AND seq < ?3
                       AND manifest_hash IS NOT NULL AND superseded_at IS NULL
                       AND pruned_at IS NULL AND charged = 1",
                )
                .context("prepare supersede accounting read")?;
            let rows = stmt
                .query_map(
                    rusqlite::params![folder_id, path_hash.as_slice(), head_seq],
                    |r| {
                        Ok(ChargedVersion {
                            seq: r.get(0)?,
                            path_hash: path_hash.to_vec(),
                            manifest_hash: r.get(1)?,
                            actor: r.get(2)?,
                            size_bytes: r.get(3)?,
                        })
                    },
                )
                .context("query supersede accounting rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("read supersede accounting row")?);
            }
            out
        };
        let marked = conn
            .execute(
                "UPDATE sync_changes SET superseded_at = ?1
                 WHERE folder_id = ?2 AND path_hash = ?3 AND seq < ?4
                   AND manifest_hash IS NOT NULL AND superseded_at IS NULL",
                rusqlite::params![now, folder_id, path_hash.as_slice(), head_seq],
            )
            .context("mark superseded sync changes")? as u64;
        release_version_charges_in_conn(&conn, folder_id, &released)?;
        Ok(SupersedeOutcome::Marked(marked))
    }

    // ── Backup custody (custodian-authoritative GC-safety) ───────────────────
    //
    // On a destination nest, a cross-location backup is a file-sync of the
    // owner's reserved custody-copy folder. The `fauna.sync.changes.record`
    // handler routes those records here (not into the append-only `sync_changes`
    // device-sync feed): the latest non-deleted manifest per `(folder, path)`.
    // GC walks this set so a live backup blob is never deleted; reclamation
    // fires on supersede / compacted-out / destination removal. See
    // `docs/goal/architecture/message-segment-store.md` § Cross-location backup
    // protocol → *GC-safety — custodian-authoritative custody* and
    // `bins/fauna-nest/src/db/migrations.rs` (`MIGRATIONS_BACKUP_CUSTODY`).

    /// Record (UPSERT) the current manifest for one backup path, with atomic
    /// per-actor storage-quota accounting + enforcement (the custody twin of
    /// [`Self::record_sync_change_metered`]). A re-upload of the same path
    /// supersedes the prior manifest (its now-exclusive chunks become
    /// GC-eligible). `uploader_actor` is the authenticated bearer and the actor
    /// charged. `size_bytes` is the CHARGE, and the stored value is what every
    /// credit leg (tombstone, T-reclaim, set-delete) later reads back — charge
    /// and credit stay symmetric by construction. For a **reserved destination**
    /// set the caller derives it from what this nest actually holds under the
    /// manifest (`crate::backup::custody_charge` — never the writer's
    /// declaration); for an ordinary folder it is the owner's own
    /// client's declared logical size.
    ///
    /// Under one `conn` lock: reads the path's prior live `size_bytes` (0 if new
    /// or tombstoned), computes the supersede delta, rejects with
    /// [`StorageQuotaError::Exceeded`] iff a positive delta would exceed
    /// `max_storage_bytes`, else upserts (storing the new `size_bytes`) and moves
    /// `users.storage_bytes_used` by the delta.
    pub async fn upsert_backup_custody(
        &self,
        uploader_actor: &[u8; 32],
        folder_id: i64,
        path_hash: &[u8; 32],
        path: Option<&str>,
        manifest_hash: &[u8; 32],
        size_bytes: i64,
        thumbnail_hash: Option<&str>,
        path_sealed: Option<&[u8]>,
        max_storage_bytes: i64,
    ) -> Result<(), StorageQuotaError> {
        // The same refusal the sync twin makes, for the same reason and BEFORE
        // anything is read or written. Sweep closed the
        // negative-size credit in `record_sync_change_metered` and placed it in
        // that metering core "so a sixth door cannot be added past it" — but
        // this function is a SECOND core, not a sixth door: for an ordinary
        // backup-type set `size_bytes` is the owner's client's *declared*
        // logical size (see this fn's doc comment), and nothing checked it. A
        // negative one makes `delta` negative, which skips the `delta > 0`
        // ceiling check and then credits the uploader — measured PROBE-389-F:
        // a full 1000-byte tier floored to 0 by one record, and a 900-byte
        // write over the full tier then succeeded. The reserved-destination
        // arm derives its charge from bytes this nest holds
        // (`backup::custody_charge`) and so was never at risk; this guard costs
        // it nothing.
        if size_bytes < 0 {
            return Err(StorageQuotaError::NegativeSize { size_bytes });
        }
        let uploader = *uploader_actor;
        let path_hash = *path_hash;
        let path = path.map(|s| s.to_string());
        let manifest_hash = *manifest_hash;
        let thumbnail_hash = thumbnail_hash.map(|s| s.to_string());
        let path_sealed = path_sealed.map(<[u8]>::to_vec);
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        // Prior live (manifest, size) for this custody path — `None` if the path
        // is new or already tombstoned.
        let prior_row: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT manifest_hash, size_bytes FROM backup_custody
                 WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash IS NOT NULL",
                rusqlite::params![folder_id, path_hash.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("query prior backup_custody row")?;
        let prior: i64 = prior_row.as_ref().map(|(_, s)| *s).unwrap_or(0);

        // A content-identical re-record (the same manifest is already live for
        // this path — a reconnect / crashed-ack re-drive) is a freshness touch,
        // not a supersede: nothing is superseded, nothing freed, nothing newly
        // held. Charging or retaining here would put the live manifest in the
        // retained set — the both-live-and-retained state
        // `unretain_custody_generation_in_conn`'s invariant forbids — and
        // double-charge the owner until T healed it. `size_bytes` deliberately
        // stays untouched: the stored value IS the charged amount, and moving
        // it without moving `storage_bytes_used` would break credit symmetry.
        if let Some((prior_manifest, _)) = &prior_row
            && prior_manifest.as_slice() == manifest_hash.as_slice()
        {
            conn.execute(
                "UPDATE backup_custody SET
                     uploader_actor = ?1,
                     updated_at     = ?2,
                     thumbnail_hash = COALESCE(?3, thumbnail_hash),
                     path_sealed    = COALESCE(?4, path_sealed)
                 WHERE folder_id = ?5 AND path_hash = ?6",
                rusqlite::params![
                    uploader.as_slice(),
                    now,
                    thumbnail_hash,
                    path_sealed.as_deref(),
                    folder_id,
                    path_hash.as_slice(),
                ],
            )
            .context("refresh content-identical custody record")?;
            return Ok(());
        }

        // Grace window: on a reserved destination set the generation we are
        // about to supersede is RETAINED rather than forgotten, so a rogue
        // source nest's supersede cannot delete the owner's backup. A retained
        // generation keeps its bytes charged, so the charge for this write is
        // the full new size (nothing was freed) rather than the supersede delta.
        //
        // ⚠ Every read + the quota decision happen BEFORE any write. A refused
        // write must leave the custody tables untouched: retaining first would
        // leave a manifest both live and retained on the refusal path, breaking
        // the invariant the window rests on (and over-pinning it in the GC walk).
        let retains = custody_set_retains_generations(&conn, folder_id)?;
        let retained = retains && prior_row.is_some();

        // If this content is itself a retained generation (the path flapped back
        // to it), it becomes live again and leaves the retained set — its bytes
        // are already charged, so only the difference is new.
        let promoted =
            retained_custody_generation_bytes(&conn, folder_id, &path_hash, &manifest_hash)?;

        // Charge = (new live bytes not already charged) − (bytes actually freed).
        // A retained prior frees nothing: its bytes stay charged for the whole
        // grace window, which is what makes quota the supersede rate cap.
        let freed = if retained { 0 } else { prior };
        let delta = (size_bytes - promoted) - freed;

        let used: i64 = conn
            .query_row(
                "SELECT storage_bytes_used FROM users WHERE actor_id = ?1",
                rusqlite::params![uploader.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("read storage_bytes_used")?
            .unwrap_or(0);
        // `checked_add` for the same reason as the sync twin's
        // ceiling: an unrepresentable total must read as over the ceiling,
        // never wrap past this comparison into an admitted record.
        if delta > 0
            && used
                .checked_add(delta)
                .is_none_or(|total| total > max_storage_bytes)
        {
            return Err(StorageQuotaError::Exceeded {
                used,
                requested: delta,
                max: max_storage_bytes,
            });
        }

        // Past the refusal point — now the writes.
        if let Some((prior_manifest, prior_size)) = &prior_row
            && retains
        {
            retain_custody_generation_in_conn(
                &conn,
                folder_id,
                &uploader,
                &path_hash,
                prior_manifest,
                *prior_size,
                path.as_deref(),
                now,
            )?;
        }
        if promoted != 0 {
            unretain_custody_generation_in_conn(&conn, folder_id, &path_hash, &manifest_hash)?;
        }

        conn.execute(
            "INSERT INTO backup_custody
                 (uploader_actor, folder_id, path_hash, manifest_hash, size_bytes, updated_at, path, thumbnail_hash, path_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(folder_id, path_hash) DO UPDATE SET
                 uploader_actor = excluded.uploader_actor,
                 manifest_hash  = excluded.manifest_hash,
                 size_bytes     = excluded.size_bytes,
                 updated_at     = excluded.updated_at,
                 path           = excluded.path,
                 thumbnail_hash = excluded.thumbnail_hash,
                 -- COALESCE, not overwrite: a keyless re-record (an FFI or
                 -- restore writer) must never blank a seal an earlier keyed
                 -- writer already stored for this path.
                 path_sealed    = COALESCE(excluded.path_sealed, backup_custody.path_sealed)",
            rusqlite::params![
                uploader.as_slice(),
                folder_id,
                path_hash.as_slice(),
                manifest_hash.as_slice(),
                size_bytes,
                now,
                path,
                thumbnail_hash,
                path_sealed.as_deref(),
            ],
        )
        .context("upsert backup custody")?;

        if delta != 0 {
            conn.execute(
                "UPDATE users SET storage_bytes_used = MAX(0, storage_bytes_used + ?1)
                 WHERE actor_id = ?2",
                rusqlite::params![delta, uploader.as_slice()],
            )
            .context("update storage_bytes_used")?;
        }
        Ok(())
    }

    /// Tombstone one backup path (a compacted-out segment): set its manifest to
    /// NULL so GC drops its now-orphaned chunks. Kept as a row (not deleted) so
    /// the path's history is auditable; GC ignores NULL-manifest rows. Credits
    /// the path's prior live `size_bytes` back to the uploader's storage quota
    /// and zeroes the row's `size_bytes` (a tombstone holds no live bytes).
    pub async fn tombstone_backup_custody(
        &self,
        folder_id: i64,
        path_hash: &[u8; 32],
    ) -> Result<()> {
        let path_hash = *path_hash;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        // The bytes to credit back + whom to credit (None if absent / already
        // tombstoned — nothing live to reclaim).
        let prior: Option<(Vec<u8>, i64, Vec<u8>, Option<String>)> = conn
            .query_row(
                "SELECT uploader_actor, size_bytes, manifest_hash, path FROM backup_custody
                 WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash IS NOT NULL",
                rusqlite::params![folder_id, path_hash.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .context("query backup_custody for tombstone")?;

        // Grace window (reserved destination sets): a tombstone is the *other*
        // half of a rogue writer's delete power, so the generation it removes is
        // retained exactly like a supersede's — and therefore stays charged.
        let retained = if let Some((uploader, size, manifest, path)) = &prior
            && custody_set_retains_generations(&conn, folder_id)?
        {
            let uploader: [u8; 32] = blob_to_array(uploader.as_slice(), "uploader_actor")?;
            retain_custody_generation_in_conn(
                &conn,
                folder_id,
                &uploader,
                &path_hash,
                manifest,
                *size,
                path.as_deref(),
                now,
            )?
        } else {
            false
        };

        conn.execute(
            "UPDATE backup_custody SET manifest_hash = NULL, size_bytes = 0, updated_at = ?3
             WHERE folder_id = ?1 AND path_hash = ?2",
            rusqlite::params![folder_id, path_hash.as_slice(), now],
        )
        .context("tombstone backup custody")?;

        if let Some((uploader, size, _, _)) = prior
            && size != 0
            && !retained
        {
            conn.execute(
                "UPDATE users SET storage_bytes_used = MAX(0, storage_bytes_used - ?1)
                 WHERE actor_id = ?2",
                rusqlite::params![size, uploader.as_slice()],
            )
            .context("decrement storage_bytes_used on tombstone")?;
        }
        Ok(())
    }

    /// Collect the retained superseded generations' manifest hashes — the second
    /// half of the destination's GC reference set (the live half is
    /// [`Self::backup_custody_manifest_hashes`]).
    ///
    /// Every row here is by construction still inside the grace window: the GC
    /// reclaims expired rows ([`Self::reclaim_expired_backup_custody_generations`])
    /// **before** it builds its reference set, so a row that exists is a row
    /// whose chunks must survive this sweep. Ordering the two the other way
    /// would open a window in which an about-to-expire generation was neither
    /// reclaimed nor protected — the over-delete direction, which the
    /// no-user-data-loss invariant forbids.
    pub async fn backup_custody_generation_manifest_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT DISTINCT manifest_hash FROM backup_custody_generations")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Reclaim every retained generation superseded at or before `cutoff`
    /// (epoch seconds — the caller passes `now - T`): drop the row and credit its
    /// bytes back to the uploader's storage quota. The chunks it was protecting
    /// become GC-eligible on this same sweep, which is what makes T the actual
    /// retention bound rather than an unbounded leak. Returns the row count.
    ///
    /// Idempotent and crash-safe: the credit and the delete ride one statement
    /// pair under the held connection, and a row that is already gone credits
    /// nothing.
    pub async fn reclaim_expired_backup_custody_generations(&self, cutoff: i64) -> Result<u64> {
        let conn = self.conn.lock().await;

        // Credit each affected owner back in one grouped pass, then drop the
        // rows — so a partial failure can only over-charge (safe), never
        // under-charge while the bytes are gone.
        let mut stmt = conn.prepare(
            "SELECT uploader_actor, COALESCE(SUM(size_bytes), 0)
             FROM backup_custody_generations
             WHERE superseded_at <= ?1
             GROUP BY uploader_actor",
        )?;
        let credits: Vec<(Vec<u8>, i64)> = stmt
            .query_map(rusqlite::params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        for (uploader, bytes) in &credits {
            if *bytes != 0 {
                conn.execute(
                    "UPDATE users SET storage_bytes_used = MAX(0, storage_bytes_used - ?1)
                     WHERE actor_id = ?2",
                    rusqlite::params![bytes, uploader.as_slice()],
                )
                .context("credit back reclaimed custody generation bytes")?;
            }
        }

        let removed = conn
            .execute(
                "DELETE FROM backup_custody_generations WHERE superseded_at <= ?1",
                rusqlite::params![cutoff],
            )
            .context("delete expired backup custody generations")?;
        Ok(removed as u64)
    }

    /// Test-only: age every retained generation back by `secs`, so a test can
    /// cross the 30-day grace window without sleeping through it.
    ///
    /// Backdating the row is the *latency-independent* way to test a timer
    /// (testing.md § point 14): the assertion is about state on either side of
    /// the boundary, never about wall-clock. Compiled out of release artifacts
    /// (§ point 15) — `debug_assertions` is on for `tests/`, off for a shipped
    /// nest, so this can never be reached in production. Returns the row count.
    #[cfg(any(debug_assertions, feature = "test-hooks"))]
    pub async fn backdate_backup_custody_generations_for_test(&self, secs: i64) -> Result<u64> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE backup_custody_generations SET superseded_at = superseded_at - ?1",
                rusqlite::params![secs],
            )
            .context("backdate retained custody generations")?;
        Ok(n as u64)
    }

    /// Every **live** custody row this nest holds for `owner` — the read behind
    /// `fauna.backup.custody.list`, and the source-untrusted observable the
    /// client-side audit loop checks a backup's freshness against
    /// (`docs/goal/behavior/backup-restore.md` § Background Tasks).
    ///
    /// Why it exists next to [`Self::list_backup_custody_generations`] rather
    /// than being folded into it: that one reports the *superseded* generations
    /// retained inside the grace window `T`, which answers "what can I roll back
    /// to". This one answers "what is actually here right now", which is the
    /// question an audit asks. A client needs both, and conflating them would
    /// let a destination holding nothing but retained tombstones read as a
    /// healthy backup.
    ///
    /// Scoped by the **folder's** `actor_id`, so a caller only ever sees
    /// custody held for itself — never the uploading writer's other owners.
    /// Tombstoned paths (`manifest_hash IS NULL`, the compacted-out `delete`
    /// record) are excluded: a tombstone is the absence of custody, and an audit
    /// that counted it as presence would pass a destination that has dropped
    /// everything.
    /// `after` resumes past a previous page's last row — `(updated_at, rowid)`,
    /// the deterministic serve order's own key (`transport.md` § Max frame
    /// corollary: pages are contiguous-cursor walks, never skip-and-continue).
    /// `limit` bounds the fetch (the serve handler additionally cuts at the
    /// frame byte budget); non-positive means "no row bound before the budget".
    /// Every live custody row of ONE named set, unpaged.
    ///
    /// [`Self::list_backup_custody`] is the owner's paged audit read across all
    /// of their sets; this is the set-scoped read `fauna.backup.custody.materialize`
    /// needs — it reconstitutes one set and would otherwise have to walk (and
    /// hold in memory) the whole account's custody to find the handful of paths
    /// belonging to it. Unpaged deliberately: a materialize is a bounded
    /// recovery ceremony over one set, and a partial view of the set is not a
    /// smaller job but a wrong one.
    pub async fn list_backup_custody_in_set(
        &self,
        owner: &[u8; 32],
        set_name: &str,
    ) -> Result<Vec<BackupCustodyRow>> {
        let owner = *owner;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(f.name, ''), c.path, c.path_hash, c.manifest_hash, c.size_bytes, c.updated_at,
                    c.rowid, c.path_sealed
             FROM backup_custody c
             INNER JOIN folders f ON f.id = c.folder_id
             WHERE f.actor_id = ?1 AND f.name = ?2 AND c.manifest_hash IS NOT NULL
             ORDER BY c.rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner.as_slice(), set_name], |r| {
            Ok(BackupCustodyRow {
                folder_name: r.get(0)?,
                path: r.get(1)?,
                path_hash: r.get(2)?,
                manifest_hash: r.get(3)?,
                size_bytes: r.get(4)?,
                updated_at: r.get(5)?,
                rowid: r.get(6)?,
                path_sealed: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every **retained** generation of ONE named set, unpaged — the set-scoped
    /// sibling of [`Self::list_backup_custody_generations`], as
    /// [`Self::list_backup_custody_in_set`] is of the live list.
    ///
    /// `fauna.backup.custody.recover` needs it: a regressed source's lost
    /// segments sit at the destination either live or retained `T` from the
    /// supersede that overwrote their id, and a delivery lands every generation
    /// of a path on the source, where each later one supersedes the earlier into
    /// this table. So the verb reads live rows and these alike. Unpaged for
    /// materialize's reason: a partial view of the set is a wrong answer, not a
    /// smaller one.
    pub async fn list_backup_custody_generations_in_set(
        &self,
        owner: &[u8; 32],
        set_name: &str,
    ) -> Result<Vec<BackupCustodyGenerationRow>> {
        let owner = *owner;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(f.name, ''), g.path, g.path_hash, g.manifest_hash, g.size_bytes, g.superseded_at,
                    g.rowid
             FROM backup_custody_generations g
             INNER JOIN folders f ON f.id = g.folder_id
             WHERE f.actor_id = ?1 AND f.name = ?2
             ORDER BY g.superseded_at ASC, g.rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner.as_slice(), set_name], |r| {
            Ok(BackupCustodyGenerationRow {
                folder_name: r.get(0)?,
                path: r.get(1)?,
                path_hash: r.get(2)?,
                manifest_hash: r.get(3)?,
                size_bytes: r.get(4)?,
                superseded_at: r.get(5)?,
                rowid: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Re-home one page of a covered-folder mirror's custody into a live folder
    /// — the write half of `fauna.backup.custody.materialize`'s folder arm.
    ///
    /// **Pure row re-homing, no keys anywhere**
    /// (`docs/goal/architecture/message-segment-store.md` § Client-device
    /// custodian (pull) → *Restore*): the mirror's manifests and chunks already
    /// ARE the at-rest folder corpus, so nothing here unseals, re-seals or even
    /// reads a byte of content. Each live row is minted from the triple the
    /// custody row carries — the source's `path_hash`, its `path_sealed` name,
    /// and the `manifest_hash` — under the owner's signature the arm verified
    /// (`page`), with the owner's re-seed pseudo-device as `device_id`. The
    /// target is the live set the ceremony's seed-holding process created under
    /// the folder's display name (`writer-signed-change-records.md` ruling
    /// (7)(a)(i)); this function never creates a folder, and refuses
    /// ([`FolderMaterializeOutcome::Missing`]) one the owner does not list.
    /// `intended` is the whole custody set — what the classifier calls "ours"
    /// and what `remaining` counts against — and `page` the rows this call
    /// writes.
    ///
    /// ## One transaction, and why the empty-target rule is enforced only inside it
    ///
    /// The rule is asserted **here and nowhere else**, against the same
    /// transaction that does the writing. That is deliberate rather than
    /// incidental: a check at the top of the verb would hold no lock, so a
    /// folder could go from fresh to lived-in — or from private to shared —
    /// while the custody read runs. The segments arm learned this the expensive
    /// way (its top-of-verb read let mail delivered during the corpus fetch
    /// merge into a scope that had since become live), and the folder axis
    /// inherits the lesson even though it moves no bytes. An outer early-out
    /// would buy nothing this one does not already promise, and would be a
    /// second place for the rule to drift.
    ///
    /// ## The rule is FRESHNESS, of which emptiness is one half
    ///
    /// A target refuses if it holds live records this ceremony did not write —
    /// and equally if it holds none but carries any publication-bearing property
    /// ([`TargetFolderPosture`]): group-bound, public, website-serving,
    /// WebDAV-exposed or paywalled. Both are the one ratified rule
    /// (`backup-destinations.md` § Third destination kind → *Re-seed*: "target
    /// freshness is per-scope and enforced at materialize"), and neither has a
    /// force arm.
    ///
    /// ## Torn runs resume rather than locking the owner out
    ///
    /// A run interrupted partway leaves a target holding some of this very
    /// ceremony's rows. Refusing that as "not empty" would make the set
    /// permanently un-materializable under a rule whose whole point is to
    /// protect data — so an existing row is classified the way the segments
    /// arm classifies an occupied segment path: a row whose `(path_hash,
    /// manifest_hash)` is one this call would itself write is **ours**, and it
    /// is skipped, not rewritten; anything else is a lived-in folder and
    /// refuses. Nothing is ever deleted or overwritten on either branch.
    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_folder_custody(
        &self,
        owner: &[u8; 32],
        folder_id: i64,
        set_nonce: &[u8; 32],
        device_id: &[u8; 32],
        intended: &[FolderRehomeRow],
        page: &[FolderRehomeSigned<'_>],
        max_storage_bytes: i64,
    ) -> Result<FolderMaterializeOutcome, StorageQuotaError> {
        let owner = *owner;
        let device_id = *device_id;
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin the folder materialize transaction")
            .map_err(StorageQuotaError::Db)?;

        // The target's identity AND its posture in one read, inside the
        // transaction: a folder can be shared, published or served WHILE the
        // custody read runs, so a freshness check outside this lock would be the
        // same top-of-verb read the segments arm already paid for once.
        let existing: Option<(i64, TargetFolderPosture, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT id, mls_group_id, audience, website_enabled, webdav_enabled,
                        web_paywall_tier, set_nonce
                 FROM folders WHERE actor_id = ?1 AND id = ?2 AND custody_copy = 0",
                rusqlite::params![owner.as_slice(), folder_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        TargetFolderPosture {
                            mls_group_id: r.get(1)?,
                            audience: r.get(2)?,
                            website_enabled: r.get::<_, i64>(3)? != 0,
                            webdav_enabled: r.get::<_, i64>(4)? != 0,
                            web_paywall_tier: r.get(5)?,
                        },
                        r.get(6)?,
                    ))
                },
            )
            .optional()
            .context("look up the materialize target folder by id")
            .map_err(StorageQuotaError::Db)?;

        // Every `(path_hash, manifest_hash)` this call means to write — the
        // classifier's notion of "ours".
        let intended_by_path: std::collections::HashMap<[u8; 32], [u8; 32]> = intended
            .iter()
            .map(|r| (r.path_hash, r.manifest_hash))
            .collect();

        let (folder_id, already) = match existing {
            None => return Ok(FolderMaterializeOutcome::Missing),
            Some((_, _, stored)) if stored.as_deref() != Some(&set_nonce[..]) => {
                return Ok(FolderMaterializeOutcome::NonceMoved);
            }
            Some((id, posture, _)) => {
                // Freshness before emptiness: a folder with an audience refuses
                // whether or not it holds records, and saying so by property
                // rather than by row count is what tells the owner which knob to
                // clear. The transaction drops unwritten either way.
                if let Some(property) = posture.unfresh_property() {
                    return Ok(FolderMaterializeOutcome::NotFresh { property });
                }
                let found = {
                    let mut stmt = tx
                        .prepare(
                            "SELECT path_hash, manifest_hash FROM sync_changes
                             WHERE folder_id = ?1",
                        )
                        .context("prepare the target folder's existing-row read")
                        .map_err(StorageQuotaError::Db)?;
                    let mapped = stmt
                        .query_map(rusqlite::params![id], |r| {
                            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
                        })
                        .context("read the target folder's existing rows")
                        .map_err(StorageQuotaError::Db)?;
                    let mut out = Vec::new();
                    for row in mapped {
                        out.push(
                            row.context("read one existing target row")
                                .map_err(StorageQuotaError::Db)?,
                        );
                    }
                    out
                };

                let mut already: std::collections::HashSet<[u8; 32]> =
                    std::collections::HashSet::new();
                let mut foreign: i64 = 0;
                for (path_hash, manifest_hash) in found {
                    let path_hash: Option<[u8; 32]> = path_hash.try_into().ok();
                    let manifest_hash: Option<[u8; 32]> =
                        manifest_hash.and_then(|m| m.try_into().ok());
                    match (path_hash, manifest_hash) {
                        (Some(p), Some(m)) if intended_by_path.get(&p) == Some(&m) => {
                            already.insert(p);
                        }
                        // A row at a path we do not carry, a row at a path we do
                        // carry but pointing at other content, or a tombstone
                        // (`manifest_hash IS NULL`): all of them are somebody's
                        // live folder, and none of them is ours to write over.
                        _ => foreign += 1,
                    }
                }
                if foreign > 0 {
                    // The transaction drops unwritten — the refusal changes
                    // nothing, which is the empty-target rule's own promise.
                    return Ok(FolderMaterializeOutcome::NotEmpty { records: foreign });
                }
                (id, already)
            }
        };

        let resumed = already.len() as u64;
        let mut rehomed: u64 = 0;
        for FolderRehomeSigned { row, signature } in page {
            if already.contains(&row.path_hash) {
                continue;
            }
            Self::record_sync_change_in_conn(
                &tx,
                // Recorder and owner are the same actor: the verb is served on
                // the owner's own authenticated connection, and a re-homed row
                // is theirs however the bytes reached the box.
                &owner,
                &owner,
                None,
                &row.path_hash,
                Some(&row.manifest_hash),
                row.size_bytes,
                "create",
                folder_id,
                &device_id,
                // No plaintext path: a live folder set is not one of the three
                // resting-plaintext classes, and the sealed name below is the
                // only label this row will ever carry.
                None,
                // No content key version, exactly as the custodian's own store
                // records (`custodian_store.rs`): these rows are owner-sealed,
                // and custody carries no generation stamp to re-home.
                None,
                None,
                Some(&row.path_sealed),
                None,
                None,
                max_storage_bytes,
                // The owner's signature the arm verified over this row's
                // re-home statement — never an unsigned row.
                Some(RowSignature {
                    signature: signature.signature,
                    signer_key: signature.signer_key,
                }),
            )?;
            rehomed += 1;
        }
        let remaining = (intended_by_path.len() as u64).saturating_sub(resumed + rehomed);

        tx.commit()
            .context("commit the folder materialize transaction")
            .map_err(StorageQuotaError::Db)?;
        Ok(FolderMaterializeOutcome::Done {
            folder_id,
            rehomed,
            resumed,
            remaining,
        })
    }

    pub async fn list_backup_custody(
        &self,
        owner: &[u8; 32],
        after: Option<(i64, i64)>,
        limit: i64,
    ) -> Result<Vec<BackupCustodyRow>> {
        let owner = *owner;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(f.name, ''), c.path, c.path_hash, c.manifest_hash, c.size_bytes, c.updated_at,
                    c.rowid, c.path_sealed
             FROM backup_custody c
             INNER JOIN folders f ON f.id = c.folder_id
             WHERE f.actor_id = ?1 AND c.manifest_hash IS NOT NULL
               AND (?2 IS NULL
                    OR c.updated_at < ?2
                    OR (c.updated_at = ?2 AND c.rowid < ?3))
             ORDER BY c.updated_at DESC, c.rowid DESC
             LIMIT ?4",
        )?;
        let (after_key, after_rowid) = match after {
            Some((k, r)) => (Some(k), Some(r)),
            None => (None, None),
        };
        let limit = if limit > 0 { limit } else { i64::MAX };
        let rows = stmt.query_map(
            rusqlite::params![owner.as_slice(), after_key, after_rowid, limit],
            |r| {
                Ok(BackupCustodyRow {
                    folder_name: r.get(0)?,
                    path: r.get(1)?,
                    path_hash: r.get(2)?,
                    manifest_hash: r.get(3)?,
                    size_bytes: r.get(4)?,
                    updated_at: r.get(5)?,
                    rowid: r.get(6)?,
                    path_sealed: r.get(7)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// List an owner's retained generations across all of their custody sets —
    /// the read behind `fauna.backup.generation.list`. Newest supersede first.
    /// Scoped to `owner` as the *set owner*, so a caller only ever sees custody
    /// held for itself.
    /// `after` resumes past a previous page's last row — `(superseded_at,
    /// rowid)`, the deterministic serve order's own key (see
    /// [`Self::list_backup_custody`] for the paging contract). `limit`
    /// non-positive means "no row bound before the frame byte budget".
    pub async fn list_backup_custody_generations(
        &self,
        owner: &[u8; 32],
        after: Option<(i64, i64)>,
        limit: i64,
    ) -> Result<Vec<BackupCustodyGenerationRow>> {
        let owner = *owner;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(f.name, ''), g.path, g.path_hash, g.manifest_hash, g.size_bytes, g.superseded_at,
                    g.rowid
             FROM backup_custody_generations g
             INNER JOIN folders f ON f.id = g.folder_id
             WHERE f.actor_id = ?1
               AND (?2 IS NULL
                    OR g.superseded_at < ?2
                    OR (g.superseded_at = ?2 AND g.rowid < ?3))
             ORDER BY g.superseded_at DESC, g.rowid DESC
             LIMIT ?4",
        )?;
        let (after_key, after_rowid) = match after {
            Some((k, r)) => (Some(k), Some(r)),
            None => (None, None),
        };
        let limit = if limit > 0 { limit } else { i64::MAX };
        let rows = stmt.query_map(
            rusqlite::params![owner.as_slice(), after_key, after_rowid, limit],
            |r| {
                Ok(BackupCustodyGenerationRow {
                    folder_name: r.get(0)?,
                    path: r.get(1)?,
                    path_hash: r.get(2)?,
                    manifest_hash: r.get(3)?,
                    size_bytes: r.get(4)?,
                    superseded_at: r.get(5)?,
                    rowid: r.get(6)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Promote a retained generation back to live for its path — the write
    /// behind `fauna.backup.generation.restore`, and the affordance that makes
    /// the grace window *useful* rather than merely protective: after revoking a
    /// rogue source's writer grant, the owner's client walks its generations and
    /// restores the good ones.
    ///
    /// The swap is symmetric and quota-neutral in the common case: the current
    /// live generation (if any) is retained by the same machinery that a
    /// supersede uses, and the restored generation leaves the retained set
    /// because it is live again. [`GenerationRestore::NotFound`] if no such
    /// generation exists for this owner (an unknown manifest, or one already
    /// reclaimed past T).
    ///
    /// A generation superseded before the owner's writer seat was taken is
    /// refused ([`GenerationRestore::BeforeSeat`]): another writer took the
    /// seat over custody with no live path, so that generation belongs to the
    /// previous writer's numbering and restoring it would put two numberings
    /// into one set. An owner with no seat row has no clock and no refusal.
    ///
    /// `owner` is the authenticated caller and is matched against the folder's
    /// `actor_id`, so a caller can only restore into its own custody.
    pub async fn restore_backup_custody_generation(
        &self,
        owner: &[u8; 32],
        folder_name: &str,
        path_hash: &[u8; 32],
        manifest_hash: &[u8; 32],
    ) -> Result<GenerationRestore> {
        let owner = *owner;
        let folder_name = folder_name.to_string();
        let path_hash = *path_hash;
        let manifest_hash = *manifest_hash;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        let Some(folder_id): Option<i64> = conn
            .query_row(
                "SELECT id FROM folders WHERE name = ?1 AND actor_id = ?2",
                rusqlite::params![folder_name, owner.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("resolve folder for generation restore")?
        else {
            return Ok(GenerationRestore::NotFound);
        };

        let Some((gen_size, gen_path, gen_uploader, gen_superseded_at)): Option<(
            i64,
            Option<String>,
            Vec<u8>,
            i64,
        )> = conn
            .query_row(
                "SELECT size_bytes, path, uploader_actor, superseded_at
                 FROM backup_custody_generations
                 WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash = ?3",
                rusqlite::params![folder_id, path_hash.as_slice(), manifest_hash.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .context("resolve retained generation to restore")?
        else {
            return Ok(GenerationRestore::NotFound);
        };

        let seated_at: Option<i64> = conn
            .query_row(
                "SELECT seated_at FROM backup_writer_grants WHERE owner_actor_id = ?1",
                rusqlite::params![owner.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("read the writer seat's clock for generation restore")?;
        if seated_at.is_some_and(|seated_at| gen_superseded_at < seated_at) {
            return Ok(GenerationRestore::BeforeSeat);
        }

        // Retain whatever is live right now (the same move a supersede makes),
        // so restoring never destroys the generation it replaces.
        let live: Option<(Vec<u8>, i64, Vec<u8>, Option<String>)> = conn
            .query_row(
                "SELECT uploader_actor, size_bytes, manifest_hash, path FROM backup_custody
                 WHERE folder_id = ?1 AND path_hash = ?2 AND manifest_hash IS NOT NULL",
                rusqlite::params![folder_id, path_hash.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .context("query live custody row for generation restore")?;
        let live_size = live.as_ref().map(|(_, s, _, _)| *s).unwrap_or(0);
        let live_retained = if let Some((up, size, manifest, path)) = &live {
            let up: [u8; 32] = blob_to_array(up.as_slice(), "uploader_actor")?;
            retain_custody_generation_in_conn(
                &conn,
                folder_id,
                &up,
                &path_hash,
                manifest,
                *size,
                path.as_deref(),
                now,
            )?
        } else {
            false
        };

        conn.execute(
            "INSERT INTO backup_custody
                 (uploader_actor, folder_id, path_hash, manifest_hash, size_bytes, updated_at, path, thumbnail_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT(folder_id, path_hash) DO UPDATE SET
                 uploader_actor = excluded.uploader_actor,
                 manifest_hash  = excluded.manifest_hash,
                 size_bytes     = excluded.size_bytes,
                 updated_at     = excluded.updated_at,
                 path           = COALESCE(excluded.path, backup_custody.path)",
            rusqlite::params![
                gen_uploader.as_slice(),
                folder_id,
                path_hash.as_slice(),
                manifest_hash.as_slice(),
                gen_size,
                now,
                gen_path,
            ],
        )
        .context("promote retained generation to live custody")?;

        // The restored generation is live now, so it leaves the retained set —
        // the same invariant an ordinary re-record maintains.
        unretain_custody_generation_in_conn(
            &conn,
            folder_id,
            &path_hash,
            manifest_hash.as_slice(),
        )?;

        // Quota: the restored generation moved retained → live (no net change),
        // and the displaced live generation moved live → retained (also none).
        // Only a set that does not retain (defensive — generations exist solely
        // for sets that do) would actually free the displaced bytes.
        let delta = if live_retained { 0 } else { -live_size };
        if delta != 0 {
            conn.execute(
                "UPDATE users SET storage_bytes_used = MAX(0, CAST(storage_bytes_used AS INTEGER) + ?1)
                 WHERE actor_id = ?2",
                rusqlite::params![delta, owner.as_slice()],
            )
            .context("adjust storage_bytes_used on generation restore")?;
        }
        Ok(GenerationRestore::Restored)
    }

    /// Collect all live (non-tombstoned) custody manifest hashes — the
    /// destination's authoritative backup live set for the GC reference walk.
    pub async fn backup_custody_manifest_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT manifest_hash FROM backup_custody WHERE manifest_hash IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Drop all custody for a folder (destination removal → the owner's
    /// reserved backup set is deleted, so every chunk it protected reclaims).
    ///
    /// Retained generations go too. The grace window bounds a custody **writer's**
    /// supersede power — the source nest's — not the **owner's** own authority
    /// over their backup: an owner removing their destination is authorized
    /// deletion, and leaving generations behind would pin chunks (and charge
    /// quota) for T with no row left to restore them from.
    pub async fn drop_backup_custody_for_folder(&self, folder_id: i64) -> Result<u64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM backup_custody_generations WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .context("drop retained custody generations for folder")?;
        let n = conn
            .execute(
                "DELETE FROM backup_custody WHERE folder_id = ?1",
                rusqlite::params![folder_id],
            )
            .context("drop backup custody for folder")?;
        Ok(n as u64)
    }

    /// List all snapshot IDs across all folders.
    pub async fn list_all_snapshot_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT id FROM snapshots")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Count the number of sync devices registered for an actor, excluding
    /// the WebDAV pseudo-device — the same set the tier cap check counts
    /// (`register_sync_device_capped`), so a displayed `used` count never
    /// disagrees with the `max` it is shown against.
    pub async fn count_sync_devices(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor = actor_id.to_vec();
        let webdav = fauna_core::label_custody::webdav_pseudo_device_id(actor_id).to_vec();
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_devices WHERE actor_id = ?1 AND device_id != ?2",
                rusqlite::params![actor, webdav],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count)
    }

    /// Delete all sync devices for an actor.
    pub async fn delete_sync_devices_for_actor(&self, actor_id: &[u8; 32]) -> Result<u64> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM sync_devices WHERE actor_id = ?1",
                rusqlite::params![actor],
            )
            .context("delete sync devices")?;
        Ok(deleted as u64)
    }

    /// Get global repository statistics.
    pub async fn global_stats(&self) -> Result<(i64, i64, i64, i64, i64, i64)> {
        let conn = self.conn.lock().await;
        let total_blobs: i64 = conn
            .query_row("SELECT COUNT(*) FROM blob_metadata", [], |r| r.get(0))
            .unwrap_or(0);
        let total_size: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM blob_metadata",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let chunk_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blob_metadata WHERE content_type = 'chunk'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let manifest_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blob_metadata WHERE content_type = 'manifest'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let total_snapshots: i64 = conn
            .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get(0))
            .unwrap_or(0);
        let total_folders: i64 = conn
            .query_row("SELECT COUNT(*) FROM folders", [], |r| r.get(0))
            .unwrap_or(0);
        Ok((
            total_blobs,
            total_size,
            chunk_count,
            manifest_count,
            total_snapshots,
            total_folders,
        ))
    }

    /// Get raw size (sum of snapshot_files.size_bytes across all snapshots) for a folder.
    pub async fn folder_raw_size(&self, folder_id: i64) -> Result<i64> {
        let conn = self.conn.lock().await;
        let raw: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(sf.size_bytes), 0) FROM snapshot_files sf \
             JOIN snapshots s ON s.id = sf.snapshot_id WHERE s.folder_id = ?1",
                rusqlite::params![folder_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(raw)
    }

    // ==================== Snapshot Soft-Delete Lifecycle ====================

    /// Soft-delete a snapshot: mark it deleted with a 30-day purge window.
    /// Sets soft_deleted=1, purge_after=now+30days, deletion_pending=0.
    pub async fn soft_delete_snapshot(&self, snapshot_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        let purge_after = now_epoch_secs() + 30 * 24 * 3600;
        conn.execute(
            "UPDATE snapshots SET soft_deleted = 1, purge_after = ?1, deletion_pending = 0 WHERE id = ?2",
            rusqlite::params![purge_after, snapshot_id],
        ).context("soft_delete_snapshot")?;
        Ok(())
    }

    /// Undelete a soft-deleted snapshot: clear all deletion state.
    /// Sets soft_deleted=0, purge_after=NULL, deletion_pending=0.
    pub async fn undelete_snapshot(&self, snapshot_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE snapshots SET soft_deleted = 0, purge_after = NULL, deletion_pending = 0 WHERE id = ?1",
            rusqlite::params![snapshot_id],
        ).context("undelete_snapshot")?;
        Ok(())
    }

    /// Stamp (or re-stamp) a snapshot's `tags_sealed` display copy in place —
    /// the `fauna.filesync.snapshot.stamp_labels` write (S8 D3), and the
    /// FIRST-ever UPDATE of this column: its only other writer is
    /// `create_snapshot_v2`'s INSERT, and snapshots being immutable is exactly
    /// why the backfill needed a dedicated stamp. The blob is opaque here —
    /// this nest holds no key that opens it, and retention matching stays
    /// hash-to-hash on `tag_hashes`, which this deliberately never touches.
    ///
    /// **Compare-and-swap**: `expected_current`
    /// is the exact column value the caller decided against — `None` meaning
    /// "there was no seal". The `IS` comparison (not `=`) is what makes the
    /// `NULL` arm work in SQLite. `Ok(false)` = the row moved under the
    /// decision; the handler turns that into a retryable refusal rather than
    /// silently clobbering whatever landed in between. Overwrite is still
    /// *possible* — the migration-window axis re-seal needs it — but it is no
    /// longer *unconditional*, and the per-axis predicate that decides lives
    /// at the handler, which is the only layer that parses the envelope.
    pub async fn set_snapshot_tags_sealed(
        &self,
        snapshot_id: i64,
        tags_sealed: &[u8],
        expected_current: Option<&[u8]>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE snapshots SET tags_sealed = ?1 WHERE id = ?2 AND tags_sealed IS ?3",
                rusqlite::params![tags_sealed, snapshot_id, expected_current],
            )
            .context("set_snapshot_tags_sealed")?;
        Ok(affected == 1)
    }

    /// Count active (not soft-deleted and not deletion_pending) snapshots for a folder.
    pub async fn count_active_snapshots(&self, folder_id: i64) -> Result<i64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM snapshots WHERE folder_id = ?1 AND soft_deleted = 0 AND deletion_pending = 0",
            rusqlite::params![folder_id],
            |row| row.get(0),
        ).context("count_active_snapshots")?;
        Ok(count)
    }

    /// Check whether deleting one snapshot from this folder is allowed — the
    /// § 7 Layer 1 hard floor, through the one shared statement of it
    /// ([`fauna_protocol::filesync::snapshot_delete_allowed`]) that the pruner
    /// and every app also read. This used to hard-code `count > 3` beside the
    /// pruner's named constant: two spellings of one rule.
    pub async fn check_snapshot_delete_allowed(&self, folder_id: i64) -> Result<bool> {
        let count = self.count_active_snapshots(folder_id).await?;
        Ok(fauna_protocol::filesync::snapshot_delete_allowed(
            usize::try_from(count).unwrap_or(0),
        ))
    }

    /// Mark a snapshot as deletion_pending=1 (a pending action is in flight for it).
    pub async fn mark_snapshot_deletion_pending(&self, snapshot_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE snapshots SET deletion_pending = 1 WHERE id = ?1",
            rusqlite::params![snapshot_id],
        )
        .context("mark_snapshot_deletion_pending")?;
        Ok(())
    }

    /// List soft-deleted snapshot IDs whose purge_after has passed.
    pub async fn list_expired_soft_deleted(&self) -> Result<Vec<i64>> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM snapshots WHERE soft_deleted = 1 AND purge_after IS NOT NULL AND purge_after <= ?1",
            )
            .context("prepare list_expired_soft_deleted")?;
        let rows = stmt
            .query_map(rusqlite::params![now], |row| row.get(0))
            .context("query expired soft-deleted snapshots")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read expired snapshot id")?);
        }
        Ok(out)
    }

    // ==================== Test Helpers ====================

    /// Insert a bare snapshot row with an explicit created_at.
    /// Intended for tests that need multiple snapshots in the same second
    /// (avoiding the UNIQUE(folder_id, created_at) constraint).
    pub async fn insert_snapshot_at(&self, folder_id: i64, created_at: i64) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO snapshots (folder_id, created_at, file_count, total_bytes) VALUES (?1, ?2, 0, 0)",
            rusqlite::params![folder_id, created_at],
        ).context("insert_snapshot_at")?;
        Ok(conn.last_insert_rowid())
    }

    /// Point a snapshot's `parent_id` at another snapshot (test-only).
    /// Lets a test build a parent→child chain so the `snapshots.parent_id`
    /// self-FK is exercised (e.g. by folder delete).
    pub async fn set_snapshot_parent(&self, snapshot_id: i64, parent_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE snapshots SET parent_id = ?1 WHERE id = ?2",
            rusqlite::params![parent_id, snapshot_id],
        )
        .context("set_snapshot_parent")?;
        Ok(())
    }

    /// Override purge_after on a snapshot. Used in tests to simulate an expired purge window.
    pub async fn set_snapshot_purge_after(&self, snapshot_id: i64, purge_after: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE snapshots SET purge_after = ?1 WHERE id = ?2",
            rusqlite::params![purge_after, snapshot_id],
        )
        .context("set_snapshot_purge_after")?;
        Ok(())
    }

    /// Test hook — [`Self::set_snapshot_purge_after`]'s version-plane twin:
    /// force a soft-pruned version's `purge_after` so a test can reach the
    /// purge step without waiting out the 30-day window.
    pub async fn set_version_purge_after(&self, seq: i64, purge_after: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sync_changes SET purge_after = ?1 WHERE seq = ?2",
            rusqlite::params![purge_after, seq],
        )
        .context("set_version_purge_after")?;
        Ok(())
    }

    // ==================== Device & Member Management ====================

    /// List all devices for an actor (accepts variable-length actor_id).
    pub async fn list_devices_for_actor(&self, actor_id: &[u8]) -> Result<Vec<SyncDeviceRow>> {
        // (The two participation columns ride every roster read — see
        // `set_device_p2p_participation` for what writes them.)
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, device_id, label, label_sealed, registered_at, last_seen,
                    capabilities, guardian_marked, auth_device_key,
                    p2p_participation, p2p_off_requested
             FROM sync_devices WHERE actor_id = ?1 ORDER BY registered_at",
            )
            .context("prepare list_devices_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id], |row| {
                Ok(SyncDeviceRow {
                    actor_id: row.get(0)?,
                    device_id: row.get(1)?,
                    label: row.get(2)?,
                    label_sealed: row.get(3)?,
                    registered_at: row.get(4)?,
                    last_seen: row.get(5)?,
                    capabilities: row.get(6)?,
                    guardian_marked: row.get::<_, i64>(7)? != 0,
                    principal: row.get(8)?,
                    p2p_participation: row
                        .get::<_, Option<String>>(9)?
                        .map(|s| s == P2P_PARTICIPATION_ON),
                    p2p_off_requested: row.get::<_, i64>(10)? != 0,
                })
            })
            .context("query list_devices_for_actor")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read device row")?);
        }
        Ok(results)
    }

    /// The places a device holds in the caller's own folders, as
    /// `(the set's label, flags)`.
    pub async fn get_device_folder_places(
        &self,
        device_id: &[u8],
        actor_id: &[u8],
    ) -> Result<Vec<(SetLabel, PlaceFlags)>> {
        let device_id = device_id.to_vec();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        // Scoped to the caller's own folders: a device id is unique only per
        // actor, so an unscoped read lists another account's folders under a
        // borrowed id (`devices.md` § Removing a Device, step 4).
        let mut stmt = conn
            .prepare(
                "SELECT fs.name, fs.name_hash, fs.name_sealed,
                    fsm.originates, fsm.accepts, fsm.applies_deletes
             FROM folder_members fsm
             JOIN folders fs ON fsm.folder_id = fs.id
             WHERE fsm.device_id = ?1 AND fs.actor_id = ?2",
            )
            .context("prepare get_device_folder_places")?;
        let rows = stmt
            .query_map(rusqlite::params![device_id, actor_id], |row| {
                Ok((SetLabel::from_row(row, 0)?, place_flags_from_row(row, 3)?))
            })
            .context("query get_device_folder_places")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read place row")?);
        }
        Ok(results)
    }

    /// Get a single device scoped to an actor (ownership check).
    pub async fn get_device_for_user(
        &self,
        device_id: &[u8],
        actor_id: &[u8],
    ) -> Result<Option<SyncDeviceRow>> {
        let device_id = device_id.to_vec();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT actor_id, device_id, label, label_sealed, registered_at, last_seen,
                capabilities, guardian_marked, auth_device_key,
                p2p_participation, p2p_off_requested
             FROM sync_devices WHERE device_id = ?1 AND actor_id = ?2",
            rusqlite::params![device_id, actor_id],
            |row| {
                Ok(SyncDeviceRow {
                    actor_id: row.get(0)?,
                    device_id: row.get(1)?,
                    label: row.get(2)?,
                    label_sealed: row.get(3)?,
                    registered_at: row.get(4)?,
                    last_seen: row.get(5)?,
                    capabilities: row.get(6)?,
                    guardian_marked: row.get::<_, i64>(7)? != 0,
                    principal: row.get(8)?,
                    p2p_participation: row
                        .get::<_, Option<String>>(9)?
                        .map(|s| s == P2P_PARTICIPATION_ON),
                    p2p_off_requested: row.get::<_, i64>(10)? != 0,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get device for user"),
        }
    }

    /// The device's OWN report of its peer participation (`p2p.md` § Per-device
    /// participation, the self arm): set the reported column, and clear a
    /// pending brake only when the report is `off` — an `on` report leaves a
    /// brake that raced it in place for the device's next pass to fold, so
    /// no ordering of the two calls ends with a brake silently dropped.
    /// Returns whether a row of `actor_id` matched.
    pub async fn set_device_p2p_participation(
        &self,
        actor_id: &[u8],
        device_id: &[u8],
        participating: bool,
    ) -> Result<bool> {
        let a = actor_id.to_vec();
        let d = device_id.to_vec();
        let reported = if participating {
            P2P_PARTICIPATION_ON
        } else {
            P2P_PARTICIPATION_OFF
        };
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE sync_devices
                    SET p2p_participation = ?3,
                        p2p_off_requested = CASE WHEN ?4 THEN p2p_off_requested ELSE 0 END
                  WHERE actor_id = ?1 AND device_id = ?2",
                rusqlite::params![a, d, reported, participating],
            )
            .context("set device p2p participation")?;
        Ok(n > 0)
    }

    /// The owner arm: another of the account's devices asks this one to turn
    /// its peer listeners off. Raises the pending brake and nothing else —
    /// the reported column stays the device's own word until it folds and
    /// reports. Returns whether a row of `actor_id` matched.
    pub async fn request_device_p2p_off(&self, actor_id: &[u8], device_id: &[u8]) -> Result<bool> {
        let a = actor_id.to_vec();
        let d = device_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE sync_devices SET p2p_off_requested = 1
                  WHERE actor_id = ?1 AND device_id = ?2",
                rusqlite::params![a, d],
            )
            .context("request device p2p off")?;
        Ok(n > 0)
    }

    /// Delete a device and its folder memberships, and — when the row carried
    /// a renewal grant — tombstone that grant's device key in
    /// `revoked_device_grants`, atomically with the row delete
    /// (`nest/common.md` § Client-state recoverability: a crash cannot leave
    /// the row gone with the grant replayable). The tombstone is what stops a
    /// post-delete `fauna.sync.device_grant.register` replay of the same
    /// root-signed grant from restoring renewal (`sync-agent.md` § Credential
    /// model — device revocation).
    ///
    /// Returns (device_deleted, folders_removed_count, revoked_auth_device_key);
    /// the caller revokes the sessions that key minted from the in-memory
    /// token store (which cannot outlive a crash, so it needs no transaction).
    pub async fn delete_device(
        &self,
        device_id: &[u8],
        actor_id: &[u8],
    ) -> Result<(bool, i64, Option<[u8; 32]>)> {
        let device_id = device_id.to_vec();
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("delete device tx")?;

        // Capture the renewal key before the row dies — it is both the
        // tombstone value and the token-revocation key.
        let auth_device_key: Option<[u8; 32]> = tx
            .query_row(
                "SELECT auth_device_key FROM sync_devices
                 WHERE device_id = ?1 AND actor_id = ?2",
                rusqlite::params![device_id, actor_id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()
            .context("read device auth key")?
            .flatten()
            .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok());

        // Only this actor's own folders' seats: another account may have
        // registered the same device id (`devices.md` § Removing a Device,
        // step 4).
        tx.execute(
            "DELETE FROM folder_members WHERE device_id = ?1
             AND folder_id IN (SELECT id FROM folders WHERE actor_id = ?2)",
            rusqlite::params![device_id, actor_id],
        )
        .context("delete folder members for device")?;
        let members_removed = tx.changes() as i64;

        tx.execute(
            "DELETE FROM sync_devices WHERE device_id = ?1 AND actor_id = ?2",
            rusqlite::params![device_id, actor_id],
        )
        .context("delete sync device")?;
        let device_deleted = tx.changes() > 0;

        let revoked_key = if device_deleted {
            if let Some(key) = auth_device_key {
                tombstone_revoked_grant(&tx, &actor_id, key.as_slice(), now_epoch_secs())?;
            }
            auth_device_key
        } else {
            None
        };
        tx.commit().context("commit device delete")?;

        Ok((device_deleted, members_removed, revoked_key))
    }

    /// Retire ONE renewal grant, named by its device public key, and tombstone
    /// that key — [`Self::delete_device`]'s revocation half factored out from
    /// its row deletion (`sync-agent.md` § Credential model → the RULED
    /// 2026-08-15 block, decision 2: *the grant named by its device public
    /// key, never the whole device row*).
    ///
    /// The two differ in exactly one way, and it is the point: the device row,
    /// its label and its `folder_members` all survive. Only the
    /// `auth_device_key` / `auth_grant` columns are cleared, so the machine
    /// keeps syncing under a device the user still recognises while the
    /// credential that could mint bearers for it is gone.
    ///
    /// The tombstone is written **unconditionally**, not only when a row was
    /// cleared. An authorized caller asking that this key never mint again is
    /// answered the same way whether the grant is still stored, was already
    /// cleared by a previous attempt, or died with a device the user deleted
    /// first — which is what makes the caller's retry loop terminate on a
    /// not-found rather than spin. (`delete_device` tombstones only on a real
    /// delete because *there* the key is discovered from the row; here the
    /// caller names it and has proven it may.)
    ///
    /// Returns what was cleared; the caller revokes the sessions that key
    /// minted from the in-memory token store, as the device delete does.
    pub async fn revoke_device_grant(
        &self,
        actor_id: &[u8; 32],
        auth_device_key: &[u8; 32],
    ) -> Result<GrantRevokeOutcome> {
        let actor_id = *actor_id;
        let auth_device_key = *auth_device_key;
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("revoke device grant tx")?;

        tx.execute(
            "UPDATE sync_devices SET auth_device_key = NULL, auth_grant = NULL
             WHERE actor_id = ?1 AND auth_device_key = ?2",
            rusqlite::params![actor_id.as_slice(), auth_device_key.as_slice()],
        )
        .context("clear sync device grant")?;
        let cleared = tx.changes() > 0;

        tombstone_revoked_grant(
            &tx,
            actor_id.as_slice(),
            auth_device_key.as_slice(),
            now_epoch_secs(),
        )?;
        tx.commit().context("commit device grant revoke")?;

        Ok(GrantRevokeOutcome { cleared })
    }

    /// Set a device place's flags — the at-rest half of
    /// `fauna.folders.places.set`, the one add/edit door.
    ///
    /// Takes an already-resolved `folder_id` because the handler has resolved
    /// (and thereby ownership-checked) the folder already, hash-first or by
    /// name; the device's ownership is checked here, exactly as the `members`
    /// door does.
    ///
    /// **Every flag point is writable**, and rests exactly as sent.
    pub async fn set_folder_place_flags(
        &self,
        folder_id: i64,
        actor_id: &[u8],
        device_id: &[u8],
        flags: &PlaceFlags,
    ) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let device_id = device_id.to_vec();
        let conn = self.conn.lock().await;

        let _: i64 = conn
            .query_row(
                "SELECT 1 FROM sync_devices WHERE device_id = ?1 AND actor_id = ?2",
                rusqlite::params![device_id, actor_id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    anyhow::anyhow!("device not found or not owned by actor")
                }
                other => anyhow::anyhow!(other),
            })?;

        upsert_folder_place(&conn, folder_id, &device_id, flags)
    }

    /// Remove one device place from a set the caller has already resolved
    /// owner-scoped (`members.remove` resolves hash-first, so the ownership
    /// check is the lookup, not this delete). Returns true if a place was
    /// removed.
    pub async fn remove_folder_member(&self, fs_id: i64, device_id: &[u8]) -> Result<bool> {
        let device_id = device_id.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM folder_members WHERE folder_id = ?1 AND device_id = ?2",
            rusqlite::params![fs_id, device_id],
        )
        .context("remove folder member")?;
        Ok(conn.changes() > 0)
    }

    /// List a folder's device places with their device labels — the read
    /// behind `fauna.folders.members.list`.
    ///
    /// One row per seat: the label joins the READER's own registration of the
    /// device only (`sync_devices` is keyed `(actor_id, device_id)`, so an
    /// unscoped join duplicated the seat for every account that registered the
    /// same id — `path-sealing.md` § device label, gap (a)).
    pub async fn list_folder_members_with_labels(
        &self,
        folder_id: i64,
        reader_actor_id: &[u8],
    ) -> Result<Vec<FolderPlaceWithLabel>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT fsm.device_id, COALESCE(sd.label, ''),
                        fsm.originates, fsm.accepts, fsm.applies_deletes
             FROM folder_members fsm
             LEFT JOIN sync_devices sd ON fsm.device_id = sd.device_id AND sd.actor_id = ?2
             WHERE fsm.folder_id = ?1",
            )
            .context("prepare list_folder_members_with_labels")?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id, reader_actor_id], |row| {
                Ok(FolderPlaceWithLabel {
                    device_id: row.get(0)?,
                    label: row.get(1)?,
                    flags: place_flags_from_row(row, 2)?,
                })
            })
            .context("query list_folder_members_with_labels")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read member with label row")?);
        }
        Ok(results)
    }

    /// Bind a folder to an MLS group (`Some`) or clear the binding (`None`) —
    /// the cross-user *shared* set ⇔ owner-only toggle (shared-folders Slice 1).
    /// Keyed by the row id the handler resolved ([`Self::update_folder_by_id`]),
    /// so the owner scope is the resolver's. A bound set's chunks seal under
    /// `chunk_crypto` keyed by the group's `export_chunk_key`; an unbound set
    /// stays on `BackupKey`. Returns whether a row changed (false = no such row).
    pub async fn set_folder_mls_group_by_id(
        &self,
        folder_id: i64,
        // The **raw** MLS group id, variable length (openMLS group ids are not
        // fixed-32) — `None` unbinds. Stored verbatim; the chunk_crypto per-chunk
        // root source + the shared-set flag. The row's `actor_id` stays the owner
        // (a folder transitions from owner-owned, unlike conv's born-shared
        // reserved sets); the membership gate derives the ChannelId via
        // `ChannelId::from_group_id(mls_group_id)`.
        mls_group_id: Option<&[u8]>,
    ) -> Result<bool> {
        let group: Option<Vec<u8>> = mls_group_id.map(|g| g.to_vec());
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE folders SET mls_group_id = ?1 WHERE id = ?2",
                rusqlite::params![group, folder_id],
            )
            .context("set folder mls group")?;
        Ok(changed > 0)
    }

    /// [`Self::set_folder_mls_group_by_id`] on the set `actor_id` owns under
    /// `name` — the by-name convenience [`Self::update_folder_for_user`]
    /// describes. `actor_id` is the row's owner column, which a channel-scoped
    /// `__conv` set fills with its channel id. An empty name names no set.
    pub async fn set_folder_mls_group(
        &self,
        name: &str,
        actor_id: &[u8],
        mls_group_id: Option<&[u8]>,
    ) -> Result<bool> {
        if name.is_empty() {
            return Ok(false);
        }
        let id: Option<i64> = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT id FROM folders WHERE name = ?1 AND actor_id = ?2",
                rusqlite::params![name, actor_id],
                |r| r.get(0),
            )
            .optional()
            .context("look up folder for mls group")?
        };
        match id {
            Some(id) => self.set_folder_mls_group_by_id(id, mls_group_id).await,
            None => Ok(false),
        }
    }

    /// Upsert the opaque content-key envelope for a group-bound shared folder,
    /// keyed by the 32-byte derived ChannelId (M2, shared-folders Slice 3 —
    /// `mls-group-key-material.md` § M2 content-key mechanism). The envelope is
    /// **re-published on every membership change** → `INSERT OR REPLACE` (one row
    /// per group, always the latest generation bundle sealed under the current
    /// epoch). The nest holds `sealed` as **opaque ciphertext** — it has no group
    /// secret and never decrypts it; the seal/open live in `fauna-mls`.
    /// `current_version` is the owner-stamped **version floor** for non-owner
    /// records (KMH § M2 version floor, multi-writer Phase 1) — **monotonic**:
    /// the stored floor only ever rises (`MAX(stored, new)`), which is why
    /// this is an `ON CONFLICT DO UPDATE` and not an `INSERT OR REPLACE`.
    pub async fn upsert_folder_content_key(
        &self,
        channel_id: &[u8; 32],
        epoch: i64,
        sealed: &[u8],
        current_version: i64,
    ) -> Result<()> {
        let channel_id = channel_id.to_vec();
        let sealed = sealed.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO folder_content_keys (channel_id, epoch, sealed, updated_at, current_version)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel_id) DO UPDATE SET
                 epoch = excluded.epoch,
                 sealed = excluded.sealed,
                 updated_at = excluded.updated_at,
                 current_version = MAX(folder_content_keys.current_version,
                                       excluded.current_version)",
            rusqlite::params![channel_id, epoch, sealed, now, current_version],
        )
        .context("upsert folder content key")?;
        Ok(())
    }

    /// The owner-stamped version floor for a group's content keys, or `None`
    /// when no envelope has been published yet.
    /// Read by `changes.record`'s non-owner staleness check (`stale_content_key`).
    pub async fn get_folder_content_key_floor(&self, channel_id: &[u8; 32]) -> Result<Option<i64>> {
        let channel_id = channel_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT current_version FROM folder_content_keys WHERE channel_id = ?1",
                rusqlite::params![channel_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .context("get folder content key floor")?;
        Ok(result)
    }

    /// The content-key floors of several groups in one read — the
    /// `fauna.folders.list` projection's, which is polled on a tick by every
    /// seat and must not scale per row. Groups with no floor are absent.
    pub async fn content_key_floors_for(
        &self,
        channel_ids: &[[u8; 32]],
    ) -> Result<std::collections::HashMap<[u8; 32], i64>> {
        let mut out = std::collections::HashMap::new();
        if channel_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT current_version FROM folder_content_keys WHERE channel_id = ?1")?;
        for channel_id in channel_ids {
            let floor = stmt
                .query_row(rusqlite::params![channel_id.as_slice()], |row| {
                    row.get::<_, i64>(0)
                })
                .optional()
                .context("read folder content key floors")?;
            if let Some(floor) = floor {
                out.insert(*channel_id, floor);
            }
        }
        Ok(out)
    }

    /// Fetch the opaque content-key envelope for a group (by its 32-byte derived
    /// ChannelId). Returns `(epoch, sealed)`, or `None` if the owner has not
    /// published one yet (a member must wait for the first publish). The caller
    /// gates read access via `folder_authz` before calling this.
    pub async fn get_folder_content_key(
        &self,
        channel_id: &[u8; 32],
    ) -> Result<Option<(i64, Vec<u8>)>> {
        let channel_id = channel_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT epoch, sealed FROM folder_content_keys WHERE channel_id = ?1",
                rusqlite::params![channel_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("get folder content key")?;
        Ok(result)
    }

    /// Mark the actor's device carrying the granted key `auth_device_key`
    /// seen now — a WS-RPC connection bound to the device registering
    /// (`routes.rs::AppState::register_upgraded_connection`). Returns the rows
    /// touched: 1 when a row of this actor carries the key, else 0. The same
    /// `(actor_id, auth_device_key)` join as [`Self::get_sync_device_grant`].
    pub async fn touch_device_last_seen_by_principal(
        &self,
        actor_id: &[u8; 32],
        auth_device_key: &[u8; 32],
    ) -> Result<usize> {
        let actor_id = *actor_id;
        let auth_device_key = *auth_device_key;
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "UPDATE sync_devices SET last_seen = ?1
             WHERE actor_id = ?2 AND auth_device_key = ?3",
            rusqlite::params![now, actor_id.as_slice(), auth_device_key.as_slice()],
        )
        .context("touch device last seen by principal")
    }

    // ==================== Sync Conflicts ====================

    /// Report a new sync conflict. Returns the row id.
    /// Record a conflict together with its candidate versions. The daemon
    /// supplies the diverging manifests (`candidates`); an empty slice is the
    /// candidate-free (mark-only) conflict (resolves by `id` alone). Conflict row +
    /// candidate rows land in one transaction.
    ///
    /// Metered (2026-09-28, `file-versions.md` § Retention (4)): a resolved report
    /// mints charged rows, so each is charged like a metered record — refusing a
    /// negative declared size ([`StorageQuotaError::NegativeSize`], on every
    /// candidate too, since a later choose-winner mints a row at a candidate's
    /// size), the owner's ceiling and a member reporter's `byte_cap` — and a
    /// refusal lands nothing.
    #[allow(clippy::too_many_arguments)]
    pub async fn report_conflict(
        &self,
        actor_id: &[u8],
        folder_id: i64,
        device_id: &[u8],
        path: &str,
        conflict_type: &str,
        details: Option<&str>,
        candidates: &[ConflictCandidateRow],
        resolution: Option<ResolvedReport>,
        sealed: super::SealedConflictLabels,
        // S9 flip: `true` only for a `web`-mode set (public paths by design)
        // — its rows keep resting plaintext; every other set rests the scrub
        // sentinel + the sealed pair, and `path` here is only the wire value
        // the hash fallback derives from.
        rest_plaintext: bool,
    ) -> Result<i64, StorageQuotaError> {
        self.report_conflict_signed(
            actor_id,
            folder_id,
            device_id,
            path,
            conflict_type,
            details,
            candidates,
            resolution,
            sealed,
            rest_plaintext,
            ReportRowSignatures::default(),
        )
        .await
    }

    /// [`Self::report_conflict`] carrying the reporter's verified writer
    /// signatures over the rows a resolved report mints
    /// (`writer-signed-change-records.md` § Writer-signed change records):
    /// the winner head row's (ruling (1)(ii)) and the retained loser's
    /// (ruling (10)(d)), each stored on its row. An unresolved report mints no
    /// row to carry either.
    #[allow(clippy::too_many_arguments)]
    pub async fn report_conflict_signed(
        &self,
        actor_id: &[u8],
        folder_id: i64,
        device_id: &[u8],
        path: &str,
        conflict_type: &str,
        details: Option<&str>,
        candidates: &[ConflictCandidateRow],
        resolution: Option<ResolvedReport>,
        sealed: super::SealedConflictLabels,
        rest_plaintext: bool,
        signatures: ReportRowSignatures<'_>,
    ) -> Result<i64, StorageQuotaError> {
        // The declarations ARE the meter for the rows this report mints (the
        // record door's reasoning, `record_sync_change_in_conn`): refuse a
        // negative one before anything is read or written. Every candidate is
        // checked, not only the two a resolved report charges now — an
        // unresolved report's candidates are what `choose-winner` later mints
        // its head row from.
        if let Some(size_bytes) = candidates
            .iter()
            .map(|c| c.size_bytes)
            .chain(resolution.as_ref().map(|r| r.winning_size_bytes))
            .find(|s| *s < 0)
        {
            return Err(StorageQuotaError::NegativeSize { size_bytes });
        }
        let reporter: [u8; 32] = actor_id
            .try_into()
            .map_err(|_| anyhow::anyhow!("reporter actor id is not 32 bytes"))?;
        let actor_id = actor_id.to_vec();
        let device_id = device_id.to_vec();
        let path = path.to_string();
        let conflict_type = conflict_type.to_string();
        let details = details.map(|s| s.to_string());
        let candidates = candidates.to_vec();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let tx = conn
            .unchecked_transaction()
            .context("begin report_conflict transaction")?;
        // `resolution` = the auto-resolve shape (ratified 2026-07-10): the
        // detecting device already resolved, so the row lands resolved (the
        // review-list surface), never blocking a chooser. None = the daemon's degraded
        // (candidate-free) unresolved report.
        let (resolved_at, resolution_kind, winner) = match &resolution {
            Some(r) => (
                Some(now),
                Some(r.resolution.clone()),
                Some(r.winning_manifest_hash.clone()),
            ),
            None => (None, None, None),
        };
        // The routing companion, written at insert so a conflict is addressable
        // by hash from the moment it exists. The client's hash wins when it sent one — it is the
        // salt its own `path_sealed` was minted under, so re-deriving here
        // could only disagree with it (and after the plaintext write flip there
        // is nothing left to derive from). A keyless or malformed-hash report sends none.
        let path_hash = sealed
            .path_hash
            .unwrap_or_else(|| fauna_core::sync::path_hash(&path));
        let rest_path: &str = if rest_plaintext { &path } else { "" };
        let rest_details = if rest_plaintext {
            details.clone()
        } else {
            None
        };
        tx.execute(
            "INSERT INTO sync_conflicts (folder_id, device_id, path, conflict_type, details, created_at, resolved_at, resolution, winning_manifest_hash, path_hash, path_sealed, details_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![folder_id, device_id, rest_path, conflict_type, rest_details, now, resolved_at, resolution_kind, winner,
                path_hash.as_slice(), sealed.path_sealed, sealed.details_sealed],
        ).context("report sync conflict")?;
        let conflict_id = tx.last_insert_rowid();
        for c in &candidates {
            tx.execute(
                "INSERT INTO sync_conflict_candidates (conflict_id, manifest_hash, device_id, size_bytes, created_at, content_key_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![conflict_id, c.manifest_hash, c.device_id, c.size_bytes, c.created_at, c.content_key_version.map(|v| v as i64)],
            ).context("insert conflict candidate")?;
        }

        // Propagate a pre-resolved report transactionally (the choose-winner
        // precedent, step 4 of `resolve_conflict_choose_winner`): first the
        // reporter's LOSING candidate as an `is_retention` change row — version
        // retention, so the losing version stays GC-pinned, and listed in the
        // version history as its reporter's signed row (ruling (10)(e)) — then
        // the winner as the new head row every device converges on via normal
        // catch-up. Same transaction as the conflict row: either the conflict
        // is recorded WITH both versions retained, or nothing landed.
        if let Some(r) = resolution {
            // Both rows file under the conflict row's own routing key (the
            // client's hash, else the derivation above): the winner's is part
            // of the statement the reporter signs, whose `path_hash` is the
            // request's (ruling (1)(ii)).
            let now_millis = now_epoch_millis();
            // Both rows join the charged population (a retained loser and a
            // head), and supersede / soft-prune credit each one back when it
            // leaves, so each is charged as it lands, as two metered records in
            // order — loser, then winner, each vetted against the counters the
            // one before it moved. A refusal returns before the commit, so the
            // transaction rolls back whole: no conflict row, no change row, no
            // charge (`file-versions.md` § Retention (4)).
            //
            // Metered like the record door, its same-manifest transfer
            // included (`writer-signed-change-records.md` ruling (11)(g)): a
            // row minted over the manifest the path's head already holds — the
            // latest-wins winner that IS the remote head, the common case —
            // adds no bytes and takes the head's charge. Both rows are asked
            // against the head the path held when the report arrived: the
            // loser retention row lands first and is no head.
            let meter = conflict_meter_in_conn(&tx, folder_id, &reporter)?;
            let head_manifest =
                content_head_manifest_in_conn(&tx, folder_id, path_hash.as_slice())?;
            let vet = |manifest: &[u8], size: i64| -> Result<RowCharge, StorageQuotaError> {
                vet_row_charge_in_conn(
                    &tx,
                    folder_id,
                    path_hash.as_slice(),
                    (head_manifest.as_deref() == Some(manifest)).then_some(manifest),
                    &meter.owner,
                    &reporter,
                    meter.member_channel.as_ref(),
                    size,
                    meter.max_storage_bytes,
                )
            };
            let settle = |charge: &RowCharge| {
                settle_row_charge_in_conn(
                    &tx,
                    folder_id,
                    &meter.owner,
                    &reporter,
                    meter.member_channel.as_ref(),
                    charge,
                )
            };
            // The reporter's own candidate is the version that would otherwise
            // be unrecorded (its device produced it but never recorded it — the
            // conflict pre-empted that). Never record it when it IS the winner
            // (the winner row below covers it).
            if let Some(l) = r.retained_loser(&candidates, &device_id) {
                let loser_charge = vet(&l.manifest_hash, l.size_bytes)?;
                tx.execute(
                    // `is_retention = 1` (loser-row ruling, 2026-08-05): the
                    // retention marker the fold accounts-and-skips on — the
                    // row keeps GC-pinning and candidate-listing the losing
                    // version, but no receiver merges it as a peer edit (the
                    // same-anchor duplication / loser-latest-wins engine).
                    // The reporter signs it, flag included (ruling (10)(d)):
                    // a reader of versions lists it only by that signature.
                    "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path, content_key_version, path_sealed, derived_through, is_retention, signature, signer_key)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14)",
                    rusqlite::params![
                        actor_id,
                        path_hash.as_slice(),
                        l.manifest_hash,
                        l.size_bytes,
                        "modify",
                        now_millis,
                        folder_id,
                        l.device_id,
                        rest_plaintext.then_some(path.as_str()),
                        l.content_key_version.map(|v| v as i64),
                        // Same path as the conflict, so the reporter's own
                        // convergent seal is this row's label too — without it
                        // a post-flip retention row would be un-renderable.
                        sealed.path_sealed,
                        // A fresh edit causally (the reporter's pre-merge
                        // local): its ancestor seq when the reporter knew it,
                        // never `is_resolution`.
                        r.losing_derived_through,
                        // The reporter's signature over exactly this row,
                        // verified by the handler; NULL when unsigned.
                        signatures.loser.map(|s| s.signature),
                        signatures.loser.map(|s| s.signer_key),
                    ],
                )
                .context("retain losing version via sync_changes")?;
                settle(&loser_charge)?;
            }
            let winner_charge = vet(&r.winning_manifest_hash, r.winning_size_bytes)?;
            tx.execute(
                "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path, content_key_version, path_sealed, derived_through, is_resolution, signature, signer_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    actor_id,
                    path_hash.as_slice(),
                    r.winning_manifest_hash,
                    r.winning_size_bytes,
                    "modify",
                    now_millis,
                    folder_id,
                    r.winner_device_id,
                    rest_plaintext.then_some(path.as_str()),
                    r.winning_content_key_version.map(|v| v as i64),
                    // See the loser row above: the same convergent seal labels
                    // the winner head every device converges on.
                    sealed.path_sealed,
                    // The 2026-08-02 ruling: a propagated winner is a
                    // resolution by construction, its watermark the
                    // reporter's claim EXACTLY as sent. The gap-2 upgrade to
                    // the loser row's seq is retired (`conflicts.md` §
                    // *Retention rows are transparent to the licence*): a
                    // reporter cannot sign a seq the nest assigns after it
                    // signs, and receivers read rule 4 against a content
                    // frontier the loser retention row never advances.
                    r.winning_derived_through,
                    // …EXCEPT a winner the reporter marked as carrying its
                    // unpublished pre-merge novelty (the same-anchor ruling,
                    // 2026-08-05): that winner is the novelty's only carrier
                    // and mints EDIT-class — a resolution stamp on it
                    // licensed the byte-free stale-skips that killed the
                    // live leg-4a append.
                    !r.winning_carries_novelty.unwrap_or(false),
                    // The reporter's signature over exactly this row (ruling
                    // (1)(ii)), verified by the handler; NULL when unsigned.
                    signatures.winner.map(|s| s.signature),
                    signatures.winner.map(|s| s.signer_key),
                ],
            )
            .context("propagate auto-resolved winner via sync_changes")?;
            settle(&winner_charge)?;
        }
        tx.commit().context("commit report_conflict")?;
        Ok(conflict_id)
    }

    /// List conflicts for a folder, optionally including resolved ones.
    pub async fn list_conflicts_for_folder(
        &self,
        folder_id: i64,
        include_resolved: bool,
    ) -> Result<Vec<SyncConflictRow>> {
        let conn = self.conn.lock().await;
        let sql = if include_resolved {
            "SELECT id, folder_id, device_id, path, conflict_type, details, created_at, resolved_at, resolution, winning_manifest_hash, path_hash, path_sealed, details_sealed
             FROM sync_conflicts WHERE folder_id = ?1 ORDER BY created_at DESC"
        } else {
            "SELECT id, folder_id, device_id, path, conflict_type, details, created_at, resolved_at, resolution, winning_manifest_hash, path_hash, path_sealed, details_sealed
             FROM sync_conflicts WHERE folder_id = ?1 AND resolved_at IS NULL ORDER BY created_at DESC"
        };
        let mut stmt = conn
            .prepare(sql)
            .context("prepare list_conflicts_for_folder")?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                Ok(SyncConflictRow {
                    id: row.get(0)?,
                    folder_id: row.get(1)?,
                    device_id: row.get(2)?,
                    path: row.get(3)?,
                    conflict_type: row.get(4)?,
                    details: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                    winning_manifest_hash: row.get(9)?,
                    path_hash: row.get(10)?,
                    path_sealed: row.get(11)?,
                    details_sealed: row.get(12)?,
                })
            })
            .context("query list_conflicts_for_folder")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read conflict row")?);
        }
        Ok(results)
    }

    /// Resolve a conflict by ID, **scoped to the caller**. Returns true if a row
    /// was updated. `sync_conflicts.id` is enumerable autoincrement, so the
    /// update is gated on the conflict's `folder` belonging to `actor_id`
    /// (mirrors `list_conflicts_for_actor`); a non-owner is a silent no-op.
    pub async fn resolve_conflict(&self, actor_id: &[u8; 32], id: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        let count = conn
            .execute(
                "UPDATE sync_conflicts SET resolved_at = ?1
                 WHERE id = ?2 AND resolved_at IS NULL
                   AND folder_id IN (SELECT id FROM folders WHERE actor_id = ?3)",
                rusqlite::params![now, id, actor_id.as_slice()],
            )
            .context("resolve sync conflict")?;
        Ok(count > 0)
    }

    /// Resolve a conflict by choosing one of its recorded candidate versions as
    /// the winner, and **propagate** that choice: the winner is recorded on the
    /// conflict row and an ordinary `sync_changes` row is written for
    /// `(folder, path, manifest_hash = winner)` so every device converges via
    /// the normal `changes.list` catch-up — no new push channel (see
    /// `docs/goal/behavior/file-sync.md` § Conflicts). The propagated change is
    /// attributed to the **winning candidate's device** so that device treats
    /// it as a self-echo (no redundant re-download) while every other device
    /// downloads the winner. The whole thing is one transaction.
    pub async fn resolve_conflict_choose_winner(
        &self,
        actor_id: &[u8; 32],
        id: i64,
        winning_manifest_hash: &[u8],
    ) -> Result<ResolveWinner, StorageQuotaError> {
        self.resolve_conflict_choose_winner_signed(actor_id, id, winning_manifest_hash, None)
            .await
    }

    /// The facts the choose-winner head row will be minted from — what a
    /// chooser's writer signature must cover (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (1)(ii)). `None` when the
    /// conflict is not the caller's, is resolved, or `winning_manifest_hash` is
    /// not a candidate; [`Self::resolve_conflict_choose_winner_signed`] then
    /// answers the precise outcome. The rows read are immutable once written,
    /// so the facts cannot drift before the resolve.
    pub async fn conflict_winner_facts(
        &self,
        actor_id: &[u8; 32],
        id: i64,
        winning_manifest_hash: &[u8],
    ) -> Result<Option<ConflictWinnerFacts>> {
        let conn = self.conn.lock().await;
        #[allow(clippy::type_complexity)]
        let conflict: Option<(i64, Vec<u8>, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT folder_id, path_hash, path_sealed FROM sync_conflicts
                 WHERE id = ?1 AND resolved_at IS NULL
                   AND folder_id IN (SELECT id FROM folders WHERE actor_id = ?2)",
                rusqlite::params![id, actor_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("lookup conflict for winner facts")?;
        let Some((folder_id, path_hash, path_sealed)) = conflict else {
            return Ok(None);
        };
        let candidate: Option<(Vec<u8>, i64, Option<i64>)> = conn
            .query_row(
                "SELECT device_id, size_bytes, content_key_version FROM sync_conflict_candidates
                 WHERE conflict_id = ?1 AND manifest_hash = ?2",
                rusqlite::params![id, winning_manifest_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("lookup winning candidate facts")?;
        let Some((device_id, size_bytes, content_key_version)) = candidate else {
            return Ok(None);
        };
        // The same routing key step 4 of the resolve propagates.
        Ok(Some(ConflictWinnerFacts {
            folder_id,
            path_hash,
            path_sealed,
            device_id,
            size_bytes,
            content_key_version,
        }))
    }

    /// [`Self::resolve_conflict_choose_winner`] carrying the chooser's verified
    /// writer signature over the winner head row, stored on that row.
    ///
    /// Metered (2026-09-28, `file-versions.md` § Retention (4)): the head row joins
    /// the charged population at the winning candidate's report-time size, so
    /// it is charged to the owner like a metered record — a negative size
    /// refuses [`StorageQuotaError::NegativeSize`], a charge past the owner's
    /// ceiling refuses [`StorageQuotaError::Exceeded`], and either leaves the
    /// conflict open with nothing written.
    pub async fn resolve_conflict_choose_winner_signed(
        &self,
        actor_id: &[u8; 32],
        id: i64,
        winning_manifest_hash: &[u8],
        signature: Option<RowSignature<'_>>,
    ) -> Result<ResolveWinner, StorageQuotaError> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin choose-winner transaction")?;

        // 1. The conflict must exist, be unresolved, AND belong to the caller's
        //    folder (owner-scoped — `id` is enumerable autoincrement, so a
        //    non-owner who guesses it must see `NotFound`, not resolve a victim's
        //    conflict). Mirrors `list_conflicts_for_actor`.
        #[allow(clippy::type_complexity)]
        let conflict: Option<(i64, String, Vec<u8>, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT folder_id, path, path_hash, path_sealed FROM sync_conflicts
                 WHERE id = ?1 AND resolved_at IS NULL
                   AND folder_id IN (SELECT id FROM folders WHERE actor_id = ?2)",
                rusqlite::params![id, actor_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .context("lookup conflict for resolve")?;
        let (folder_id, path, path_hash, path_sealed) = match conflict {
            Some(c) => c,
            None => return Ok(ResolveWinner::NotFound),
        };

        // 2. The chosen winner must be one of the recorded candidates; the
        //    candidate carries the display metadata (device, size) we attribute
        //    the propagated change to.
        let candidate: Option<(Vec<u8>, i64, Option<i64>)> = tx
            .query_row(
                "SELECT device_id, size_bytes, content_key_version FROM sync_conflict_candidates
                 WHERE conflict_id = ?1 AND manifest_hash = ?2",
                rusqlite::params![id, winning_manifest_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("lookup winning candidate")?;
        let (winner_device_id, size_bytes, winner_ckv) = match candidate {
            Some(c) => c,
            None => return Ok(ResolveWinner::BadCandidate),
        };
        // The head row below is a charged version minted at the candidate's
        // report-time declaration: refuse it the way the record door refuses
        // its own (a negative size would credit the owner; one past the
        // ceiling would rest uncharged-at-quota), before anything is written.
        // The report door refuses negative candidates now, but a candidate row
        // is data at rest, so the mint re-checks it.
        if size_bytes < 0 {
            return Err(StorageQuotaError::NegativeSize { size_bytes });
        }
        // Owner-scoped (step 1), so the chooser IS the owner: no member half.
        // The winning candidate is normally a version the path already holds
        // as its head, so the mint is the record door's same-manifest case
        // (`writer-signed-change-records.md` ruling (11)(g)): it takes the
        // head's charge rather than adding one, and is never refused at a full
        // quota for a choose that moves no bytes.
        let meter = conflict_meter_in_conn(&tx, folder_id, actor_id)?;
        let head_manifest = content_head_manifest_in_conn(&tx, folder_id, &path_hash)?;
        let row_charge = vet_row_charge_in_conn(
            &tx,
            folder_id,
            &path_hash,
            (head_manifest.as_deref() == Some(winning_manifest_hash))
                .then_some(winning_manifest_hash),
            &meter.owner,
            actor_id,
            meter.member_channel.as_ref(),
            size_bytes,
            meter.max_storage_bytes,
        )?;

        // 3. Record the winner + mark resolved.
        let now_secs = now_epoch_secs();
        tx.execute(
            "UPDATE sync_conflicts SET resolved_at = ?1, winning_manifest_hash = ?2 WHERE id = ?3",
            rusqlite::params![now_secs, winning_manifest_hash, id],
        )
        .context("record conflict winner")?;

        // 4. Propagate via an ordinary change record (same shape as
        //    `record_sync_change` — `created_at` is millis there).
        //
        //    The routing key is READ off the conflict row rather than re-derived
        //    from its plaintext (path-sealing S1): the plaintext scrubs, and
        //    the `NOT NULL` companion is stamped at insert. The conflict's
        //    sealed label rides onto the propagated change the same way — a
        //    keyless server-side copy of a self-describing envelope.
        let now_millis = now_epoch_millis();
        tx.execute(
            "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path, content_key_version, path_sealed, signature, signer_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                actor_id.as_slice(),
                path_hash.as_slice(),
                winning_manifest_hash,
                size_bytes,
                "modify",
                now_millis,
                folder_id,
                winner_device_id,
                // The conflict row's rest value: '' is the scrub sentinel
                // (post-flip sealed rows) — propagate NULL, not the sentinel;
                // a web-mode row's real plaintext passes through.
                (!path.is_empty()).then_some(path.as_str()),
                // Sealed-set readers select the decrypt key by the row's
                // generation — echo the winning candidate's (was silently NULL
                // before candidates carried it).
                winner_ckv,
                path_sealed,
                // The chooser's signature over exactly this row (ruling
                // (1)(ii)), verified by the handler; NULL when unsigned.
                signature.map(|s| s.signature),
                signature.map(|s| s.signer_key),
            ],
        )
        .context("propagate conflict winner via sync_changes")?;
        let change_seq = tx.last_insert_rowid();
        settle_row_charge_in_conn(
            &tx,
            folder_id,
            &meter.owner,
            actor_id,
            meter.member_channel.as_ref(),
            &row_charge,
        )?;

        tx.commit().context("commit choose-winner")?;
        Ok(ResolveWinner::Resolved { change_seq })
    }

    /// Returns `true` iff this nest holds this `(kind, scope)`'s reserved
    /// folder as a **custody copy** — meaning the nest stores opaque
    /// chunks only and cannot serve IMAP, run compaction, or create a
    /// message-kind snapshot for this `(kind, scope)`.
    ///
    /// The reserved folder name is derived from the segment `kind`
    /// (`mail` → `__mail`). The predicate reads the row's
    /// `folders.custody_copy` through
    /// [`crate::db::snapshots::is_reserved_custody_copy`] — the flag this nest
    /// wrote itself when it provisioned the copy, never a value the data owner
    /// declared (`reserved-folders.md` § Destination capability). See
    /// `docs/goal/architecture/message-segment-store.md` § Destination
    /// capability for the full enforcement table.
    pub async fn is_pure_backup_destination(
        &self,
        kind: &str,
        scope_id: &[u8; 32],
    ) -> Result<bool> {
        // The reserved folder name is derived from the segment kind. Mail,
        // conv and post are wired (conv = Plan 9, post = Track C); calendar and
        // card joined at S6.8c, when compaction (and its orphan reaper) first
        // ran over them — a pure-backup destination holds no metadata rows, so
        // an ungated reaper would read that nest's entire mirror as orphaned.
        // Unknown kinds are conservatively "not a pure-backup destination"
        // (returns false).
        //
        // Mail's reserved set is the fixed name `__mail` scoped by
        // `actor_id = actor`. Post mirrors mail — fixed name `__post` scoped by
        // `actor_id = author` (a post's audience-scope is its author, exactly
        // one `__post` scope per author, so the mail shape fits; see
        // `get_or_create_reserved_folder(author, "post")` in db/snapshots.rs).
        // Conv encodes the channel in the NAME (`__conv/<channel_hex>`) AND
        // scopes by `actor_id = channel_id` — historical, because one nest holds
        // many channels (see `get_or_create_reserved_conv_folder`). Either way
        // the `WHERE name = ?1 AND actor_id = ?2` query below binds `scope_id`
        // as `?2` (the actor for mail/post, the channel for conv).
        let Some(folder_name) = reserved_backup_set_name(kind, scope_id) else {
            return Ok(false);
        };
        let conn = self.conn.lock().await;
        let custody_copy: bool = conn
            .query_row(
                "SELECT custody_copy FROM folders WHERE name = ?1 AND actor_id = ?2",
                rusqlite::params![folder_name, scope_id.as_slice()],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .optional()
            .context("query is_pure_backup_destination")?
            .unwrap_or(false);
        if !crate::db::snapshots::is_reserved_custody_copy(custody_copy, &folder_name) {
            return Ok(false);
        }
        // Channel-scoped conv is out of this rule: a channel's scope holds live
        // records on every member's nest, so records there say nothing about a
        // materialize, and a conv set cannot be materialized yet anyway.
        if kind == "conv" {
            return Ok(true);
        }
        // "Pure" is the corpus shape, not the custody flag alone: a destination
        // holds opaque chunks and no local plaintext-framed segments, which is
        // what every caller of this predicate refuses or skips on. A re-seeded
        // nest keeps the custody set marked a custody copy after
        // `fauna.backup.custody.materialize` (the custody rows stay, and a
        // retried delivery still records into them), but its scope now holds
        // LIVE records. For an actor-scoped kind nothing else can produce that
        // pair, since no enroll points an owner's destination at their own home
        // nest. So it is the owner's live account, and serving,
        // snapshotting and compacting it is exactly what "seeded means a live
        // account" asks for (`behavior/backup-destinations.md` § Re-seed).
        let live = crate::segments::records_db::count_live_records(&conn, scope_id, kind)
            .context("count live records for is_pure_backup_destination")?;
        Ok(live == 0)
    }

    /// `include_resolved = true` also returns resolved rows (the auto-resolve
    /// review list); `false` keeps the historic unresolved-only contract.
    pub async fn list_conflicts_for_actor(
        &self,
        actor_id: &[u8],
        include_resolved: bool,
    ) -> Result<Vec<SyncConflictRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let sql = if include_resolved {
            "SELECT sc.id, sc.folder_id, sc.device_id, sc.path, sc.conflict_type, sc.details, sc.created_at, sc.resolved_at, sc.resolution, sc.winning_manifest_hash, sc.path_hash, sc.path_sealed, sc.details_sealed
             FROM sync_conflicts sc
             JOIN folders fs ON sc.folder_id = fs.id
             WHERE fs.actor_id = ?1
             ORDER BY sc.created_at DESC"
        } else {
            "SELECT sc.id, sc.folder_id, sc.device_id, sc.path, sc.conflict_type, sc.details, sc.created_at, sc.resolved_at, sc.resolution, sc.winning_manifest_hash, sc.path_hash, sc.path_sealed, sc.details_sealed
             FROM sync_conflicts sc
             JOIN folders fs ON sc.folder_id = fs.id
             WHERE fs.actor_id = ?1 AND sc.resolved_at IS NULL
             ORDER BY sc.created_at DESC"
        };
        let mut stmt = conn
            .prepare(sql)
            .context("prepare list_conflicts_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id], |row| {
                Ok(SyncConflictRow {
                    id: row.get(0)?,
                    folder_id: row.get(1)?,
                    device_id: row.get(2)?,
                    path: row.get(3)?,
                    conflict_type: row.get(4)?,
                    details: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                    winning_manifest_hash: row.get(9)?,
                    path_hash: row.get(10)?,
                    path_sealed: row.get(11)?,
                    details_sealed: row.get(12)?,
                })
            })
            .context("query list_conflicts_for_actor")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read conflict row")?);
        }
        Ok(results)
    }

    /// Load candidate versions for all of an actor's conflicts (resolved ones
    /// included — the review list needs them) in one query, returned as
    /// `(conflict_id, candidate)` pairs for the handler to group per conflict.
    /// Pairs with `list_conflicts_for_actor`; conflicts the reply omits simply
    /// never consume their pairs.
    pub async fn list_conflict_candidates_for_actor(
        &self,
        actor_id: &[u8],
    ) -> Result<Vec<(i64, ConflictCandidateRow)>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cc.conflict_id, cc.manifest_hash, cc.device_id, cc.size_bytes, cc.created_at, cc.content_key_version
                 FROM sync_conflict_candidates cc
                 JOIN sync_conflicts sc ON cc.conflict_id = sc.id
                 JOIN folders fs ON sc.folder_id = fs.id
                 WHERE fs.actor_id = ?1
                 ORDER BY cc.id",
            )
            .context("prepare list_conflict_candidates_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ConflictCandidateRow {
                        manifest_hash: row.get(1)?,
                        device_id: row.get(2)?,
                        size_bytes: row.get(3)?,
                        created_at: row.get(4)?,
                        content_key_version: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    },
                ))
            })
            .context("query list_conflict_candidates_for_actor")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read conflict candidate row")?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod custody_grace_window_tests {
    use super::{GenerationRestore, StorageQuotaError};
    use crate::db::CacheDb;

    const T: i64 = crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS;

    async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
        db.list_users()
            .await
            .unwrap()
            .into_iter()
            .find(|u| u.actor_id == actor.to_vec())
            .unwrap()
            .storage_bytes_used
    }

    /// Create an owner + one set of the given name, return its id. A reserved
    /// name is minted as the custody copy a provisioner would mint; any other
    /// is an ordinary folder.
    async fn setup(db: &CacheDb, owner: &[u8; 32], name: &str) -> i64 {
        db.create_user(owner, "free", "owner").await.unwrap();
        db.create_folder_with_options(
            name,
            owner,
            crate::db::FolderOptions {
                custody_copy: crate::db::snapshots::is_reserved_folder_name(name),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.get_folder(name).await.unwrap().unwrap().id
    }

    /// the custody twin's half of sweep's negative-size
    /// refusal. The sweep closed the credit in `record_sync_change_metered` and
    /// placed it in that metering core so no *door* could be added past it —
    /// but `upsert_backup_custody` is a second CORE, reached by its own branch
    /// of `record_change_core`, and for an ordinary folder its
    /// `size_bytes` is the owner client's DECLARED logical size. It was
    /// unguarded: PROBE-389-F floored a full 1000-byte tier to 0 with one
    /// record and then wrote 900 bytes over the full tier.
    ///
    /// Red-verifiable: drop the `size_bytes < 0` guard and this test sees
    /// `Ok(())`, `used == 0`, and the follow-up write succeed.
    #[tokio::test]
    async fn a_negative_custody_size_is_refused_and_credits_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x51u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let max = 1_000i64;

        // Fill the tier honestly.
        db.upsert_backup_custody(
            &owner,
            fs_id,
            blake3::hash(b"a.dat").as_bytes(),
            Some("a.dat"),
            &[0xA1u8; 32],
            1_000,
            None,
            None,
            max,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 1_000, "the tier is now full");

        // The evasion: a fresh path declared at a large negative size.
        let err = db
            .upsert_backup_custody(
                &owner,
                fs_id,
                blake3::hash(b"free.dat").as_bytes(),
                Some("free.dat"),
                &[0xB2u8; 32],
                -1_000_000,
                None,
                None,
                max,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                StorageQuotaError::NegativeSize {
                    size_bytes: -1_000_000
                }
            ),
            "a negative declared custody size is refused typed, not metered: {err}"
        );

        // Refused before any write: nothing credited...
        assert_eq!(
            used(&db, owner).await,
            1_000,
            "a refused custody record credits nothing"
        );
        // ...and the tier is still enforced.
        let after = db
            .upsert_backup_custody(
                &owner,
                fs_id,
                blake3::hash(b"more.dat").as_bytes(),
                Some("more.dat"),
                &[0xC3u8; 32],
                900,
                None,
                None,
                max,
            )
            .await;
        assert!(
            matches!(after, Err(StorageQuotaError::Exceeded { .. })),
            "the tier must still be full after the refused record"
        );
    }

    /// The core of the grace window: on a **reserved destination** set a
    /// supersede must RETAIN the generation it replaces, not overwrite it away.
    /// Before this, `upsert_backup_custody`'s `ON CONFLICT … DO UPDATE SET
    /// manifest_hash = excluded.manifest_hash` meant the prior manifest was
    /// *forgotten*, so GC could never see it again and a rogue source nest's
    /// supersede was a permanent delete.
    #[tokio::test]
    async fn a_supersede_retains_the_generation_it_replaces() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000001.dat").as_bytes();
        let old = [0xA1u8; 32];
        let new = [0xB2u8; 32];

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000001.dat"),
            &old,
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000001.dat"),
            &new,
            700,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        let live = db.backup_custody_manifest_hashes().await.unwrap();
        assert_eq!(
            live,
            vec![new.to_vec()],
            "the new generation is the live one"
        );

        let retained = db
            .backup_custody_generation_manifest_hashes()
            .await
            .unwrap();
        assert_eq!(
            retained,
            vec![old.to_vec()],
            "the superseded generation must be RETAINED — it is what GC pins so a \
             rogue source's supersede is recoverable, not terminal"
        );
    }

    /// A tombstone is the *other* half of a custody writer's delete power, so it
    /// retains exactly like a supersede does.
    #[tokio::test]
    async fn a_tombstone_retains_the_generation_it_removes() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000002.dat").as_bytes();
        let m = [0xC3u8; 32];

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000002.dat"),
            &m,
            400,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.tombstone_backup_custody(fs_id, &ph).await.unwrap();

        assert!(
            db.backup_custody_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "the tombstoned path holds no live manifest"
        );
        assert_eq!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap(),
            vec![m.to_vec()],
            "a tombstoned generation is retained for the grace window"
        );
        assert_eq!(
            used(&db, owner).await,
            400,
            "a retained generation stays CHARGED — quota-charging retention is \
             what makes quota the supersede rate cap"
        );
    }

    /// Scope gate: an **ordinary** user backup-type set (the wizard's Backup
    /// mode / Photo Library) is written by the owner's own client and already
    /// protects superseded versions with real snapshot pins. Retaining
    /// generations there too would double-protect and double-charge the bytes.
    #[tokio::test]
    async fn an_ordinary_backup_mode_set_does_not_retain_generations() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "photo-library").await;
        let ph: [u8; 32] = *blake3::hash(b"IMG_0001.heic").as_bytes();

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("IMG_0001.heic"),
            &[0xA1u8; 32],
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("IMG_0001.heic"),
            &[0xB2u8; 32],
            700,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "an ordinary folder must NOT retain generations — snapshots \
             already pin its superseded versions"
        );
        assert_eq!(
            used(&db, owner).await,
            700,
            "and its supersede accounting is unchanged: the old bytes are freed"
        );
    }

    /// Quota is the rate cap (ratified: 'reclaim rate-cap = not-a-parameter').
    /// Each *distinct* superseded generation stays charged, so a supersede storm
    /// walks the owner's own quota boundary instead of needing a bespoke limiter
    /// — and a write that would cross it is refused.
    #[tokio::test]
    async fn retained_generations_stay_charged_until_the_quota_refuses_the_storm() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000003.dat").as_bytes();

        for (i, size) in [400i64, 400, 400].into_iter().enumerate() {
            db.upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000003.dat"),
                &[i as u8 + 1; 32],
                size,
                None,
                None,
                2_000,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            used(&db, owner).await,
            1_200,
            "three generations of 400 = 1200 charged (two retained + one live)"
        );

        // The fourth crosses the 2000-byte cap: 1200 + 900 > 2000.
        let refused = db
            .upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000003.dat"),
                &[0x9Fu8; 32],
                900,
                None,
                None,
                2_000,
            )
            .await;
        assert!(
            matches!(refused, Err(super::StorageQuotaError::Exceeded { .. })),
            "the supersede storm must hit the owner's own quota wall, got {refused:?}"
        );
        assert_eq!(
            used(&db, owner).await,
            1_200,
            "a refused write charges nothing"
        );
    }

    /// A quota-refused write must leave the custody tables **completely**
    /// untouched — not merely the byte counter. Retaining before the quota
    /// decision would leave the still-live manifest also sitting in the retained
    /// set: the invariant broken, the manifest over-pinned in the GC walk, and a
    /// generation listed to the user that is actually the live one.
    #[tokio::test]
    async fn a_quota_refused_write_retains_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000009.dat").as_bytes();
        let live = [0xA1u8; 32];

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000009.dat"),
            &live,
            900,
            None,
            None,
            1_000,
        )
        .await
        .unwrap();
        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "precondition: nothing retained yet"
        );

        // 900 already charged + 900 more > the 1000 cap ⇒ refused.
        let refused = db
            .upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000009.dat"),
                &[0xB2u8; 32],
                900,
                None,
                None,
                1_000,
            )
            .await;
        assert!(
            matches!(refused, Err(super::StorageQuotaError::Exceeded { .. })),
            "expected a quota refusal, got {refused:?}"
        );

        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "a REFUSED write must retain nothing — the live manifest must not also \
             appear in the retained set"
        );
        assert_eq!(
            db.backup_custody_manifest_hashes().await.unwrap(),
            vec![live.to_vec()],
            "and the live row is untouched"
        );
        assert_eq!(used(&db, owner).await, 900, "and nothing is charged");
    }

    /// A path that flaps between two contents must not grow rows or double-charge:
    /// the destination genuinely only holds two distinct generations. This pins
    /// the accounting invariant the window rests on — **a manifest live for a
    /// path is never simultaneously retained for that path**.
    #[tokio::test]
    async fn a_flapping_path_charges_each_distinct_generation_exactly_once() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000004.dat").as_bytes();
        let a = [0xAAu8; 32];
        let b = [0xBBu8; 32];

        for m in [&a, &b, &a, &b, &a, &b] {
            db.upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000004.dat"),
                m,
                300,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        }

        let retained = db
            .backup_custody_generation_manifest_hashes()
            .await
            .unwrap();
        assert_eq!(
            retained,
            vec![a.to_vec()],
            "A is retained, B is live — never both, and never a growing row set"
        );
        assert_eq!(
            used(&db, owner).await,
            600,
            "two distinct generations of 300 = 600, however many times they flap"
        );
    }

    /// A content-identical re-record (the same manifest recorded again while it
    /// is still the live one — a reconnect / crashed-ack re-drive) is a
    /// freshness touch, not a supersede: it charges nothing and retains
    /// nothing. Pre-fix it retained the still-live manifest as a generation and
    /// charged its bytes a second time — the both-live-and-retained state the
    /// module's own invariant forbids (`unretain_custody_generation_in_conn`
    /// doc), self-healing only at T.
    #[tokio::test]
    async fn a_content_identical_re_record_charges_nothing_and_retains_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000009.dat").as_bytes();
        let m = [0xD4u8; 32];

        for _ in 0..2 {
            db.upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000009.dat"),
                &m,
                500,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        }

        assert_eq!(
            used(&db, owner).await,
            500,
            "a content-identical replay charges nothing"
        );
        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "the live manifest must never be retained as its own generation"
        );
        assert_eq!(
            db.backup_custody_manifest_hashes().await.unwrap(),
            vec![m.to_vec()],
            "the path stays live under the same manifest"
        );
    }

    /// The window actually closes: past T the generation is reclaimed, its bytes
    /// credited back, and its manifest leaves the GC reference set so its
    /// exclusive chunks become sweepable.
    #[tokio::test]
    async fn a_generation_past_the_grace_window_is_reclaimed_and_credited_back() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000005.dat").as_bytes();

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000005.dat"),
            &[0xA1u8; 32],
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000005.dat"),
            &[0xB2u8; 32],
            700,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 1_200);

        let now = crate::db::now_epoch_secs();

        // One second short of T: still protected. This is the assertion that
        // makes the window a window rather than an immediate reclaim.
        let reclaimed = db
            .reclaim_expired_backup_custody_generations(now - T + 1)
            .await
            .unwrap();
        assert_eq!(reclaimed, 0, "inside T nothing is reclaimed");
        assert_eq!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .len(),
            1,
            "and the generation is still pinned for GC"
        );
        assert_eq!(used(&db, owner).await, 1_200, "and still charged");

        // Past T.
        let reclaimed = db
            .reclaim_expired_backup_custody_generations(now)
            .await
            .unwrap();
        assert_eq!(reclaimed, 1);
        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "past T the generation leaves the GC reference set so its chunks reclaim"
        );
        assert_eq!(
            used(&db, owner).await,
            700,
            "and its bytes are credited back — only the live generation remains charged"
        );
    }

    /// Restore is what makes the window *useful*: after revoking a rogue source's
    /// writer grant, the owner promotes a good generation back to live. The
    /// displaced generation is retained by the same machinery, so restoring is
    /// never itself destructive.
    #[tokio::test]
    async fn restore_promotes_a_retained_generation_and_retains_the_displaced_one() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000006.dat").as_bytes();
        let good = [0xA1u8; 32];
        let junk = [0xB2u8; 32];

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000006.dat"),
            &good,
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        // The rogue source overwrites it with junk.
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000006.dat"),
            &junk,
            10,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        let gens = db
            .list_backup_custody_generations(&owner, None, 0)
            .await
            .unwrap();
        assert_eq!(gens.len(), 1);
        assert_eq!(gens[0].manifest_hash, good.to_vec());
        assert_eq!(gens[0].folder_name, "__mail");
        assert_eq!(gens[0].path.as_deref(), Some("seg-00000006.dat"));
        assert_eq!(gens[0].size_bytes, 500);

        let restored = db
            .restore_backup_custody_generation(&owner, "__mail", &ph, &good)
            .await
            .unwrap();
        assert_eq!(restored, GenerationRestore::Restored);

        assert_eq!(
            db.backup_custody_manifest_hashes().await.unwrap(),
            vec![good.to_vec()],
            "the good generation is live again"
        );
        assert_eq!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap(),
            vec![junk.to_vec()],
            "and the junk it displaced is itself retained — restore is never destructive"
        );
        assert_eq!(
            used(&db, owner).await,
            510,
            "quota is unchanged by the swap: both generations are still held"
        );
    }

    /// A generation superseded before the owner's writer seat was taken is a
    /// previous writer's numbering: restoring it is refused and changes
    /// nothing, while one superseded under the seat restores as ever.
    #[tokio::test]
    async fn restore_refuses_a_generation_from_before_the_writer_seat() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000006.dat").as_bytes();
        let (before, live, after) = ([0xA1u8; 32], [0xB2u8; 32], [0xC3u8; 32]);
        for (manifest, size) in [(&before, 500), (&live, 10)] {
            db.upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000006.dat"),
                manifest,
                size,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        }
        // `before` was superseded a minute before the seat is taken.
        db.backdate_backup_custody_generations_for_test(60)
            .await
            .unwrap();
        db.register_backup_writer(&owner, &[0x51u8; 32], None)
            .await
            .unwrap();

        assert_eq!(
            db.restore_backup_custody_generation(&owner, "__mail", &ph, &before)
                .await
                .unwrap(),
            GenerationRestore::BeforeSeat
        );
        assert_eq!(
            db.backup_custody_manifest_hashes().await.unwrap(),
            vec![live.to_vec()],
            "a refused restore leaves the live generation where it was"
        );
        assert_eq!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap(),
            vec![before.to_vec()],
            "and the refused one retained, to age out under T"
        );

        // Superseded under the seat: restorable.
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000006.dat"),
            &after,
            20,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(
            db.restore_backup_custody_generation(&owner, "__mail", &ph, &live)
                .await
                .unwrap(),
            GenerationRestore::Restored
        );
    }

    /// The cursor pagination behind `fauna.backup.generation.list` — the reads
    /// land in one epoch second, so this exercises the sharp edge: equal
    /// `superseded_at` keys disambiguated by the `rowid` tiebreaker. A cursor
    /// that compared only the leading key would skip or repeat the tied rows.
    #[tokio::test]
    async fn generation_pages_concatenate_to_the_unpaged_list_across_tied_keys() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000008.dat").as_bytes();
        // Six manifests in turn on one path: five retained generations.
        for i in 0u8..6 {
            db.upsert_backup_custody(
                &owner,
                fs_id,
                &ph,
                Some("seg-00000008.dat"),
                &[0xC0 + i; 32],
                100,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        }

        let all = db
            .list_backup_custody_generations(&owner, None, 0)
            .await
            .unwrap();
        assert_eq!(all.len(), 5);

        let mut paged = Vec::new();
        let mut after = None;
        // Bounded walk: 5 rows at limit 2 needs 3 pages + 1 empty — a cursor
        // the query ignored would repeat pages forever, and that must FAIL,
        // not hang.
        for _ in 0..10 {
            let page = db
                .list_backup_custody_generations(&owner, after, 2)
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            let last = page.last().unwrap();
            after = Some((last.superseded_at, last.rowid));
            paged.extend(page);
        }
        assert!(paged.len() <= 5, "a page repeated — the cursor was ignored");

        assert_eq!(
            paged
                .iter()
                .map(|g| (g.manifest_hash.clone(), g.rowid))
                .collect::<Vec<_>>(),
            all.iter()
                .map(|g| (g.manifest_hash.clone(), g.rowid))
                .collect::<Vec<_>>(),
            "pages concatenate to exactly the unpaged serve — no row skipped or repeated"
        );
    }

    /// Ownership: `restore` resolves the set by `(name, actor_id = caller)`, so a
    /// caller can only ever restore into custody held for **itself**.
    #[tokio::test]
    async fn restore_refuses_another_actors_custody_and_an_unknown_generation() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let other = [0x22u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        db.create_user(&other, "free", "other").await.unwrap();
        let ph: [u8; 32] = *blake3::hash(b"seg-00000007.dat").as_bytes();
        let good = [0xA1u8; 32];

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000007.dat"),
            &good,
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000007.dat"),
            &[0xB2u8; 32],
            10,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        assert_eq!(
            db.restore_backup_custody_generation(&other, "__mail", &ph, &good)
                .await
                .unwrap(),
            GenerationRestore::NotFound,
            "another actor must not reach this owner's custody set"
        );
        assert!(
            db.list_backup_custody_generations(&other, None, 0)
                .await
                .unwrap()
                .is_empty(),
            "nor see it in a listing"
        );
        assert_eq!(
            db.restore_backup_custody_generation(&owner, "__mail", &ph, &[0xFFu8; 32])
                .await
                .unwrap(),
            GenerationRestore::NotFound,
            "an unknown / already-reclaimed generation restores nothing"
        );
    }

    /// Destination removal drops the set, so its retained generations must be
    /// credited back too — otherwise removing a destination leaks quota that no
    /// later sweep can free (the rows are gone with the set).
    #[tokio::test]
    async fn deleting_the_custody_set_credits_back_retained_generations() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let fs_id = setup(&db, &owner, "__mail").await;
        let ph: [u8; 32] = *blake3::hash(b"seg-00000008.dat").as_bytes();

        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000008.dat"),
            &[0xA1u8; 32],
            500,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        db.upsert_backup_custody(
            &owner,
            fs_id,
            &ph,
            Some("seg-00000008.dat"),
            &[0xB2u8; 32],
            700,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 1_200);

        db.delete_folder_for_user("__mail", &owner).await.unwrap();

        assert_eq!(
            used(&db, owner).await,
            0,
            "removing the destination credits back BOTH the live and the retained bytes"
        );
        assert!(
            db.backup_custody_generation_manifest_hashes()
                .await
                .unwrap()
                .is_empty(),
            "and leaves no orphaned generation rows behind"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{ResolveWinner, StorageQuotaError, SupersedeOutcome};
    use crate::db::{CacheDb, FolderRow, FolderUpdate};

    /// Multi-writer Phase 1 owner-pays metering (`file-sync.md` § Multi-writer
    /// shared sets): a writer's record charges the **owner's**
    /// `storage_bytes_used`, bumps the writer's role-row `bytes_used` in the
    /// same locked step, refuses a cap-exceeding record with
    /// `MemberCapExceeded` charging **nothing**, and floors the member counter
    /// at 0 on delete-reclaim.
    #[tokio::test]
    async fn writer_record_charges_owner_and_enforces_member_cap() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let channel = [0xCCu8; 32];
        db.set_folder_member_access(&channel, &writer, "writer", Some(1000))
            .await
            .unwrap();

        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }

        let p: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let q: [u8; 32] = *blake3::hash(b"b.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let m1 = [0xA1u8; 32];

        // Writer records 600 bytes → the OWNER is charged; the writer is not;
        // the member abuse counter bumps.
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &p,
            Some(&m1),
            600,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 600, "owner pays");
        assert_eq!(
            used(&db, writer).await,
            0,
            "the writer's own quota untouched"
        );
        let role = db
            .get_folder_member_role(&channel, &writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role.bytes_used, 600);

        // A second record that would push the member past their 1000-byte cap
        // refuses typed — and charges NOTHING on either counter.
        let err = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &q,
                Some(&[0xA2u8; 32]),
                600,
                "create",
                fs.id,
                &dev,
                Some("b.txt"),
                None,
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageQuotaError::MemberCapExceeded { .. }),
            "typed member_cap_exceeded, got: {err}"
        );
        assert_eq!(
            used(&db, owner).await,
            600,
            "refusal charges nothing (owner)"
        );
        assert_eq!(
            db.get_folder_member_role(&channel, &writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used,
            600,
            "refusal charges nothing (member counter)"
        );

        // The writer deletes the path → retained accounting: NET ZERO. The
        // former head stays listable (restorable) and keeps charging both
        // counters until the retention pipeline releases it — the
        // escape closed (file-versions.md § Retention (4)).
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &p,
            None,
            0,
            "delete",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 600, "delete is net zero (owner)");
        assert_eq!(
            db.get_folder_member_role(&channel, &writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used,
            600,
            "delete is net zero — the retained version keeps charging the member"
        );

        // An uncapped writer (byte_cap = NULL) is limited only by the owner's
        // ceiling — the ratified blank-cap-means-uncapped.
        db.set_folder_member_access(&channel, &writer, "writer", None)
            .await
            .unwrap();
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &q,
            Some(&[0xA3u8; 32]),
            5000,
            "create",
            fs.id,
            &dev,
            Some("b.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 5600, "600 retained + 5000 new");

        // The OWNER's ceiling refuses a writer record too (owner-pays means the
        // owner's quota is the hard boundary) — typed Exceeded, nothing charged.
        // (A fresh path keeps this a clean create with prior 0.)
        let r: [u8; 32] = *blake3::hash(b"c.txt").as_bytes();
        let err = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &r,
                Some(&[0xA4u8; 32]),
                600,
                "create",
                fs.id,
                &dev,
                Some("c.txt"),
                None,
                None,
                None,
                None,
                None,
                5100,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StorageQuotaError::Exceeded { .. }));
        assert_eq!(used(&db, owner).await, 5600);
    }

    /// **The probe (file-versions.md § Retention (4), slice 3).** A
    /// granted member's create→delete loop must be REFUSED by the `byte_cap`
    /// while the deleted versions rest pinned on the owner's nest — retained
    /// accounting: a delete releases nothing (the former head stays listable
    /// and restorable), so the member's counter keeps carrying every retained
    /// version they recorded until the retention pipeline actually releases it.
    #[tokio::test]
    async fn a_member_create_delete_loop_is_bounded_by_the_cap_while_bytes_rest_pinned() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let channel = [0xCCu8; 32];
        db.set_folder_member_access(&channel, &writer, "writer", Some(1000))
            .await
            .unwrap();

        let p: [u8; 32] = *blake3::hash(b"loop.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let record = |manifest: [u8; 32], change: &'static str| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &writer,
                    &owner,
                    Some(&channel),
                    &p,
                    (change == "create").then_some(&manifest),
                    if change == "create" { 600 } else { 0 },
                    change,
                    fs_id,
                    &dev,
                    Some("loop.txt"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    i64::MAX,
                )
                .await
            }
        };

        record([0xA1u8; 32], "create").await.unwrap();
        record([0x00u8; 32], "delete").await.unwrap();
        // The loop's second lap: the first 600 bytes still rest pinned and
        // charged, so 600 more must pass the member's 1000-byte cap — REFUSED,
        // not observed.
        let err = record([0xA2u8; 32], "create").await.unwrap_err();
        match err {
            StorageQuotaError::MemberCapExceeded {
                used,
                requested,
                cap,
            } => {
                assert_eq!(used, 600, "the retained lap keeps charging");
                assert_eq!(requested, 600);
                assert_eq!(cap, 1000);
            }
            other => panic!("expected MemberCapExceeded, got {other:?}"),
        }
        // Both counters carry the retained version — nothing was refunded by
        // the delete, and the refused lap charged nothing.
        let role = db
            .get_folder_member_role(&channel, &writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role.bytes_used, 600);
    }

    /// The owner twin: retained accounting bounds a create→delete loop by
    /// `max_storage_bytes` — a delete is net zero (the former head keeps
    /// charging as a retained version), so the loop ratchets to the ceiling
    /// and refuses there instead of evading the quota forever.
    #[tokio::test]
    async fn an_owner_create_delete_loop_is_bounded_by_the_quota() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("mine", &owner).await.unwrap();
        let fs = db.get_folder("mine").await.unwrap().unwrap();
        let p: [u8; 32] = *blake3::hash(b"loop.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let record = |manifest: [u8; 32], change: &'static str| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &owner,
                    &owner,
                    None,
                    &p,
                    (change == "create").then_some(&manifest),
                    if change == "create" { 600 } else { 0 },
                    change,
                    fs_id,
                    &dev,
                    Some("loop.txt"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    1000,
                )
                .await
            }
        };
        record([0xA1u8; 32], "create").await.unwrap();
        record([0x00u8; 32], "delete").await.unwrap();
        let err = record([0xA2u8; 32], "create").await.unwrap_err();
        match err {
            StorageQuotaError::Exceeded {
                used,
                requested,
                max,
            } => {
                assert_eq!(used, 600, "the retained lap keeps charging");
                assert_eq!(requested, 600);
                assert_eq!(max, 1000);
            }
            other => panic!("expected Exceeded, got {other:?}"),
        }
    }

    /// The release side (§ Retention (4) ⇄ (3)): the retention pipeline's
    /// soft-prune credits the released version to both counters, undelete
    /// re-charges it (never refuses — recovery must not strand behind a full
    /// quota), and a second undelete of the same row moves nothing.
    #[tokio::test]
    async fn soft_prune_releases_the_retained_charge_and_undelete_recharges() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("mine", &owner).await.unwrap();
        let fs = db.get_folder("mine").await.unwrap().unwrap();
        let p: [u8; 32] = *blake3::hash(b"doc.txt").as_bytes();
        let dev = [0xD1u8; 32];
        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }
        let record = |manifest: [u8; 32]| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &owner,
                    &owner,
                    None,
                    &p,
                    Some(&manifest),
                    600,
                    "create",
                    fs_id,
                    &dev,
                    Some("doc.txt"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    i64::MAX,
                )
                .await
            }
        };
        let v1 = record([0xA1u8; 32]).await.unwrap();
        let _v2 = record([0xA2u8; 32]).await.unwrap();
        assert_eq!(used(&db, owner).await, 1200, "head + retained both charge");

        assert!(db.soft_prune_version(v1).await.unwrap());
        assert_eq!(used(&db, owner).await, 600, "soft-prune releases the row");
        // Idempotent: re-running the pipeline hop on an already-pruned row
        // transitions nothing and credits nothing.
        assert!(!db.soft_prune_version(v1).await.unwrap());
        assert_eq!(used(&db, owner).await, 600);

        assert!(db.undelete_version(v1).await.unwrap());
        assert_eq!(used(&db, owner).await, 1200, "undelete re-charges");
        assert!(!db.undelete_version(v1).await.unwrap());
        assert_eq!(
            used(&db, owner).await,
            1200,
            "a second undelete moves nothing"
        );
    }

    /// Undelete never refuses (§ Retention (4)) — so at an `i64::MAX`-class
    /// ceiling (a missing tier row, or an effectively unlimited tier) its
    /// re-charge is the one door that can push a counter past the integer
    /// domain: record A admitted exactly up to the ceiling → prune A (credit)
    /// → refill → undelete A. An unclamped `used + size` overflows SQLite's
    /// integer arithmetic, the column turns REAL, and every `i64` read of it —
    /// the nest-wide admin user listing included — fails, repairable from no
    /// app. Both counters the re-charge moves (the owner's
    /// `storage_bytes_used` and the recording member's `bytes_used`) must
    /// saturate at `i64::MAX` instead, and the undelete must still succeed.
    ///
    /// Red-verifiable: drop either UPDATE's clamp in
    /// `adjust_version_accounting_in_conn` and the matching read below fails.
    #[tokio::test]
    async fn undelete_at_the_i64_ceiling_saturates_both_counters() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let raw_group_id = vec![0x7du8; 20];
        let channel = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        assert!(
            db.set_folder_mls_group("shared", &owner, Some(&raw_group_id))
                .await
                .unwrap()
        );
        db.set_folder_member_access(&channel, &writer, "writer", None)
            .await
            .unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let p: [u8; 32] = *blake3::hash(b"doc.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let big = i64::MAX - 1_000;
        let record = |manifest: [u8; 32], size: i64| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &writer,
                    &owner,
                    Some(&channel),
                    &p,
                    Some(&manifest),
                    size,
                    "modify",
                    fs_id,
                    &dev,
                    Some("doc.txt"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    i64::MAX,
                )
                .await
            }
        };
        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .expect("the admin user listing must stay readable")
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }
        let member_used = || {
            let db = &db;
            async move {
                db.get_folder_member_role(&channel, &writer)
                    .await
                    .expect("the member role row must stay readable")
                    .unwrap()
                    .bytes_used
            }
        };

        // A is admitted exactly up to the ceiling; a newer head fills it.
        let a = record([0xA1u8; 32], big).await.unwrap();
        record([0xA2u8; 32], 1_000).await.unwrap();
        assert_eq!(used(&db, owner).await, i64::MAX);
        assert_eq!(member_used().await, i64::MAX);

        // Prune A (the credit), then refill the freed headroom.
        assert!(db.soft_prune_version(a).await.unwrap());
        assert_eq!(used(&db, owner).await, 1_000);
        record([0xA3u8; 32], big).await.unwrap();
        assert_eq!(used(&db, owner).await, i64::MAX);
        assert_eq!(member_used().await, i64::MAX);

        // Undelete A: never refused, and both counters saturate as INTEGERs.
        assert!(
            db.undelete_version(a).await.unwrap(),
            "undelete never refuses"
        );
        assert_eq!(
            used(&db, owner).await,
            i64::MAX,
            "the owner's re-charge saturates"
        );
        assert_eq!(
            member_used().await,
            i64::MAX,
            "the member's re-charge saturates"
        );
    }

    /// A record's member half is bounded by no `checked_add` when
    /// the recorder is an uncapped member (`byte_cap` NULL, the default share)
    /// and the record is a same-manifest transfer: the owner is asked only
    /// about the net move (zero) while the member takes the whole size. Once
    /// an undelete has saturated that member's counter at `i64::MAX` (its
    /// re-charge never refuses), the transfer's bare `bytes_used + size`
    /// overflowed SQLite's integer arithmetic, the column turned REAL, and the
    /// member's role read and the folder's roster read both failed. Every
    /// counter move in `apply_charge_in_conn` saturates instead; the column's
    /// `typeof` is read back directly, not only through an `i64` decode.
    ///
    /// Red-verifiable: put back the bare `bytes_used + ?1` in
    /// `apply_charge_in_conn` and the member reads below fail.
    #[tokio::test]
    async fn a_transfer_onto_a_saturated_uncapped_member_saturates() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let raw_group_id = vec![0x7du8; 20];
        let channel = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        assert!(
            db.set_folder_mls_group("shared", &owner, Some(&raw_group_id))
                .await
                .unwrap()
        );
        db.set_folder_member_access(&channel, &writer, "writer", None)
            .await
            .unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let doc: [u8; 32] = *blake3::hash(b"doc.txt").as_bytes();
        let other: [u8; 32] = *blake3::hash(b"other.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let big = i64::MAX - 1_000;
        let record = |recorder: [u8; 32],
                      member_channel: Option<[u8; 32]>,
                      path: [u8; 32],
                      manifest: [u8; 32],
                      size: i64| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &recorder,
                    &owner,
                    member_channel.as_ref(),
                    &path,
                    Some(&manifest),
                    size,
                    "modify",
                    fs_id,
                    &dev,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    i64::MAX,
                )
                .await
            }
        };
        let typeof_counters = || {
            let db = &db;
            async move {
                let conn = db.conn.lock().await;
                let owner_type: String = conn
                    .query_row(
                        "SELECT typeof(storage_bytes_used) FROM users WHERE actor_id = ?1",
                        rusqlite::params![owner.as_slice()],
                        |r| r.get(0),
                    )
                    .unwrap();
                let member_type: String = conn
                    .query_row(
                        "SELECT typeof(bytes_used) FROM folder_member_access
                         WHERE channel_id = ?1 AND actor_id = ?2",
                        rusqlite::params![channel.as_slice(), writer.as_slice()],
                        |r| r.get(0),
                    )
                    .unwrap();
                (owner_type, member_type)
            }
        };

        // The owner holds the charge of other.txt @ Y.
        record(owner, None, other, [0x59u8; 32], 10).await.unwrap();
        // The member saturates their counter: record A, a newer head, prune A
        // (the credit), refill, undelete A (the never-refused re-charge).
        let a = record(writer, Some(channel), doc, [0xA1u8; 32], big)
            .await
            .unwrap();
        record(writer, Some(channel), doc, [0xA2u8; 32], 10)
            .await
            .unwrap();
        assert!(db.soft_prune_version(a).await.unwrap());
        record(writer, Some(channel), doc, [0xA3u8; 32], big)
            .await
            .unwrap();
        assert!(db.undelete_version(a).await.unwrap());

        // The net-zero transfer: the member re-records the owner's manifest.
        record(writer, Some(channel), other, [0x59u8; 32], 10)
            .await
            .expect("a net-zero transfer is never refused");
        assert_eq!(
            typeof_counters().await,
            ("integer".to_string(), "integer".to_string()),
            "both counters stay in SQLite's integer domain"
        );
        assert_eq!(
            db.get_folder_member_role(&channel, &writer)
                .await
                .expect("the member role row must stay readable")
                .unwrap()
                .bytes_used,
            i64::MAX,
            "the member's transfer charge saturates"
        );
        assert!(
            db.list_folder_member_access(&channel).await.is_ok(),
            "the folder roster must stay readable"
        );
    }

    /// The owner-driven M2 supersede releases exactly the rows it newly
    /// terminates: a row the pipeline already soft-pruned (already credited)
    /// is marked terminal but NOT credited again, and the later GC purge of a
    /// soft-pruned row moves the meter not at all.
    #[tokio::test]
    async fn the_owner_supersede_credits_only_unreleased_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("mine", &owner).await.unwrap();
        let fs = db.get_folder("mine").await.unwrap().unwrap();
        let p: [u8; 32] = *blake3::hash(b"doc.txt").as_bytes();
        let dev = [0xD1u8; 32];
        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }
        let record = |manifest: [u8; 32]| {
            let db = &db;
            let fs_id = fs.id;
            async move {
                db.record_sync_change_metered(
                    &owner,
                    &owner,
                    None,
                    &p,
                    Some(&manifest),
                    600,
                    "create",
                    fs_id,
                    &dev,
                    Some("doc.txt"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    i64::MAX,
                )
                .await
            }
        };
        let v1 = record([0xA1u8; 32]).await.unwrap();
        let _v2 = record([0xA2u8; 32]).await.unwrap();
        let head = [0xA3u8; 32];
        let _v3 = record(head).await.unwrap();
        assert_eq!(used(&db, owner).await, 1800);

        assert!(db.soft_prune_version(v1).await.unwrap());
        assert_eq!(used(&db, owner).await, 1200, "v1 released by the pipeline");

        // The owner's floorless privacy instrument marks BOTH pre-head rows
        // terminal, but only v2 was still charged — one credit, not two.
        let outcome = db
            .supersede_sync_changes_for_path(fs.id, &p, &head)
            .await
            .unwrap();
        match outcome {
            SupersedeOutcome::Marked(n) => assert_eq!(n, 2, "both pre-head rows terminate"),
            other => panic!("expected Marked, got {other:?}"),
        }
        assert_eq!(
            used(&db, owner).await,
            600,
            "only the unreleased row credits"
        );
    }

    /// A **negative** declared `size_bytes` is refused here, in the one metering
    /// core every record door funnels through — because the declaration IS the
    /// meter on this plane (`admin.md` § 2 Users: the tier *is* the quota, and
    /// `users.storage_bytes_used` is what enforces it), and a negative one
    /// unmakes it twice over: under retained accounting the charge IS the
    /// declared size, so it slips past the `charge > 0` ceiling check
    /// unexamined, and then *credits* the owner's
    /// account. One record with a large negative size floors
    /// `storage_bytes_used` to 0 and the tier is free again — unbounded, at the
    /// cost of one RPC. The sibling door `fauna.index.record` has always
    /// refused a non-positive `size_bytes` (`content_index_handlers.rs`); this
    /// is the same refusal, placed where all five record doors share it.
    /// The mirror of the negative-size refusal, at the TOP of the range. Sweep
    /// 168 bounded the declared `size_bytes` from below; nothing bounded it from
    /// above, and the owner ceiling check summed `used + charge` unchecked. A
    /// declared size near `i64::MAX` overflows that sum -- a panic in this
    /// (debug) profile, but a silent WRAP to a negative value in the shipped
    /// release profile, which then passes `> max_storage_bytes` and lets the
    /// record through. The charge that follows overflows SQLite's integer
    /// arithmetic as well, flipping `users.storage_bytes_used` to REAL, after
    /// which every `i64` read of the column fails -- the owner's record plane
    /// AND the nest-wide admin user listing, unrecoverable from any client.
    ///
    /// Cross-principal: the ceiling keys on the OWNER's row whoever records, so
    /// a granted member of a shared folder could brick the folder owner.
    ///
    /// Red-verifiable: restore the bare `used + charge` and this test panics
    /// with "attempt to add with overflow" instead of seeing a typed refusal.
    #[tokio::test]
    async fn an_overflowing_declared_size_is_refused_as_over_the_ceiling() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x37u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("photos", &owner).await.unwrap();
        let fs = db.get_folder("photos").await.unwrap().unwrap();
        let dev = [0xD3u8; 32];
        let max = 1_000i64;

        // A non-zero baseline, so `used + charge` genuinely overflows.
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            blake3::hash(b"a.txt").as_bytes(),
            Some(&[0xB1u8; 32]),
            1_000,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            max,
        )
        .await
        .unwrap();

        let err = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                blake3::hash(b"huge.txt").as_bytes(),
                Some(&[0xB9u8; 32]),
                i64::MAX,
                "create",
                fs.id,
                &dev,
                Some("huge.txt"),
                None,
                None,
                None,
                None,
                None,
                max,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                StorageQuotaError::Exceeded {
                    requested: i64::MAX,
                    ..
                }
            ),
            "an unrepresentable total is refused as over the ceiling: {err}"
        );

        // Nothing was charged, and -- the half that makes this unrecoverable --
        // the column is still an INTEGER every later read can parse.
        let users = db
            .list_users()
            .await
            .expect("the admin user listing must still be readable for EVERY user");
        let u = users
            .into_iter()
            .find(|u| u.actor_id == owner.to_vec())
            .unwrap();
        assert_eq!(
            u.storage_bytes_used, 1_000,
            "a refused record charges nothing"
        );
    }

    #[tokio::test]
    async fn a_negative_declared_size_is_refused_and_credits_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x33u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("photos", &owner).await.unwrap();
        let fs = db.get_folder("photos").await.unwrap().unwrap();
        let dev = [0xD2u8; 32];
        let max = 1_000i64;

        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }

        // Fill the tier honestly.
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            blake3::hash(b"a.txt").as_bytes(),
            Some(&[0xB1u8; 32]),
            1_000,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            max,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 1_000, "the tier is now full");

        // The evasion: a fresh path declared at a large negative size.
        let err = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                blake3::hash(b"free.txt").as_bytes(),
                Some(&[0xB2u8; 32]),
                -1_000_000,
                "create",
                fs.id,
                &dev,
                Some("free.txt"),
                None,
                None,
                None,
                None,
                None,
                max,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                StorageQuotaError::NegativeSize {
                    size_bytes: -1_000_000
                }
            ),
            "a negative declared size is refused typed, not metered: {err}"
        );

        // Refused before any write: the account is untouched…
        assert_eq!(
            used(&db, owner).await,
            1_000,
            "a refused record credits nothing"
        );
        // …no row landed…
        assert!(
            db.get_sync_changes(&owner, 0)
                .await
                .unwrap()
                .iter()
                .all(|c| c.size_bytes >= 0),
            "no negative-size row was inserted"
        );
        // …and the tier still binds, which is the whole point.
        let still_bound = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                blake3::hash(b"b.txt").as_bytes(),
                Some(&[0xB3u8; 32]),
                1,
                "create",
                fs.id,
                &dev,
                Some("b.txt"),
                None,
                None,
                None,
                None,
                None,
                max,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(still_bound, StorageQuotaError::Exceeded { .. }),
            "the quota still refuses one more byte: {still_bound}"
        );
    }

    /// Create → delete → recreate of the SAME path, under retained accounting
    /// (`file-versions.md` § Retention (4)): the delete is net zero (the former
    /// head stays listable and keeps charging), and the recreate charges its
    /// own full size on top — so the cycle ratchets both counters instead of
    /// evading the quota. (Historically this guarded the delete-recreate
    /// zero-delta hole; retained accounting closes that class by construction —
    /// no record ever reads a prior for its charge.)
    #[tokio::test]
    async fn writer_record_meters_delete_then_recreate_of_same_path() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let channel = [0xCCu8; 32];
        db.set_folder_member_access(&channel, &writer, "writer", Some(10_000))
            .await
            .unwrap();

        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }
        async fn member_used(db: &CacheDb, channel: &[u8; 32], writer: &[u8; 32]) -> i64 {
            db.get_folder_member_role(channel, writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used
        }

        let p: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let dev = [0xD1u8; 32];

        // create(600) → owner charged, member counter bumps.
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &p,
            Some(&[0xA1u8; 32]),
            600,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 600, "create charges the owner");
        assert_eq!(member_used(&db, &channel, &writer).await, 600);

        // delete → both counters credited back to 0.
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &p,
            None,
            0,
            "delete",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 600, "delete is net zero (owner)");
        assert_eq!(member_used(&db, &channel, &writer).await, 600);

        // recreate(600) of the SAME path charges its own size on top of the
        // retained lap — 600 + 600 on both counters.
        db.record_sync_change_metered(
            &writer,
            &owner,
            Some(&channel),
            &p,
            Some(&[0xA5u8; 32]),
            600,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(
            used(&db, owner).await,
            1200,
            "recreate charges on top of the retained lap (owner)"
        );
        assert_eq!(
            member_used(&db, &channel, &writer).await,
            1200,
            "recreate charges on top of the retained lap (member)"
        );
    }

    /// A superseded row must never resurrect as the "prior live size". Metering
    /// reads the newest row per path (`seq DESC LIMIT 1`), and
    /// `supersede_sync_changes_for_path` only marks rows strictly below the head,
    /// so the head is never superseded. A create → modify → supersede → delete →
    /// recreate cycle stays exactly honest at every step.
    #[tokio::test]
    async fn writer_record_metering_never_reads_a_superseded_row_as_prior() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();

        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }

        let p: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let m1 = [0xA1u8; 32];
        let m2 = [0xA2u8; 32];

        // Owner records on their own data plane (no member channel).
        // create(600, m1).
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &p,
            Some(&m1),
            600,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 600);

        // modify(1000, m2) → charges its full size; the m1 row is now a
        // retained version and keeps charging (600 + 1000).
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &p,
            Some(&m2),
            1000,
            "modify",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(used(&db, owner).await, 1600, "head + retained both charge");

        // Compact: supersede everything below the m2 head — the release point
        // that credits the retained m1 back (1600 → 1000).
        db.supersede_sync_changes_for_path(fs.id, &p, &m2)
            .await
            .unwrap();
        assert_eq!(
            used(&db, owner).await,
            1000,
            "the supersede releases the retained m1's 600"
        );

        // delete → net zero: the m2 head stays listable and keeps charging.
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &p,
            None,
            0,
            "delete",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(
            used(&db, owner).await,
            1000,
            "delete is net zero — the m2 version keeps charging; the superseded \
             m1 never resurrects into the charge"
        );

        // recreate(700) charges its own size on top of the retained m2.
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &p,
            Some(&[0xA3u8; 32]),
            700,
            "create",
            fs.id,
            &dev,
            Some("a.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(
            used(&db, owner).await,
            1700,
            "recreate charges 700 on top of the retained 1000"
        );
    }

    /// **Exactly-once by CONTENT, not by key** (`federation.md` § Cross-nest…,
    /// contract point (i)). The L3 idempotency cache is
    /// per-connection, 1000-entry and 5-min-TTL'd, so it cannot span a
    /// reconnect, a >TTL redelivery, or a crashed-ack retry — and the federated
    /// record relay must not lean on it. The durable guarantee is here instead:
    /// a record whose `(acting actor, path_hash, manifest_hash, change_type,
    /// content_key_version)` already equals the path's LATEST entry is
    /// already-applied — it returns that row's `seq` and charges nothing.
    ///
    /// Same-nest and federated records share this one implementation (uniform
    /// shape — a same-nest retry double-charge is the same bug unfired), which
    /// is why this test drives the owner's own plane too.
    #[tokio::test]
    async fn record_is_content_idempotent_and_a_replay_charges_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let writer = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let channel = [0xCCu8; 32];
        db.set_folder_member_access(&channel, &writer, "writer", Some(10_000))
            .await
            .unwrap();

        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }
        async fn rows(db: &CacheDb, folder_id: i64) -> usize {
            db.get_sync_changes_for_folder(folder_id, 0, None)
                .await
                .unwrap()
                .len()
        }

        let p: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let dev = [0xD1u8; 32];
        let m1 = [0xA1u8; 32];

        // The writer's first record lands: owner pays 600, member counter 600.
        let seq1 = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                Some(&m1),
                600,
                "create",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(used(&db, owner).await, 600);
        assert_eq!(rows(&db, fs.id).await, 1);

        // ── The replay: byte-identical on all five key fields. It must return
        // the SAME seq, append NO row, and charge NEITHER counter. ──
        let seq_replay = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                Some(&m1),
                600,
                "create",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(
            seq_replay, seq1,
            "a replayed record returns the original seq, not a fresh one"
        );
        assert_eq!(
            rows(&db, fs.id).await,
            1,
            "the replay appended no second row to the change log"
        );
        assert_eq!(
            used(&db, owner).await,
            600,
            "the replay charged the owner's meter nothing"
        );
        assert_eq!(
            db.get_folder_member_role(&channel, &writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used,
            600,
            "the replay charged the member's abuse counter nothing"
        );

        // A genuinely new manifest for the same path is NOT a replay — it
        // lands and charges its full size (600 retained + 1000).
        let seq2 = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                Some(&[0xA2u8; 32]),
                1000,
                "modify",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_ne!(seq2, seq1, "different content is a genuine new version");
        assert_eq!(rows(&db, fs.id).await, 2);
        assert_eq!(used(&db, owner).await, 1600, "charged exactly once");

        // Dedupe keys on the ACTING ACTOR: the owner recording content identical
        // to the writer's head is a distinct authored version, not a replay —
        // attribution differs, and version history must show it. It adds no
        // bytes, though, so it TAKES the head's charge rather than adding one
        // (`writer-signed-change-records.md` ruling (11)(g)).
        let seq_owner = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                &p,
                Some(&[0xA2u8; 32]),
                1000,
                "modify",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_ne!(
            seq_owner, seq2,
            "a different acting actor is never deduped against another's row"
        );
        assert_eq!(rows(&db, fs.id).await, 3);
        assert_eq!(
            used(&db, owner).await,
            1600,
            "a distinct authored version of identical bytes takes the head's charge"
        );

        // The owner's own plane dedupes identically (no member channel).
        let seq_owner_replay = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                &p,
                Some(&[0xA2u8; 32]),
                1000,
                "modify",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(
            seq_owner_replay, seq_owner,
            "the same-nest owner path gets the identical dedupe"
        );
        assert_eq!(rows(&db, fs.id).await, 3, "no fourth row");

        // A replayed DELETE is deduped too — the tombstone is already the head.
        let del = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                None,
                0,
                "delete",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(
            used(&db, owner).await,
            1600,
            "the delete is net zero — every retained version keeps charging"
        );
        let del_replay = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                None,
                0,
                "delete",
                fs.id,
                &dev,
                Some("a.txt"),
                Some(3),
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(del_replay, del, "a replayed delete returns the same seq");
        assert_eq!(
            rows(&db, fs.id).await,
            4,
            "the replayed delete added no row"
        );
        assert_eq!(
            used(&db, owner).await,
            1600,
            "and the replayed delete moves the meter not at all"
        );
    }

    /// row 9 — nest-side idempotency guard for a repeated
    /// delete of an already-tombstoned path. The exactly-once check above
    /// dedupes by (acting actor, manifest, change_type, content_key_version)
    /// — deliberately keyed on the ACTOR for creates/modifies, because two
    /// actors authoring identical *content* are still distinct versions. A
    /// delete carries no content to differ by author: two actors deleting the
    /// same already-gone path converge on the identical "nothing here" state,
    /// so an echo-delete from a DIFFERENT actor than the one who tombstoned
    /// the path must no-op exactly like a same-actor replay — never append a
    /// second tombstone row that would re-propagate an already-applied delete
    /// to every other device (`delete-propagation.md` § Deletes propagate the
    /// same way, guard (2)).
    #[tokio::test]
    async fn record_no_ops_a_cross_actor_delete_of_an_already_tombstoned_path() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x41u8; 32];
        let writer = [0x42u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        db.create_folder("shared", &owner).await.unwrap();
        let fs = db.get_folder("shared").await.unwrap().unwrap();
        let channel = [0xEEu8; 32];
        db.set_folder_member_access(&channel, &writer, "writer", Some(10_000))
            .await
            .unwrap();

        async fn rows(db: &CacheDb, folder_id: i64) -> usize {
            db.get_sync_changes_for_folder(folder_id, 0, None)
                .await
                .unwrap()
                .len()
        }
        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.list_users()
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.actor_id == actor.to_vec())
                .unwrap()
                .storage_bytes_used
        }

        let p: [u8; 32] = *blake3::hash(b"b.txt").as_bytes();
        let dev_owner = [0xF1u8; 32];
        let dev_writer = [0xF2u8; 32];

        // The owner creates, then deletes — the path's head is now a
        // tombstone recorded by the OWNER.
        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &p,
            Some(&[0xB1u8; 32]),
            500,
            "create",
            fs.id,
            &dev_owner,
            Some("b.txt"),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();
        let delete_seq = db
            .record_sync_change_metered(
                &owner,
                &owner,
                None,
                &p,
                None,
                0,
                "delete",
                fs.id,
                &dev_owner,
                Some("b.txt"),
                None,
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(rows(&db, fs.id).await, 2);
        assert_eq!(
            used(&db, owner).await,
            500,
            "the delete is net zero — the tombstoned head's version keeps charging"
        );

        // A DIFFERENT actor — the granted writer, on its own device — echoes a
        // delete of the same already-tombstoned path (e.g. a stale client
        // that had not yet learned the delete already landed). Cross-actor,
        // so the exactly-once same-actor check above does not fire; this
        // guard must.
        let echo_seq = db
            .record_sync_change_metered(
                &writer,
                &owner,
                Some(&channel),
                &p,
                None,
                0,
                "delete",
                fs.id,
                &dev_writer,
                Some("b.txt"),
                None,
                None,
                None,
                None,
                None,
                i64::MAX,
            )
            .await
            .unwrap();

        assert_eq!(
            echo_seq, delete_seq,
            "a cross-actor echo-delete of an already-tombstoned path returns \
             the existing tombstone's seq, not a fresh one"
        );
        assert_eq!(
            rows(&db, fs.id).await,
            2,
            "the echo-delete appended no new row to the change log"
        );
        assert_eq!(
            used(&db, owner).await,
            500,
            "the echo-delete charged the owner's meter nothing"
        );
        assert_eq!(
            db.get_folder_member_role(&channel, &writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used,
            0,
            "the echo-delete charged the member's abuse counter nothing"
        );
    }

    /// The content-key version floor is monotonic (D2/F4): `put` only ever
    /// raises it (`MAX(stored, new)`).
    #[tokio::test]
    async fn content_key_floor_is_monotonic() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0xCCu8; 32];

        // No envelope → no floor.
        assert_eq!(
            db.get_folder_content_key_floor(&channel).await.unwrap(),
            None
        );

        // Stamped publish sets the floor.
        db.upsert_folder_content_key(&channel, 2, b"sealed-2", 3)
            .await
            .unwrap();
        assert_eq!(
            db.get_folder_content_key_floor(&channel).await.unwrap(),
            Some(3)
        );

        // A LOWER stamp never lowers it (monotonic MAX).
        db.upsert_folder_content_key(&channel, 4, b"sealed-4", 2)
            .await
            .unwrap();
        assert_eq!(
            db.get_folder_content_key_floor(&channel).await.unwrap(),
            Some(3),
            "a lower stamp never lowers the floor"
        );

        // A higher stamp raises it.
        db.upsert_folder_content_key(&channel, 5, b"sealed-5", 7)
            .await
            .unwrap();
        assert_eq!(
            db.get_folder_content_key_floor(&channel).await.unwrap(),
            Some(7)
        );
    }

    /// Bind `name` (owned by `owner`) to a fresh channel and register `member` on
    /// its roster + a reader role + a published content-key envelope — the
    /// post-`share` + `welcome.deliver` state a real multi-member shared set
    /// reaches. Returns the derived 32-byte ChannelId.
    async fn bind_shared_channel_with_member(
        db: &CacheDb,
        name: &str,
        owner: &[u8; 32],
        member: &[u8; 32],
    ) -> [u8; 32] {
        let raw_group_id = vec![0x7cu8; 20];
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw_group_id).0;
        db.create_folder(name, owner).await.unwrap();
        db.claim_folder_channel(owner, &channel_id).await.unwrap();
        assert!(
            db.set_folder_mls_group(name, owner, Some(&raw_group_id))
                .await
                .unwrap()
        );
        db.register_actor_channel(member, &channel_id)
            .await
            .unwrap();
        db.set_folder_member_access(&channel_id, member, "reader", None)
            .await
            .unwrap();
        db.upsert_folder_content_key(&channel_id, 1, b"sealed-envelope", 1)
            .await
            .unwrap();
        channel_id
    }

    /// Deleting the claimant's account must not strand every OTHER member of
    /// a shared folder channel: once the owner's
    /// `folders` row is gone, nobody can ever again `content_key.put` /
    /// `members.evict` / post an MLS Commit on it (both gates require
    /// caller == claimant), so leaving the claim + a member's roster row +
    /// their role + the stale envelope behind would freeze `member` into a
    /// contentless channel forever, with no owner left to evict them. The
    /// fix tears the WHOLE channel down for every actor on it, not just the
    /// owner's own rows.
    #[tokio::test]
    async fn deleting_the_claimants_account_does_not_strand_other_members() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let member = [0x22u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&member, "free", "member").await.unwrap();
        let channel_id = bind_shared_channel_with_member(&db, "shared", &owner, &member).await;

        // Sanity: the pre-deletion state a real share reaches.
        assert_eq!(
            db.list_channel_actors(&channel_id).await.unwrap().len(),
            2,
            "owner + member are both on the roster"
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            Some(owner)
        );
        assert!(
            db.get_folder_member_role(&channel_id, &member)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_folder_content_key(&channel_id)
                .await
                .unwrap()
                .is_some()
        );

        // The owner's whole account is deleted (finalize_user_deletion's
        // reclaim step).
        let removed = db.delete_all_folders_for_actor(&owner).await.unwrap();
        assert_eq!(removed, 1);

        // The dead-owner claim is gone (nobody could ever satisfy it again).
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            None,
            "the claim on a now-ownerless channel is released, not left dangling"
        );
        // `member`'s own roster row — untouched by a PURGE keyed on the
        // OWNER's actor_id — must still be torn down: this is the assertion
        // that catches the stranding bug.
        assert!(
            db.list_channel_actors(&channel_id)
                .await
                .unwrap()
                .is_empty(),
            "every member's roster row is gone, not just the deleted owner's"
        );
        assert!(
            db.get_folder_member_role(&channel_id, &member)
                .await
                .unwrap()
                .is_none(),
            "the member's access role dies with the channel"
        );
        assert!(
            db.get_folder_content_key(&channel_id)
                .await
                .unwrap()
                .is_none(),
            "the stale content-key envelope is cleared, not left as fetchable residue"
        );
    }

    /// The same cascade fires on the single-set `fauna.folders.delete` path
    /// (`delete_folder_for_user`), not just full-account deletion — both
    /// share `delete_folder_rows_in_tx`, and an owner deleting one shared
    /// set while their account survives strands members exactly the same way.
    #[tokio::test]
    async fn deleting_one_shared_set_does_not_strand_other_members() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x33u8; 32];
        let member = [0x44u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&member, "free", "member").await.unwrap();
        let channel_id = bind_shared_channel_with_member(&db, "shared", &owner, &member).await;

        assert!(db.delete_folder_for_user("shared", &owner).await.unwrap());

        assert!(
            db.list_channel_actors(&channel_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel_id).await.unwrap(),
            None
        );
        assert!(
            db.get_folder_member_role(&channel_id, &member)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_folder_content_key(&channel_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// M2 pre-bind re-seal reclaim (Piece B): superseding a path marks exactly
    /// the rows strictly below the verified head, drops them from the GC pin
    /// set and the changes feed, and is idempotent.
    #[tokio::test]
    async fn supersede_marks_only_pre_head_rows_and_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        db.create_folder("reseal", &owner).await.unwrap();
        let fs = db.get_folder("reseal").await.unwrap().unwrap();

        let p: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let q: [u8; 32] = *blake3::hash(b"b.txt").as_bytes();
        let (m1, m2, m3, mq) = ([0xA1u8; 32], [0xA2u8; 32], [0xA3u8; 32], [0xB1u8; 32]);
        for (ph, mh, path) in [
            (&p, &m1, "a.txt"),
            (&p, &m2, "a.txt"),
            (&p, &m3, "a.txt"),
            (&q, &mq, "b.txt"),
        ] {
            db.record_sync_change(
                &owner,
                ph,
                Some(mh),
                10,
                "modify",
                Some(fs.id),
                None,
                Some(path),
            )
            .await
            .unwrap();
        }

        assert_eq!(
            db.supersede_sync_changes_for_path(fs.id, &p, &m3)
                .await
                .unwrap(),
            SupersedeOutcome::Marked(2),
            "exactly the two pre-head rows are marked"
        );

        // GC pin set: the superseded manifests drop out; head + sibling stay.
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(!pins.contains(&m1.to_vec()) && !pins.contains(&m2.to_vec()));
        assert!(pins.contains(&m3.to_vec()) && pins.contains(&mq.to_vec()));

        // Feed: a catching-up device no longer sees the superseded rows (their
        // blobs may already be reclaimed — it must never attempt those fetches).
        let feed = db
            .get_sync_changes_for_folder(fs.id, 0, None)
            .await
            .unwrap();
        let manifests: Vec<_> = feed
            .iter()
            .filter_map(|c| c.manifest_hash.clone())
            .collect();
        assert_eq!(manifests, vec![m3.to_vec(), mq.to_vec()]);

        // Idempotent re-run marks nothing new.
        assert_eq!(
            db.supersede_sync_changes_for_path(fs.id, &p, &m3)
                .await
                .unwrap(),
            SupersedeOutcome::Marked(0),
        );
    }

    /// The head is structurally unmarkable: a supersede naming anything but the
    /// path's live head (a stale verify, a wrong hash, an unknown path) refuses
    /// with HeadMismatch and mutates nothing.
    #[tokio::test]
    async fn supersede_refuses_non_head_manifest_and_unknown_path() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x22u8; 32];
        db.create_folder("guard", &owner).await.unwrap();
        let fs = db.get_folder("guard").await.unwrap().unwrap();

        let p: [u8; 32] = *blake3::hash(b"f.bin").as_bytes();
        let (m_old, m_head) = ([0xC1u8; 32], [0xC2u8; 32]);
        for mh in [&m_old, &m_head] {
            db.record_sync_change(
                &owner,
                &p,
                Some(mh),
                10,
                "modify",
                Some(fs.id),
                None,
                Some("f.bin"),
            )
            .await
            .unwrap();
        }

        // Naming the OLD manifest (not the head) refuses.
        assert_eq!(
            db.supersede_sync_changes_for_path(fs.id, &p, &m_old)
                .await
                .unwrap(),
            SupersedeOutcome::HeadMismatch,
        );
        // Unknown path refuses.
        let unknown: [u8; 32] = *blake3::hash(b"nope").as_bytes();
        assert_eq!(
            db.supersede_sync_changes_for_path(fs.id, &unknown, &m_head)
                .await
                .unwrap(),
            SupersedeOutcome::HeadMismatch,
        );
        // Nothing was marked: both manifests still pinned.
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(pins.contains(&m_old.to_vec()) && pins.contains(&m_head.to_vec()));
    }

    // ── The version-retention prune pipeline (file-versions.md § Retention (3)) ──
    //
    // Every one of these is a no-user-data-loss assertion: read them as "what
    // the pipeline is forbidden to lose", not as column plumbing.

    /// Record `n` versions of one path in `name`'s set; returns (folder, seqs).
    /// `path` names the file — hashed for `path_hash`, so two calls against
    /// the same `name` with different `path`s record two independent version
    /// histories in one set (a multi-path fixture is just two calls;
    /// `create_folder` is skipped when the set already exists).
    async fn folder_with_versions(
        db: &CacheDb,
        name: &str,
        path: &str,
        owner: &[u8; 32],
        n: usize,
    ) -> (FolderRow, [u8; 32], Vec<i64>) {
        if db.get_folder(name).await.unwrap().is_none() {
            db.create_folder(name, owner).await.unwrap();
        }
        let fs = db.get_folder(name).await.unwrap().unwrap();
        let p: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let mut seqs = Vec::new();
        for i in 0..n {
            let mh = [0xE0u8 + i as u8; 32];
            seqs.push(
                db.record_sync_change(
                    owner,
                    &p,
                    Some(&mh),
                    100,
                    "modify",
                    Some(fs.id),
                    None,
                    Some(path),
                )
                .await
                .unwrap(),
            );
        }
        (fs, p, seqs)
    }

    /// Layer 3 in both directions: a soft-pruned version leaves the DEFAULT
    /// projection but rides the `include_pruned` recovery browse with its
    /// markers, stays servable by `get` (restore-during-window), stays
    /// GC-pinned — and `undelete` restores it fully, Layer 1-style.
    #[tokio::test]
    async fn soft_prune_hides_undelete_restores_and_the_pin_never_lapses() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x55u8; 32];
        let (fs, p, seqs) = folder_with_versions(&db, "vp-lifecycle", "v.txt", &owner, 3).await;

        assert!(db.soft_prune_version(seqs[0]).await.unwrap());

        // Default projection: gone. Recovery browse: present, marked.
        let listed = db
            .list_file_versions_in_sets(&[fs.id], &p, false)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|v| v.version_num).collect::<Vec<_>>(),
            &seqs[1..],
            "the soft-pruned version leaves the default projection"
        );
        let browsed = db
            .list_file_versions_in_sets(&[fs.id], &p, true)
            .await
            .unwrap();
        let pruned_row = browsed.iter().find(|v| v.version_num == seqs[0]).unwrap();
        assert!(pruned_row.pruned_at.is_some() && pruned_row.purge_after.is_some());
        assert!(browsed.len() == 3, "the browse is a superset, not a swap");

        // `get` still serves it — restore-as-re-point reads the manifest here.
        assert!(
            db.get_file_version_by_seq(&p, seqs[0])
                .await
                .unwrap()
                .is_some(),
            "a soft-pruned version stays fetchable for restore during its window"
        );

        // GC: `superseded_at` is still NULL, so the pin holds (the release is
        // the PURGE step's, never the soft-prune's).
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(
            pins.contains(&[0xE0u8; 32].to_vec()),
            "soft-pruned ⇒ still pinned"
        );

        // Undelete: fully back — listable, unmarked; a second undelete is a
        // no-op the handler maps to not_found.
        assert!(db.undelete_version(seqs[0]).await.unwrap());
        let relisted = db
            .list_file_versions_in_sets(&[fs.id], &p, false)
            .await
            .unwrap();
        assert_eq!(
            relisted.len(),
            3,
            "undelete restores the listable population"
        );
        assert!(
            relisted
                .iter()
                .all(|v| v.pruned_at.is_none() && v.purge_after.is_none())
        );
        assert!(!db.undelete_version(seqs[0]).await.unwrap());
    }

    /// The purge step: only past `purge_after` does `superseded_at` stamp (the
    /// terminal reclaim mark the pin predicate releases one grace later) — and
    /// the newer-row-exists guard skips, loudly counts, and keeps recoverable a
    /// row whose path history has a shape the pipeline never produces.
    #[tokio::test]
    async fn purge_stamps_superseded_only_past_the_window_and_the_guard_over_retains() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x56u8; 32];
        let (_fs, _p, seqs) = folder_with_versions(&db, "vp-purge", "v.txt", &owner, 3).await;

        assert!(db.soft_prune_version(seqs[0]).await.unwrap());

        // Window still open: purge touches nothing.
        assert_eq!(
            db.purge_expired_soft_pruned_versions().await.unwrap(),
            (0, 0)
        );

        // Window elapsed: the row is stamped superseded and drops off the pin
        // set once the stamp is older than the cutoff (next cycle's grace).
        db.set_version_purge_after(seqs[0], 0).await.unwrap();
        assert_eq!(
            db.purge_expired_soft_pruned_versions().await.unwrap(),
            (1, 0)
        );
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(
            !pins.contains(&[0xE0u8; 32].to_vec()),
            "past the grace cutoff the purged version's manifest is no longer pinned"
        );
        // Terminal: the purged row is not undeleteable (its chunks may be gone).
        assert!(!db.undelete_version(seqs[0]).await.unwrap());

        // The guard: soft-prune the path's NEWEST row (the evaluator never
        // does this — simulated directly), expire it, and the purge must skip
        // it, count it, and leave it recoverable. Over-retain, never over-delete.
        let head = *seqs.last().unwrap();
        assert!(db.soft_prune_version(head).await.unwrap());
        db.set_version_purge_after(head, 0).await.unwrap();
        assert_eq!(
            db.purge_expired_soft_pruned_versions().await.unwrap(),
            (0, 1),
            "a row with no newer sibling is guarded, not purged"
        );
        assert!(db.undelete_version(head).await.unwrap());
    }

    /// Restore-during-window safety (§ Retention (3), first build pin): a
    /// restore re-points the head at the pruned version's manifest via a NEW
    /// row, so the later purge of the old row never unpins chunks a live row
    /// references — GC reachability is row-set-wide.
    #[tokio::test]
    async fn a_purged_versions_manifest_stays_pinned_by_a_restore_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x57u8; 32];
        let (fs, p, seqs) = folder_with_versions(&db, "vp-restore", "v.txt", &owner, 3).await;
        let m_old = [0xE0u8; 32]; // seqs[0]'s manifest

        assert!(db.soft_prune_version(seqs[0]).await.unwrap());
        // The user restores the pruned version: an ordinary `modify` record
        // re-pointing the same manifest as the new head (file-sync.md § Restore).
        db.record_sync_change(
            &owner,
            &p,
            Some(&m_old),
            100,
            "modify",
            Some(fs.id),
            None,
            Some("v.txt"),
        )
        .await
        .unwrap();

        db.set_version_purge_after(seqs[0], 0).await.unwrap();
        assert_eq!(
            db.purge_expired_soft_pruned_versions().await.unwrap(),
            (1, 0)
        );
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(
            pins.contains(&m_old.to_vec()),
            "the restore row keeps the manifest pinned after the old row purges"
        );
    }

    /// The scheduler leg end-to-end: one 7-day `VersionBulkPrune` action per
    /// folder, targets marked `prune_pending` (still listable), idempotent
    /// while the window is open — and CANCEL releases the marks, so the rows
    /// re-enter the evaluator's population.
    #[tokio::test]
    async fn schedule_version_auto_prune_marks_cancel_releases() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let owner = [0x58u8; 32];
        let (_, p, seqs) = folder_with_versions(&db, "vp-schedule", "v.txt", &owner, 5).await;
        assert!(
            db.update_folder_for_user(
                "vp-schedule",
                &owner,
                FolderUpdate {
                    version_retention: Some(Some(
                        r#"{"max_versions_per_path":3,"max_age_days":0}"#
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        );
        // Re-read after the retention update — the pre-update handle would
        // schedule against a bound that is no longer set.
        let fs = db.get_folder("vp-schedule").await.unwrap().unwrap();

        // 5 versions under a bound of 3 ⇒ the oldest 2 schedule.
        let scheduled = crate::backup::version_prune::schedule_version_auto_prune(&db, &fs)
            .await
            .unwrap();
        assert_eq!(scheduled, 2);
        // Marked rows STAY in the default projection (the cancellable window
        // keeps them fully listable and restorable).
        assert_eq!(
            db.list_file_versions_in_sets(&[fs.id], &p, false)
                .await
                .unwrap()
                .len(),
            5
        );
        // Idempotent: the next cycle proposes nothing new.
        assert_eq!(
            crate::backup::version_prune::schedule_version_auto_prune(&db, &fs)
                .await
                .unwrap(),
            0
        );

        // Cancel is the user saying "keep my versions": the marks come off and
        // the rows re-enter the population (the next cycle re-proposes them —
        // policy still stands, but that next action is again cancellable).
        let actions: Vec<_> = db
            .list_pending_actions_for_actor(&owner)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.status == "pending")
            .collect();
        assert_eq!(actions.len(), 1, "one action per folder prune");
        assert_eq!(actions[0].action_type, "version.bulk_prune");
        db.cancel_pending_action(actions[0].id, &owner)
            .await
            .unwrap();
        assert_eq!(
            crate::backup::version_prune::schedule_version_auto_prune(&db, &fs)
                .await
                .unwrap(),
            2,
            "cancel released the marks — the rows are candidates again"
        );
        let _ = seqs;
    }

    /// `file-versions.md` § Retention ruling (2) — "evaluation is per
    /// `(folder, path)`" — over a folder that actually HAS two paths. Every
    /// other fixture in this file drives the scheduler with a single-path
    /// set, where grouped and ungrouped evaluation coincide by construction;
    /// this is the one test that can tell them apart. Two paths under one
    /// bound: each path's own oldest rows prune, each path's own head
    /// survives — never a count borrowed from, or a head sacrificed to, the
    /// other path's history.
    #[tokio::test]
    async fn schedule_version_auto_prune_bounds_each_path_independently() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let owner = [0x59u8; 32];
        // 5 versions on a.txt, 4 on b.txt, one set — folder_with_versions
        // skips create_folder the second time (same `name`, different `path`).
        let (_, _, a_seqs) = folder_with_versions(&db, "vp-multi-path", "a.txt", &owner, 5).await;
        let (fs, _, b_seqs) = folder_with_versions(&db, "vp-multi-path", "b.txt", &owner, 4).await;

        assert!(
            db.update_folder_for_user(
                "vp-multi-path",
                &owner,
                FolderUpdate {
                    version_retention: Some(Some(
                        r#"{"max_versions_per_path":3,"max_age_days":0}"#
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        );
        let fs = db.get_folder(&fs.name).await.unwrap().unwrap();

        // Grouped-correct math: a.txt (5, bound 3) prunes its oldest 2;
        // b.txt (4, bound 3) prunes its oldest 1 — 3 total, never either
        // path's newest (head) row.
        let scheduled = crate::backup::version_prune::schedule_version_auto_prune(&db, &fs)
            .await
            .unwrap();
        assert_eq!(scheduled, 3);

        let actions: Vec<_> = db
            .list_pending_actions_for_actor(&owner)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.status == "pending")
            .collect();
        assert_eq!(actions.len(), 1, "one action for the whole set");
        let payload: serde_json::Value =
            serde_json::from_str(actions[0].payload.as_deref().unwrap()).unwrap();
        let mut pruned: Vec<i64> = payload["version_seqs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        pruned.sort_unstable();
        let mut expected = vec![a_seqs[0], a_seqs[1], b_seqs[0]];
        expected.sort_unstable();
        assert_eq!(
            pruned, expected,
            "each path's own oldest rows, and nothing else"
        );
        assert!(
            !pruned.contains(a_seqs.last().unwrap()),
            "a.txt's head must never be scheduled"
        );
        assert!(
            !pruned.contains(b_seqs.last().unwrap()),
            "b.txt's head must never be scheduled"
        );
    }

    /// Cross-row safety: a manifest recorded under TWO paths stays GC-pinned by
    /// the other path's live row after one path supersedes it — the pin set is
    /// `DISTINCT` over the remaining live rows, never per-mark.
    #[tokio::test]
    async fn superseded_manifest_stays_pinned_by_another_live_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x33u8; 32];
        db.create_folder("shared-manifest", &owner).await.unwrap();
        let fs = db.get_folder("shared-manifest").await.unwrap().unwrap();

        let pa: [u8; 32] = *blake3::hash(b"a.dat").as_bytes();
        let pb: [u8; 32] = *blake3::hash(b"b.dat").as_bytes();
        let shared = [0xD1u8; 32];
        let new_a = [0xD2u8; 32];
        // The same manifest recorded under both paths (identical content).
        for (ph, path) in [(&pa, "a.dat"), (&pb, "b.dat")] {
            db.record_sync_change(
                &owner,
                ph,
                Some(&shared),
                10,
                "create",
                Some(fs.id),
                None,
                Some(path),
            )
            .await
            .unwrap();
        }
        // Path A re-records (re-seal) and supersedes its old row.
        db.record_sync_change(
            &owner,
            &pa,
            Some(&new_a),
            10,
            "modify",
            Some(fs.id),
            None,
            Some("a.dat"),
        )
        .await
        .unwrap();
        assert_eq!(
            db.supersede_sync_changes_for_path(fs.id, &pa, &new_a)
                .await
                .unwrap(),
            SupersedeOutcome::Marked(1),
        );

        // The shared manifest is still pinned via path B's live row.
        let pins = db.sync_change_manifest_hashes(i64::MAX).await.unwrap();
        assert!(
            pins.contains(&shared.to_vec()),
            "a manifest live under another path must stay GC-pinned"
        );
    }

    /// Two snapshot creates for the same folder within one wall-clock second
    /// collide on `UNIQUE(folder_id, created_at)`. The second must NOT surface a
    /// spurious `snapshot.internal` — it returns the existing snapshot, so a
    /// client's manual create racing the 60s auto-scheduler is idempotent
    /// (backup-restore.md § 1 Snapshot Creation).
    #[tokio::test]
    async fn create_snapshot_v2_same_second_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_folder("dup-sec", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("dup-sec").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"file.txt").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("file.txt"),
        )
        .await
        .unwrap();

        // The precondition — two creates landing in ONE wall-clock second — is
        // *arranged*, never assumed. `create_snapshot_v2` reads `now_epoch_secs()`
        // internally with no injection point, so a pair can straddle a second
        // boundary and yield distinct `created_at` -> distinct snapshots -> a
        // spurious failure of the dedup assertion. Aligning to the start of a
        // second first (the 2026-07-09 attempt) does not fix that: on a loaded box
        // the two calls can still be descheduled across the boundary, which is how
        // this test red a full `--lib` run on 2026-07-30 while passing in isolation.
        //
        // So: retry until the pair genuinely shares a second, then assert. This
        // terminates on observed STATE, not on elapsed time (convention 14) — a
        // green run pays exactly one iteration, and the bound only exists so a
        // pathological box fails loudly instead of spinning.
        let mut attempts = 0;
        let (s1, s2) = loop {
            let a = db
                .create_snapshot_v2(fs.id, None, &[], None, None)
                .await
                .unwrap();
            // Before the dedup fix this second `unwrap()` panicked on the UNIQUE
            // violation — the defect the test exists for.
            let b = db
                .create_snapshot_v2(fs.id, None, &[], None, None)
                .await
                .unwrap();
            if a.created_at == b.created_at {
                break (a, b);
            }
            attempts += 1;
            assert!(
                attempts < 50,
                "could not observe two creates inside one wall-clock second in 50 \
                 attempts — each pair takes microseconds, so this means the clock \
                 or the scheduler is pathological, not that dedup is broken"
            );
        };

        assert_eq!(
            s1.id, s2.id,
            "same-second create must dedup to the existing snapshot"
        );
        assert_eq!(s1.created_at, s2.created_at);
        // Count only the second under test, not every row in the set: a retried
        // attempt legitimately leaves a snapshot behind in an earlier second, and
        // "no duplicate row" is a statement about ONE second either way.
        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert_eq!(
            snaps
                .iter()
                .filter(|s| s.created_at == s1.created_at)
                .count(),
            1,
            "no duplicate snapshot row for the same second"
        );
    }

    /// Shared-folders Slice 1: the additive `folders.mls_group_id` binding
    /// column round-trips on a fresh DB, and the owner-scoped
    /// `set_folder_mls_group` binds / unbinds. A non-owner cannot bind.
    #[tokio::test]
    async fn mls_group_binding_round_trips_and_is_owner_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let attacker = [0xeeu8; 32];
        let group = [0x7cu8; 32];

        db.create_folder("shared", &owner).await.unwrap();
        // Born owner-only: the binding is NULL.
        let fs = db
            .get_folder_for_actor("shared", &owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.mls_group_id, None, "a fresh set is owner-only (unbound)");

        // A non-owner cannot bind it (owner-scoped UPDATE → no row changed).
        assert!(
            !db.set_folder_mls_group("shared", &attacker, Some(&group))
                .await
                .unwrap(),
            "a non-owner must not bind another user's set"
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

        // The owner binds it; the 32-byte group id round-trips.
        assert!(
            db.set_folder_mls_group("shared", &owner, Some(&group))
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_folder_for_actor("shared", &owner)
                .await
                .unwrap()
                .unwrap()
                .mls_group_id,
            Some(group.to_vec()),
            "the bound group id reads back"
        );

        // Unbinding (None) returns the set to owner-only.
        assert!(
            db.set_folder_mls_group("shared", &owner, None)
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_folder_for_actor("shared", &owner)
                .await
                .unwrap()
                .unwrap()
                .mls_group_id,
            None,
            "unbinding clears the binding"
        );
    }

    /// F8: conflict resolution must be owner-scoped. `sync_conflicts.id` is
    /// enumerable autoincrement, so a non-owner who guesses the id could force a
    /// winner onto a victim's folder or mark it resolved. Both resolve paths
    /// must reject a caller who does not own the conflict's folder.
    #[tokio::test]
    async fn conflict_resolve_is_owner_scoped() {
        use crate::db::ConflictCandidateRow;

        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa0u8; 32];
        let attacker = [0xeeu8; 32];
        let device = [0xd0u8; 32];
        let win_hash = [0x11u8; 32];

        let fs_id = db.create_folder("owned", &owner).await.unwrap();
        let conflict_id = db
            .report_conflict(
                &owner,
                fs_id,
                &device,
                "doc.txt",
                "modify-modify",
                None,
                &[ConflictCandidateRow {
                    manifest_hash: win_hash.to_vec(),
                    device_id: device.to_vec(),
                    size_bytes: 10,
                    created_at: 0,
                    content_key_version: None,
                }],
                None,
                Default::default(),
                false,
            )
            .await
            .unwrap();

        // Attacker (does not own the folder) cannot resolve either way.
        assert!(
            !db.resolve_conflict(&attacker, conflict_id).await.unwrap(),
            "non-owner mark-resolve must be a no-op"
        );
        assert!(
            matches!(
                db.resolve_conflict_choose_winner(&attacker, conflict_id, &win_hash)
                    .await
                    .unwrap(),
                ResolveWinner::NotFound
            ),
            "non-owner choose-winner must be NotFound (no leak, no propagation)"
        );
        // The conflict is still unresolved after the attacker's attempts.
        let open = db.list_conflicts_for_actor(&owner, false).await.unwrap();
        assert_eq!(open.len(), 1, "attacker must not have resolved it");

        // The owner resolves it normally.
        assert!(matches!(
            db.resolve_conflict_choose_winner(&owner, conflict_id, &win_hash)
                .await
                .unwrap(),
            ResolveWinner::Resolved { .. }
        ));
        assert!(
            db.list_conflicts_for_actor(&owner, false)
                .await
                .unwrap()
                .is_empty(),
            "owner resolution clears the conflict"
        );
    }

    /// 2026-09-28: the pre-resolved conflict report is a member-writable door that
    /// mints two charged rows — the reporter's loser retention row and the winner
    /// head row — so it meters them exactly as the record door would
    /// (`file-versions.md` § Retention (4)): a negative declared size refuses, a
    /// charge past the owner's ceiling refuses, and nothing lands on a refusal;
    /// an accepted report charges the owner loser + winner. Before the fix both
    /// rows rested uncharged at the caller's word, and supersede / soft-prune
    /// credited them back, flooring the owner's meter.
    #[tokio::test]
    async fn pre_resolved_conflict_report_meters_its_loser_and_winner_rows() {
        use crate::db::{ConflictCandidateRow, ResolvedReport};

        let db = CacheDb::open_in_memory().unwrap();
        let mut tier = db.get_tier("free").await.unwrap().unwrap();
        tier.max_storage_bytes = 1000;
        db.update_tier(&tier).await.unwrap();
        let owner = [0xb0u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        let fs_id = db.create_folder("docs", &owner).await.unwrap();
        let (me, other) = ([0xd1u8; 32], [0xd2u8; 32]);
        let (mine, theirs, merged) = ([0x11u8; 32], [0x22u8; 32], [0x33u8; 32]);

        let cands = |mine_size: i64, theirs_size: i64| {
            vec![
                ConflictCandidateRow {
                    manifest_hash: mine.to_vec(),
                    device_id: me.to_vec(),
                    size_bytes: mine_size,
                    created_at: 0,
                    content_key_version: None,
                },
                ConflictCandidateRow {
                    manifest_hash: theirs.to_vec(),
                    device_id: other.to_vec(),
                    size_bytes: theirs_size,
                    created_at: 0,
                    content_key_version: None,
                },
            ]
        };
        let merged_winner = |size: i64| ResolvedReport {
            resolution: "merged".into(),
            winning_manifest_hash: merged.to_vec(),
            winning_size_bytes: size,
            winning_content_key_version: None,
            winner_device_id: me.to_vec(),
            winning_derived_through: None,
            losing_derived_through: None,
            winning_carries_novelty: None,
        };
        let report =
            |path: &'static str, c: Vec<ConflictCandidateRow>, r: Option<ResolvedReport>| {
                let db = &db;
                async move {
                    db.report_conflict(
                        &owner,
                        fs_id,
                        &me,
                        path,
                        "modify-modify",
                        None,
                        &c,
                        r,
                        Default::default(),
                        false,
                    )
                    .await
                }
            };
        async fn used(db: &CacheDb, actor: [u8; 32]) -> i64 {
            db.get_user(&actor)
                .await
                .unwrap()
                .unwrap()
                .storage_bytes_used
        }
        async fn rows(db: &CacheDb, fs_id: i64) -> (i64, i64) {
            let conn = db.conn.lock().await;
            let changes = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_changes WHERE folder_id = ?1",
                    [fs_id],
                    |r| r.get(0),
                )
                .unwrap();
            let conflicts = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_conflicts WHERE folder_id = ?1",
                    [fs_id],
                    |r| r.get(0),
                )
                .unwrap();
            (changes, conflicts)
        }

        // Accepted: loser (100) + merged winner (200) → the owner pays 300.
        report("a.txt", cands(100, 50), Some(merged_winner(200)))
            .await
            .expect("an in-quota resolved report lands");
        assert_eq!(used(&db, owner).await, 300, "loser + winner are charged");
        assert_eq!(rows(&db, fs_id).await, (2, 1));

        // A winner declaring more than the remaining 700 refuses — and nothing
        // lands: no change row, no conflict row, no charge.
        let err = report("b.txt", cands(100, 50), Some(merged_winner(i64::MAX)))
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageQuotaError::Exceeded { .. }),
            "typed Exceeded, got {err}"
        );
        // The loser tips it over too: 400 + 400 > 700 though each fits alone.
        let err = report("b.txt", cands(400, 50), Some(merged_winner(400)))
            .await
            .unwrap_err();
        assert!(matches!(err, StorageQuotaError::Exceeded { .. }), "{err}");
        assert_eq!(used(&db, owner).await, 300, "a refusal charges nothing");
        assert_eq!(rows(&db, fs_id).await, (2, 1), "a refusal lands nothing");

        // A negative declaration refuses on either row — the winner's own size,
        // the loser candidate's, or a winning candidate's.
        for (label, c, r) in [
            ("merged winner", cands(10, 10), Some(merged_winner(-500))),
            ("loser", cands(-500, 10), Some(merged_winner(10))),
            (
                "winning candidate",
                cands(10, -500),
                Some(ResolvedReport {
                    resolution: "latest_wins".into(),
                    winning_manifest_hash: theirs.to_vec(),
                    winning_size_bytes: -500,
                    winner_device_id: other.to_vec(),
                    ..merged_winner(0)
                }),
            ),
            // An unresolved report mints no row now, but a later choose-winner
            // mints one at the candidate's declared size — refused at the door.
            ("unresolved candidate", cands(10, -500), None),
        ] {
            let err = report("c.txt", c, r).await.unwrap_err();
            assert!(
                matches!(err, StorageQuotaError::NegativeSize { .. }),
                "{label}: typed NegativeSize, got {err}"
            );
        }
        assert_eq!(
            used(&db, owner).await,
            300,
            "negative refusals credit nothing"
        );
        assert_eq!(rows(&db, fs_id).await, (2, 1));
    }

    /// The same, member half: a `writer`-granted member's resolved report bumps
    /// their `folder_member_access.bytes_used` by the same loser + winner charge
    /// (the counter `adjust_version_accounting_in_conn` credits on release) and
    /// refuses `MemberCapExceeded` past their `byte_cap`, charging nothing.
    #[tokio::test]
    async fn member_conflict_report_charges_and_is_refused_by_the_member_cap() {
        use crate::db::{ConflictCandidateRow, ResolvedReport};

        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xb1u8; 32];
        let writer = [0xb2u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&writer, "free", "writer").await.unwrap();
        let fs_id = db.create_folder("shared", &owner).await.unwrap();
        let group = b"conflict-meter-group".to_vec();
        assert!(
            db.set_folder_mls_group("shared", &owner, Some(&group))
                .await
                .unwrap()
        );
        let channel = fauna_mls::types::ChannelId::from_group_id(&group).0;
        db.set_folder_member_access(&channel, &writer, "writer", Some(250))
            .await
            .unwrap();

        let dev = [0xd3u8; 32];
        let (mine, merged) = ([0x44u8; 32], [0x55u8; 32]);
        let report = |path: &'static str| {
            let db = &db;
            async move {
                db.report_conflict(
                    &writer,
                    fs_id,
                    &dev,
                    path,
                    "modify-modify",
                    None,
                    &[ConflictCandidateRow {
                        manifest_hash: mine.to_vec(),
                        device_id: dev.to_vec(),
                        size_bytes: 100,
                        created_at: 0,
                        content_key_version: None,
                    }],
                    Some(ResolvedReport {
                        resolution: "merged".into(),
                        winning_manifest_hash: merged.to_vec(),
                        winning_size_bytes: 200,
                        winning_content_key_version: None,
                        winner_device_id: dev.to_vec(),
                        winning_derived_through: None,
                        losing_derived_through: None,
                        winning_carries_novelty: None,
                    }),
                    Default::default(),
                    false,
                )
                .await
            }
        };
        let member_used = || async {
            db.get_folder_member_role(&channel, &writer)
                .await
                .unwrap()
                .unwrap()
                .bytes_used
        };
        let user_used = |actor: [u8; 32]| {
            let db = &db;
            async move {
                db.get_user(&actor)
                    .await
                    .unwrap()
                    .unwrap()
                    .storage_bytes_used
            }
        };

        // 100 + 200 = 300 > the 250 cap → refused, nothing charged anywhere.
        let err = report("a.txt").await.unwrap_err();
        assert!(
            matches!(err, StorageQuotaError::MemberCapExceeded { .. }),
            "typed MemberCapExceeded, got {err}"
        );
        assert_eq!(member_used().await, 0);
        assert_eq!(user_used(owner).await, 0);

        // Cap raised → lands; the owner pays, the member counter bumps.
        db.set_folder_member_access(&channel, &writer, "writer", Some(1000))
            .await
            .unwrap();
        report("a.txt").await.expect("within the cap");
        assert_eq!(user_used(owner).await, 300, "owner pays");
        assert_eq!(member_used().await, 300, "the member counter bumps");
        assert_eq!(
            user_used(writer).await,
            0,
            "the writer's own quota is untouched"
        );
    }

    /// The same, choose-winner half: the chooser mints a head row at the
    /// winning candidate's report-time size, so it charges the owner like any
    /// metered record — refusing past the ceiling (the conflict stays open) and
    /// refusing a negative candidate size that rested before the door refused it.
    #[tokio::test]
    async fn choose_winner_meters_the_head_row_it_mints() {
        use crate::db::ConflictCandidateRow;

        let db = CacheDb::open_in_memory().unwrap();
        let mut tier = db.get_tier("free").await.unwrap().unwrap();
        tier.max_storage_bytes = 1000;
        db.update_tier(&tier).await.unwrap();
        let owner = [0xb3u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        let fs_id = db.create_folder("docs", &owner).await.unwrap();
        let dev = [0xd4u8; 32];
        let (small, big) = ([0x66u8; 32], [0x77u8; 32]);
        let conflict_id = db
            .report_conflict(
                &owner,
                fs_id,
                &dev,
                "doc.txt",
                "modify-modify",
                None,
                &[
                    ConflictCandidateRow {
                        manifest_hash: small.to_vec(),
                        device_id: dev.to_vec(),
                        size_bytes: 400,
                        created_at: 0,
                        content_key_version: None,
                    },
                    ConflictCandidateRow {
                        manifest_hash: big.to_vec(),
                        device_id: dev.to_vec(),
                        size_bytes: 5000,
                        created_at: 0,
                        content_key_version: None,
                    },
                ],
                None,
                Default::default(),
                false,
            )
            .await
            .unwrap();
        let used = || async {
            db.get_user(&owner)
                .await
                .unwrap()
                .unwrap()
                .storage_bytes_used
        };
        assert_eq!(used().await, 0, "an unresolved report mints no charged row");

        // Over the ceiling → refused, the conflict stays open, nothing charged.
        let err = db
            .resolve_conflict_choose_winner(&owner, conflict_id, &big)
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageQuotaError::Exceeded { .. }),
            "typed Exceeded, got {err}"
        );
        assert_eq!(used().await, 0);

        // A negative size that rested on a candidate row refuses too.
        let set_small_size = |size: i64| {
            let db = &db;
            async move {
                db.conn
                    .lock()
                    .await
                    .execute(
                        "UPDATE sync_conflict_candidates SET size_bytes = ?1
                         WHERE manifest_hash = ?2",
                        rusqlite::params![size, small.as_slice()],
                    )
                    .unwrap();
            }
        };
        set_small_size(-900).await;
        let err = db
            .resolve_conflict_choose_winner(&owner, conflict_id, &small)
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageQuotaError::NegativeSize { .. }),
            "typed NegativeSize, got {err}"
        );
        assert_eq!(used().await, 0, "a negative candidate credits nothing");
        set_small_size(400).await;

        // In quota → resolves and charges the head row's size.
        assert!(matches!(
            db.resolve_conflict_choose_winner(&owner, conflict_id, &small)
                .await
                .unwrap(),
            ResolveWinner::Resolved { .. }
        ));
        assert_eq!(used().await, 400, "the minted head row is charged");
    }

    /// S7 (the log + error-string scrub, `file-sync.md` § Sealed names &
    /// paths): `manifest_reference_sources` is a GC-debug diagnostic that used
    /// to interpolate the plaintext folder name and path directly. It now
    /// selects only their hash companions, so it must never leak the
    /// plaintext even though it never opens a seal or checks a key.
    #[tokio::test]
    async fn manifest_reference_sources_never_leaks_plaintext_name_or_path() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x33u8; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_folder("My Secret Documents", &owner)
            .await
            .unwrap();
        let fs = db.get_folder("My Secret Documents").await.unwrap().unwrap();

        let path = "taxes/eviction_notice.pdf";
        let path_hash = fauna_core::sync::path_hash(path);
        let manifest_hash = [0x44u8; 32];
        let dev = [0xD2u8; 32];

        db.record_sync_change_metered(
            &owner,
            &owner,
            None,
            &path_hash,
            Some(&manifest_hash),
            100,
            "create",
            fs.id,
            &dev,
            Some(path),
            None,
            None,
            None,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        let sources = db.manifest_reference_sources(&manifest_hash).await.unwrap();
        assert!(!sources.is_empty(), "the recorded row must be found");
        for s in &sources {
            assert!(!s.contains("My Secret Documents"), "leaked set name: {s}");
            assert!(!s.contains("eviction_notice"), "leaked path: {s}");
            assert!(!s.contains("taxes"), "leaked path: {s}");
        }

        // Still correlatable during debugging: the redacted values match what
        // the shared redaction helper produces for the same plaintext.
        let expected_name = fauna_core::log_redact::log_hash_prefix(
            "name",
            &fauna_core::path_crypto::set_name_hash("My Secret Documents"),
        );
        let expected_path = fauna_core::log_redact::log_hash_prefix("path", &path_hash);
        assert!(
            sources.iter().any(|s| s.contains(&expected_name)),
            "expected {expected_name} in {sources:?}"
        );
        assert!(
            sources.iter().any(|s| s.contains(&expected_path)),
            "expected {expected_path} in {sources:?}"
        );
    }

    /// The table→struct half of the ratchet. `unfresh_property`'s
    /// exhaustive destructure already binds struct→classifier: a field added
    /// to [`super::TargetFolderPosture`] and left unclassified there stops the
    /// build. It does NOT bind table→struct — nothing checks that
    /// `TargetFolderPosture`'s hand-picked SELECT column list still covers
    /// `folders`. This census closes that half: every live column must be
    /// either a `TargetFolderPosture` member or named on the ignore list
    /// below with the reason it carries no publication-bearing posture. A
    /// by-hand census at filing time undercounted `folders` at 15 columns
    /// (it is ~31 — 11 legacy `ALTER TABLE`-added columns were never
    /// examined) — this test enumerates the LIVE schema instead of trusting
    /// a list, precisely so that mistake cannot repeat ().
    #[tokio::test]
    async fn folders_schema_census_binds_every_column_to_posture_or_ignore_list() {
        // Mirrors `TargetFolderPosture`'s own field names by hand — the
        // destructure ratchet in `unfresh_property` fails to compile if a
        // struct field is ever added without being classified there, so this
        // list can drift only by omission (a new `folders` column), which is
        // exactly what the assertion below catches.
        const BOUND: &[&str] = &[
            "mls_group_id",
            "audience",
            "website_enabled",
            "webdav_enabled",
            "web_paywall_tier",
        ];

        // The other half of that ratchet: BOUND above binds table->struct by
        // literal, but nothing checked that the literal still names a live
        // struct field, only a live column — so a field REMOVED from
        // `TargetFolderPosture` (and from `unfresh_property`'s destructure)
        // left BOUND still naming the column and this test still green. This
        // exhaustive destructure fails to compile the moment that happens,
        // exactly like `unfresh_property`'s.
        let super::TargetFolderPosture {
            mls_group_id: _,
            audience: _,
            website_enabled: _,
            webdav_enabled: _,
            web_paywall_tier: _,
        } = super::TargetFolderPosture::default();

        // Every other `folders` column, examined and found NOT
        // publication-bearing. Add a new column here only after asking the
        // same question `unfresh_property`'s doc comment asks: does it make
        // this target group-bound, public, website-serving, WebDAV-exposed or
        // paywalled?
        const IGNORED: &[(&str, &str)] = &[
            ("id", "identity — surrogate key, carries no posture"),
            ("name", "identity — display name, not publication-bearing"),
            (
                "actor_id",
                "identity — the owning actor, not publication-bearing",
            ),
            (
                "created_at",
                "identity — creation timestamp, not publication-bearing",
            ),
            (
                "public_floor_seq",
                "near miss: bounds what the public read plane serves, but the \
                 gate is audience-keyed (folder_handlers.rs's \
                 `is_public_audience`) and `audience` is already bound above, \
                 so a folder this could affect is refused before the floor is \
                 ever consulted. A flip-back leaves a stale floor that serves \
                 nothing, and the next ->public re-stamps it strictly higher \
                 than any row materialize wrote — so a re-homed corpus cannot \
                 become visible through it.",
            ),
            (
                "audience_attestation",
                "near miss: it vouches for the `public` audience, but to the \
                 SEATS and against the nest — no nest gate reads it (a nest \
                 that checked its own adversarial input would protect no one), \
                 so it changes nothing about what this target serves. The \
                 posture it speaks about is the already-bound `audience`.",
            ),
            (
                "nest_content_residency",
                "near miss: 'metadata_only' says chunk BYTES must not rest on \
                 the nest, but materialize writes no chunk bytes — \
                 `FolderRehomeRow` carries only path_hash / path_sealed / \
                 manifest_hash / size_bytes. The chunk path is separately \
                 guarded (chunk_routes.rs) and GC reclaims (backup/gc.rs).",
            ),
            ("conflict_policy", "behaviour — merge policy, no audience"),
            (
                "exclusive_editing",
                "behaviour — the owner's per-folder single-writer-at-a-time \
                 choice (migrations.rs v72), no audience",
            ),
            (
                "nest_snapshots",
                "behaviour — snapshot retention toggle, no audience",
            ),
            (
                "nest_snapshot_quiet_secs",
                "behaviour — snapshot cadence, no audience",
            ),
            (
                "version_retention",
                "behaviour — sync-plane version retention bound, no audience",
            ),
            (
                "retention_policy",
                "behaviour — the nest-place retention bound (see \
                 version_retention's sibling doc comment in migrations.rs), \
                 no audience",
            ),
            (
                "name_hash",
                "sealed — hash companion of `name`, addressing only",
            ),
            ("name_sealed", "sealed — label over `name`, naming only"),
            (
                "include_paths_sealed",
                "sealed — scoping over the owner's local filesystem layout, no audience",
            ),
            (
                "exclude_paths_sealed",
                "sealed — scoping over the owner's local filesystem layout, no audience",
            ),
            (
                "retention_policy_sealed",
                "sealed — label over retention_policy, no audience",
            ),
            (
                "include_paths",
                "plaintext sibling of include_paths_sealed — scoping, no audience",
            ),
            (
                "exclude_paths",
                "plaintext sibling of exclude_paths_sealed — scoping, no audience",
            ),
            ("node_cache", "internal cache flag, no audience"),
            (
                "cached_snapshot_count",
                "derived cache of snapshot count, no audience",
            ),
            (
                "cached_total_bytes",
                "derived cache of storage size, no audience",
            ),
            (
                "cached_last_snapshot_at",
                "derived cache of last-snapshot timestamp, no audience",
            ),
            (
                "high_cadence",
                "behaviour — sync scheduling flag, no audience",
            ),
            (
                "exclusive_editing",
                "behaviour — the owner's one-device-at-a-time WRITE choice \
                 (file-sync.md § Exclusive editing): it governs who may \
                 upload, never who may read, so it moves no publication \
                 posture. (v72 landed without this entry; classified here.)",
            ),
            (
                "custody_copy",
                "identity — the nest-internal reserved custody discriminator \
                 (reserved-folders.md § Destination capability): a custody \
                 copy is never published, served or shared, so the flag moves \
                 no publication posture.",
            ),
            (
                "set_nonce",
                "identity — the client-minted 32-byte set binding writer \
                 signatures are built under (mls-group-key-material.md § M2 → \
                 Writer-signed change records ruling (2)); stored opaque and \
                 echoed, it gates which signed records ingest accepts, never \
                 who may read, so it moves no publication posture.",
            ),
        ];

        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn().await;
        let mut stmt = conn.prepare("PRAGMA table_info(folders)").unwrap();
        let live: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        drop(stmt);
        drop(conn);

        assert!(
            live.len() > 20,
            "sanity: expected the genesis-created `folders` table to carry \
             more than the base CREATE TABLE's 20 columns (got {}); did the \
             genesis block stop creating them?",
            live.len()
        );

        let unclassified: Vec<&String> = live
            .iter()
            .filter(|c| {
                !BOUND.contains(&c.as_str()) && !IGNORED.iter().any(|(name, _)| *name == c.as_str())
            })
            .collect();
        assert!(
            unclassified.is_empty(),
            "folders column(s) {unclassified:?} are neither bound in \
             TargetFolderPosture nor on this test's ignore list — classify \
             them: publication-bearing (group-bound, public, \
             website-serving, WebDAV-exposed, paywalled) -> add to \
             TargetFolderPosture's destructure in `unfresh_property`; \
             otherwise -> add here with the reason"
        );

        // Every BOUND/IGNORED entry should name a column that actually
        // exists — a stale entry here would silently stop being exercised by
        // the assertion above.
        for name in BOUND {
            assert!(
                live.iter().any(|c| c == name),
                "BOUND lists {name:?} but folders has no such column — TargetFolderPosture drifted from the schema"
            );
        }
        for (name, _) in IGNORED {
            assert!(
                live.iter().any(|c| c == name),
                "IGNORED lists {name:?} but folders has no such column — prune the stale entry"
            );
        }
    }
}

/// `sync_devices.p2p_participation`'s two literal values (`p2p.md` § Per-device
/// participation): the device's own last report. NULL = never reported.
const P2P_PARTICIPATION_ON: &str = "on";
const P2P_PARTICIPATION_OFF: &str = "off";

#[cfg(test)]
mod folder_name_blank_addressing_tests {
    //! The storage half of the `folders.name` contraction
    //! (`encryption-at-rest.md` § Implementation status today): once a sealed
    //! set's plaintext name stops resting, `''` is no address, and every
    //! per-set mutator keys on the row the handler resolved — never on
    //! `(name, actor_id)`, which over a blank name would match whichever
    //! blanked set of the actor came first.
    use crate::db::{CacheDb, FolderUpdate};

    const OWNER: [u8; 32] = [0x5au8; 32];

    /// Two owned sets, each bound to its own group, the first blanked the way
    /// the contraction rests a sealed set's name. (One blank per owner is all
    /// the plaintext `UNIQUE(name, actor_id)` admits until the schema step
    /// rests NULL instead.)
    async fn one_blank_set_beside_a_named_one(db: &CacheDb) -> (i64, i64) {
        db.create_folder("photos", &OWNER).await.unwrap();
        db.create_folder("docs", &OWNER).await.unwrap();
        let a = db
            .get_folder_for_actor("photos", &OWNER)
            .await
            .unwrap()
            .unwrap()
            .id;
        let b = db
            .get_folder_for_actor("docs", &OWNER)
            .await
            .unwrap()
            .unwrap()
            .id;
        for (id, g) in [(a, [1u8; 32]), (b, [2u8; 32])] {
            assert!(db.set_folder_mls_group_by_id(id, Some(&g)).await.unwrap());
        }
        db.conn
            .lock()
            .await
            .execute("UPDATE folders SET name = '' WHERE id = ?1", [a])
            .unwrap();
        (a, b)
    }

    #[tokio::test]
    async fn an_empty_name_resolves_no_set() {
        let db = CacheDb::open_in_memory().unwrap();
        let (a, _) = one_blank_set_beside_a_named_one(&db).await;

        assert!(db.get_folder_for_actor("", &OWNER).await.unwrap().is_none());
        assert!(db.get_folders_by_name("").await.unwrap().is_empty());
        assert!(
            db.get_group_bound_folders_by_name("")
                .await
                .unwrap()
                .is_empty()
        );

        let update = FolderUpdate {
            webdav_enabled: Some(true),
            ..Default::default()
        };
        assert!(!db.update_folder_for_user("", &OWNER, update).await.unwrap());
        assert!(!db.set_folder_mls_group("", &OWNER, None).await.unwrap());
        assert!(!db.delete_folder_for_user("", &OWNER).await.unwrap());

        let row = db.get_folder_by_id(a).await.unwrap().unwrap();
        assert!(
            !row.webdav_enabled,
            "a by-name write over '' reached the blank row"
        );
        assert!(
            row.mls_group_id.is_some(),
            "a by-name unbind over '' reached the blank row"
        );
    }

    #[tokio::test]
    async fn the_id_keyed_mutators_touch_only_their_own_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let (a, b) = one_blank_set_beside_a_named_one(&db).await;

        let update = FolderUpdate {
            webdav_enabled: Some(true),
            ..Default::default()
        };
        assert!(db.update_folder_by_id(a, update).await.unwrap());
        assert!(
            db.set_folder_web_paywall_tier_by_id(a, Some("gold"))
                .await
                .unwrap()
        );
        assert!(db.set_folder_mls_group_by_id(a, None).await.unwrap());

        let ra = db.get_folder_by_id(a).await.unwrap().unwrap();
        let rb = db.get_folder_by_id(b).await.unwrap().unwrap();
        assert!(ra.webdav_enabled && !rb.webdav_enabled);
        assert_eq!(ra.web_paywall_tier.as_deref(), Some("gold"));
        assert_eq!(rb.web_paywall_tier, None);
        assert_eq!(ra.mls_group_id, None);
        assert_eq!(rb.mls_group_id, Some(vec![2u8; 32]));

        assert!(db.delete_folder_by_id(a).await.unwrap());
        assert!(db.get_folder_by_id(a).await.unwrap().is_none());
        assert!(db.get_folder_by_id(b).await.unwrap().is_some());
        let gone = FolderUpdate {
            webdav_enabled: Some(false),
            ..Default::default()
        };
        assert!(!db.update_folder_by_id(a, gone).await.unwrap());
    }
}

#[cfg(test)]
impl CacheDb {
    /// Blank a folder row's plaintext name, the at-rest shape a sealed set
    /// takes once the name is contracted — for tests that prove a path
    /// addresses the set by `name_hash` alone.
    pub(crate) async fn blank_folder_name_for_test(&self, folder_id: i64) {
        self.conn
            .lock()
            .await
            .execute("UPDATE folders SET name = '' WHERE id = ?1", [folder_id])
            .expect("blank folder name");
    }
}
