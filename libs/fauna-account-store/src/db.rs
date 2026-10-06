//! SQLite-backed local sync state database.
//!
//! **Standalone — usable without an engine.** This module is the client-side
//! state-dir + SQLite floor (per-set state DBs, `device.db`, the actor-scoped
//! dir derivation and erase); it has no code dependency on the sync engine, and its
//! entire dependency set is `fauna-core` + `rusqlite` + `anyhow` plus
//! foundation facades (`tracing`, `serde`, `serde_json`). Consumers
//! that need only this layer (account scoping, backup audits, `device.db`
//! reads) use it without constructing a `SyncEngine`. The extraction its old
//! in-crate module doc deferred was executed 2026-08-10 — the production
//! consumer that wanted `db` without the engine is the account store
//! (`account-data-plane.md` § The account store), this crate;
//! `fauna-sync-engine` re-exports the module verbatim so no call site
//! re-paths. Keep the floor thin: do not add a dependency on the engine or
//! its graph (tokio/reqwest/notify/MLS) here — that invariant is now the
//! crate's (see the crate docs and `app-guidelines.md` § crate layering).

use std::path::Path;

use crate::physical::normalize_actor_hex;
use anyhow::{Context, Result};
use fauna_core::data::{ContentHash, Timestamp};
use fauna_core::folder_keys::FolderRef;
use rusqlite::{Connection, params};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// The `sync.db` **genesis** — every table at its current shape, with no
/// step written for a database predating it (the compat-remnant sweep's
/// baseline reset, `version-compatibility.md` § Dimension 2: no pre-sweep
/// `sync.db` rests anywhere). Growing a table by a nullable or
/// constant-default column needs no hand-written `ALTER`: add it here and
/// [`SyncDb::migrate`]'s `reconcile_added_columns` adds it to a long-lived
/// database; anything outside that additive class is refused at open and
/// needs an explicit expand→migrate→contract step.
const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS device_identity (
        key         TEXT PRIMARY KEY,
        value       BLOB NOT NULL
    );

    CREATE TABLE IF NOT EXISTS sync_entries (
        path            TEXT PRIMARY KEY,
        local_hash      BLOB,
        remote_hash     BLOB,
        manifest_hash   BLOB,
        state           TEXT NOT NULL DEFAULT 'synced',
        local_mtime     INTEGER NOT NULL DEFAULT 0,
        remote_mtime    INTEGER NOT NULL DEFAULT 0,
        size_bytes      INTEGER NOT NULL DEFAULT 0,
        version_num     INTEGER NOT NULL DEFAULT 1,
        last_synced_at  INTEGER NOT NULL DEFAULT 0,
        pinned          INTEGER NOT NULL DEFAULT 0,
        -- When the entry was tombstoned (`delete_entry`); `purge_tombstones` reads it.
        deleted_at      INTEGER DEFAULT NULL,
        -- M2 content-key generation the chunks of this version were sealed under
        -- (cross-user shared folders). NULL for owner-only sets;
        -- the read path tries every keys_for(version) candidate and fails closed if absent.
        content_key_version INTEGER,
        -- Owner-only re-seal marker: 1 once this
        -- entry's recorded manifest is known sealed (stored_hashes present) —
        -- either verified as such, or re-sealed by the audience flip-back pass. Local
        -- cache only (a lost marker re-checks; convergent re-upload dedups).
        owner_sealed INTEGER NOT NULL DEFAULT 0,
        -- Post-succession corpus re-seal marker (`identity-succession.md`
        -- § Re-key scope, the `BackupKey` corpus row): 1 once this entry's chunks
        -- are known to open under the engine's CURRENT owner root — either
        -- verified as such, or re-sealed by `SyncEngine::reseal_predecessor_sealed`.
        -- Distinct from `owner_sealed`, which answers sealed-at-all vs the
        -- plaintext corpus of a public-audience folder; this answers sealed-under-WHICH-root, the only question a
        -- successor's re-seal can act on. Local cache only (a lost marker merely
        -- re-checks; the convergent re-upload dedups to the identical blob).
        current_root_sealed INTEGER NOT NULL DEFAULT 0,
        -- The content hash that `manifest_hash` (the recorded head / hydration
        -- anchor) reassembles to — set ONLY where the head is proven to match the
        -- local content (record success, hydrate-on-open, download apply). It is
        -- the dehydration gate's proof that freeing the bytes is lossless: a
        -- re-hydration fetches `manifest_hash`, so only `recorded_content_hash ==
        -- disk hash` guarantees the same bytes come back. NULL for a row whose
        -- head was never proven (e.g. a record that FAILED,
        -- which advances `local_hash` but leaves `manifest_hash` at the old base);
        -- the gate fails closed on NULL. Local cache only — re-derived on the next
        -- record/hydrate/download.
        recorded_content_hash BLOB,
        -- HOW the proof above was earned (`file-sync.md` § Relay serving → *A
        -- holder keeps what it wrote*): 'fetched' when the body came from
        -- another holder (hydrate-on-open, download apply, a confirmed peer
        -- body), 'own' when this device recorded it. NULL — a row stamped
        -- before the column existed — reads as 'own', the refusing direction:
        -- in a metadata-only folder an own record proves nothing about the
        -- head being fetchable, so the dehydration gate keeps that body.
        -- Written with every stamp of `recorded_content_hash`, cleared with it.
        recorded_proof_origin TEXT,
        -- The recorded head's THUMBNAIL pointer, cached beside the rest of that
        -- head (`manifest_hash`, `size_bytes`, `content_key_version`). The value
        -- rides `sync_changes`; `SyncEngine::reseal_predecessor_sealed` is the
        -- consumer: a build whose thumbnailer cannot regenerate must MOVE the
        -- existing sealed thumbnail off the retired root, and it cannot move
        -- what it cannot name. NULL means no move available, never a wrong move.
        thumbnail_hash  TEXT,
        -- The SEEN mark (`delete-propagation.md` § *An offline placeholder delete
        -- propagates*, decision (a)): 1 once this engine has put this row's
        -- placeholder on the disk (a cfapi transfer that SUCCEEDED) or observed it
        -- present in a scan. A `Placeholder` row is counted by delete detection only
        -- when seen — a never-seen row's absence is evidence of nothing (lazy
        -- population). Device-local derived state, re-derivable from a scan, read
        -- nowhere but the delete universe; cleared BEFORE the host itself removes
        -- placeholders (decision (e)).
        seen_on_disk    INTEGER NOT NULL DEFAULT 0,
        -- Who the recorded head (`manifest_hash`) was SIGNED AS
        -- (`writer-signed-change-records.md` ruling (11)(d)): the 32-byte
        -- identity of the verified row this entry was folded from, or of this
        -- device's own record. NULL = unknown, which opens under no owner
        -- root (fail closed). Cleared by every write that moves
        -- `manifest_hash` without naming a signer, so it never describes a
        -- head it was not recorded for.
        head_signed_as  BLOB
    );

    -- The SIGNER_BOUND hold (`writer-signed-change-records.md` ruling (11)(e)):
    -- a path whose head the succession take-over could not open under its
    -- signer's roots. Its local file stays on disk, and the upload choke point
    -- never offers it to the nest as new content — re-uploading it would
    -- launder exactly the row the note refused. Released when the path gains
    -- a head this device can vouch for (a recorded head, a stamped signer).
    CREATE TABLE IF NOT EXISTS signer_bound_holds (
        path            TEXT PRIMARY KEY,
        manifest_hash   BLOB
    );

    CREATE TABLE IF NOT EXISTS transfer_queue (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        path        TEXT NOT NULL,
        direction   TEXT NOT NULL,
        chunk_hash  BLOB NOT NULL,
        priority    INTEGER NOT NULL DEFAULT 0,
        status      TEXT NOT NULL DEFAULT 'pending',
        retry_count INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL,
        -- Retry backoff: when the last attempt ran (`increment_retry`).
        last_attempt_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_transfer_queue_status
        ON transfer_queue(status, direction);

    -- Covering index for the aggregate backlog projection
    -- (`SyncDb::transfer_backlog` / `tracked_totals`): COUNT/SUM over a state
    -- subset resolve from the index alone, so a status poll never scans rows
    -- even on a 100k-folder.
    CREATE INDEX IF NOT EXISTS idx_sync_entries_state
        ON sync_entries(state, size_bytes);

    CREATE TABLE IF NOT EXISTS sync_anchor (
        key   TEXT PRIMARY KEY,
        value INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS sync_conflicts (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        path        TEXT NOT NULL,
        conflict_type TEXT NOT NULL,
        details     TEXT,
        created_at  INTEGER NOT NULL,
        resolved_at INTEGER,
        -- The four columns below serve `catchup_failed` rows only
        -- (`conflicts.md` § Skipped catch-up changes reach the review list).
        -- The nest's conflict id once the skip's report landed; NULL = the
        -- report is still owed and every catch-up pass re-sends it.
        nest_id     INTEGER,
        -- 1 when `path` holds the change row's own wire `path_hash` (its
        -- sealed path never opened here — there is no plaintext to file under).
        path_is_hash INTEGER NOT NULL DEFAULT 0,
        -- That change row's sealed label, verbatim, for the report to forward;
        -- NULL on a `path_is_hash` row = a seal-less change, which stays local.
        path_sealed BLOB,
        -- When the candidate-free `conflicts.resolve` for `nest_id` landed;
        -- NULL on a cured (`resolved_at`) row with a `nest_id` = still owed.
        nest_resolved_at INTEGER
    );

    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    -- Per-(kind, scope_id, segment_id, member_actor_id, dest_id) backup state:
    -- the per-destination already-synced cursor, derived/re-derivable (losing
    -- it forces a re-scan + idempotent re-upload, never data loss).
    -- For per-actor kinds (mail): member_actor_id == scope_id.

    CREATE TABLE IF NOT EXISTS segment_backup_state (
        kind             TEXT    NOT NULL,
        scope_id         BLOB    NOT NULL,
        segment_id       INTEGER NOT NULL,
        member_actor_id  BLOB    NOT NULL,
        dest_id          BLOB    NOT NULL,
        last_synced_at   INTEGER NOT NULL,
        last_chunk_count INTEGER NOT NULL,
        last_byte_size   INTEGER NOT NULL,
        -- Plaintext size of the segment's `.meta` sidecar as last pushed
        -- beside its `.dat`. NULL when no pushed sidecar is known for the
        -- segment: the shared diff classifies such a row as
        -- `to_backfill_meta`, so the next pass
        -- fills the gap without re-moving the `.dat` — content addressing
        -- cannot fill a path that was never pushed, and a row that reads as
        -- done would otherwise hide the missing half forever.
        last_meta_size   INTEGER,
        PRIMARY KEY (kind, scope_id, segment_id, member_actor_id, dest_id)
    );

    -- Plan 6 T5: per-(kind, scope_id, member_actor_id, dest_id) hash of the
    -- last-mirrored KindManifest. The coordinator re-uploads the manifest
    -- mirror only when this hash changes or when any segment was (re)uploaded.
    CREATE TABLE IF NOT EXISTS segment_backup_manifest_state (
        kind             TEXT    NOT NULL,
        scope_id         BLOB    NOT NULL,
        member_actor_id  BLOB    NOT NULL,
        dest_id          BLOB    NOT NULL,
        last_synced_at   INTEGER NOT NULL,
        manifest_blake3  BLOB    NOT NULL,
        -- NULL until a pass for this tuple actually uploads or drops a
        -- segment. last_synced_at above stays a pure bookkeeping timestamp
        -- (every manifest-mirror write, incl. an owner's first pass with
        -- zero segments) -- the UI-facing status reads THIS column instead
        -- (max_manifest_synced_at_for_dest), so a zero-content owner
        -- honestly reads never-synced rather than a manifest write that
        -- moved nothing of theirs (docs/goal/behavior/backup-destinations.md section
        -- Per-destination status read).
        last_content_synced_at INTEGER,
        PRIMARY KEY (kind, scope_id, member_actor_id, dest_id)
    );

    -- Track B removal-reconcile: every destination the coordinator has uploaded
    -- to, with the reconnect info its removal-reconcile needs AFTER the
    -- destination leaves the account's configured destinations (when there is no live
    -- `DestinationBinding` to read the URL / folder from). The coordinator
    -- upserts a row each pass; reconcile tears the destination-side custody down
    -- against any row whose `dest_id` is no longer configured, then forgets it.
    --
    -- `nest_id` is the destination's 32-byte nest pubkey, and it is here for one
    -- reason: the nest arm's teardown dials over the federation channel with
    -- `originate_expecting`, which REFUSES a peer that is not the id the owner
    -- named. Without the pin stored beside the URL the teardown would have to
    -- re-discover the id from the URL it is about to trust -- exactly the
    -- takeover the pin exists to refuse. Its one writer (the nest arm) always
    -- stores it, so it is NOT NULL.
    CREATE TABLE IF NOT EXISTS backup_destination_seen (
        dest_id    TEXT    PRIMARY KEY,
        dest_url   TEXT    NOT NULL,
        folder   TEXT    NOT NULL,
        nest_id    BLOB    NOT NULL,
        updated_at INTEGER NOT NULL
    );

    -- Ordinary-folder destination coverage: the per-(destination, folder) mirror
    -- cursor (backup-destinations.md section Ordinary-folder coverage). One row
    -- per source path the coordinator has mirrored, keyed by the SOURCE row's
    -- path_hash (which is also the destination custody path, hex-spelled), with
    -- the last-pushed manifest hash as the change detector. Derived/re-derivable
    -- exactly like segment_backup_state: losing it costs an idempotent re-upload
    -- (chunk dedup makes that cheap), never data.
    CREATE TABLE IF NOT EXISTS folder_backup_state (
        dest_id        TEXT    NOT NULL,
        folder_id      INTEGER NOT NULL,
        path_hash      BLOB    NOT NULL,
        manifest_hash  BLOB    NOT NULL,
        last_synced_at INTEGER NOT NULL,
        PRIMARY KEY (dest_id, folder_id, path_hash)
    );

    -- The folder mirror's per-(destination, folder) memo of the withheld-set
    -- digest a `run_folder_once` pass last fully checked its already-mirrored
    -- paths against (row 729 -- the already-mirrored short-circuit paying the
    -- whole manifest plane every pass while any legal withhold stands,
    -- `moderation.md` section Legal takedown -> The blob-serve door). A digest
    -- of the sorted set, not a bump-on-write generation: every complete GC
    -- sweep rewrites the withheld set wholesale via
    -- `replace_blob_legal_withhold`, so a counter would defeat the memo on
    -- every sweep even when the set's content is unchanged. A bare digest
    -- match is NOT enough on its own to trust: a set that changes and later
    -- reverts across a pass that pushed some paths then aborted could
    -- otherwise leave a stamp from before any of that happened, which then
    -- matches the reverted set again. The guard
    -- lives in `run_folder_once`, not here: the moment a pass sees a digest
    -- that does not match the stored row, it deletes the row before touching
    -- any path, so an abort leaves nothing behind for a later pass to wrongly
    -- trust -- a row can only ever describe the exact digest a
    -- fully-completed pass just finished re-checking everything against.
    -- Derived/re-derivable: losing it (a crash, a fresh attach) only costs
    -- one extra manifest open per already-mirrored path on the next pass.
    CREATE TABLE IF NOT EXISTS folder_withhold_checkpoint (
        dest_id         TEXT    NOT NULL,
        folder_id       INTEGER NOT NULL,
        withhold_digest BLOB    NOT NULL,
        checked_at      INTEGER NOT NULL,
        PRIMARY KEY (dest_id, folder_id)
    );

    -- The share leg's ROW-half retention (B2 — p2p-shared-set-build.md § Build design — the
    -- row half): this replica's OWN-authored change rows, written through at
    -- the engine's one recording funnel (record_change Ok(seq)), so
    -- ShareStore::changes_since serves REAL recorded rows — never rows
    -- synthesized from sync_entries, which is the trap that defeats the
    -- peer-served change-row provenance ruling (the author check would have
    -- nothing to check). Own-authored by construction: only this replica's
    -- own records pass the funnel. Columns mirror the SyncChange wire shape
    -- (hex spellings) so the serve-side conversion is a field map, not a
    -- derivation. `seq` is the nest-assigned sequence — UNIQUE, and NULL is
    -- reserved for the own-pending (offline-authored) leg, which mints rows
    -- before a nest has sequenced them and upgrades them in place.
    -- Derived-recoverable: the nest's log is the durable copy; losing this
    -- table costs offline serveability until rows re-record, never data.
    CREATE TABLE IF NOT EXISTS own_change_log (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        seq             INTEGER UNIQUE,
        path            TEXT NOT NULL,
        path_hash       TEXT NOT NULL,
        path_sealed     BLOB,
        manifest_hash   TEXT,
        size_bytes      INTEGER NOT NULL DEFAULT 0,
        change_type     TEXT NOT NULL,
        created_at      INTEGER NOT NULL,
        content_key_version INTEGER,
        thumbnail_hash  TEXT,
        derived_through INTEGER,
        is_resolution   INTEGER,
        author_actor_id TEXT NOT NULL,
        device_id       TEXT NOT NULL,
        -- The writer signature over the row AS SERVED (writer-signed change
        -- records, mls-group-key-material.md § M2): the Ed25519 signature,
        -- the key it verifies under, and the delegated signer's cert as
        -- canonical embed-as-bytes (NULL for a direct signer). All NULL on a
        -- row retained before its host could sign — served unsigned, admitted
        -- on the channel's proof alone until the flip.
        signature       BLOB,
        signer_key      BLOB,
        signer_cert     BLOB
    );

    -- The share leg's RELAYED rows (the p2p relayed-row lift —
    -- mls-group-key-material.md § M2 → Writer-signed change records, ruling
    -- (3); p2p.md § Peer-served change-row provenance): other writers' rows
    -- this replica's reader VERIFIED — off the nest pull or a peer's page —
    -- kept byte-exact so the share leg serves every row it holds and each
    -- stays self-contained. `row` is the served SyncChange's canonical
    -- dag-cbor (the signature covers it as served, so it is never re-derived);
    -- `signer_cert` the delegated signer's canonical embed-as-bytes (NULL for
    -- a direct signer). `seq` is the nest's (UNIQUE) for a sequenced row and
    -- NULL for a writer's pending row, at most one per (author, path_hash) and
    -- retired when that writer's sequenced row for the path arrives.
    -- Derived-recoverable: the nest's log is the durable copy; losing this
    -- table costs offline relay until rows are read again, never data.
    CREATE TABLE IF NOT EXISTS relayed_change_log (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        seq             INTEGER UNIQUE,
        author_actor_id TEXT NOT NULL,
        path_hash       TEXT NOT NULL,
        row             BLOB NOT NULL,
        signer_cert     BLOB
    );

    -- The share leg's cached WRITER roster (B2 — p2p-shared-set-build.md § Build design — the
    -- row half): the last successful nest read of this set's actor roster,
    -- reduced to the one fact peer-row ingest consults — is this actor a
    -- writer here (the owner, or a member holding the writer access
    -- grant)? Fail-closed by absence: no row means not a cached writer, so
    -- rows are refused while bytes still serve (the provenance ruling's
    -- arm). Replaced atomically ONLY on a successful roster read — a failed
    -- read keeps the stale roster, which denies fresh writers until
    -- reconnect (heals) and honors recently-demoted ones briefly (the
    -- provisional overlay's reconcile cleans up). Derived-recoverable: the
    -- nest's roster is the durable copy.
    -- is_owner marks the one owner-role row of the read
    -- (writer-signed-change-records.md ruling (11)(c)): the owner a reader
    -- that holds no marker judges by, offline as on the nest pull. A constant
    -- default with no backfill: the table is replaced whole by the next read.
    CREATE TABLE IF NOT EXISTS share_writer_roster (
        actor_id     TEXT PRIMARY KEY,
        is_writer    INTEGER NOT NULL,
        refreshed_at INTEGER NOT NULL,
        is_owner     INTEGER NOT NULL DEFAULT 0
    );

    -- The same roster read's PROVEN predecessor chains, each stored beside
    -- the writer it belongs to (writer-signed-change-records.md ruling
    -- (8)(e), (11)(c)/(i)): one row per (writer, position), position 0 the
    -- nearest predecessor. A predecessor is never a share_writer_roster
    -- writer row on the strength of its chain — only when the roster lists
    -- it as a writer itself — so the peer door's judge places a retired
    -- identity by its chain, offline, exactly as the nest pull does.
    -- Replaced in the same transaction as share_writer_roster;
    -- derived-recoverable like it.
    CREATE TABLE IF NOT EXISTS share_writer_predecessors (
        writer_id      TEXT NOT NULL,
        position       INTEGER NOT NULL,
        predecessor_id TEXT NOT NULL,
        PRIMARY KEY (writer_id, position)
    );

    -- The share leg's provisional READ-SIDE overlay (B2 — p2p-shared-set-build.md § Build
    -- design — the row half): latest-per-path peer-ingested provisional
    -- rows. A provenance ledger over sync_entries, never a second fold:
    -- rows here enter NO nest-log accounting (no anchor, no frontier, no
    -- edit-frontier) and apply_remote_changes never consults this table.
    -- The nest's later sequenced rows confirm or supersede on reconcile,
    -- which only ever REMOVES rows here. content_hash is stamped at
    -- materialization so a confirm can stamp the withheld dehydration proof
    -- without re-reading disk. Derived-recoverable: dropping the table
    -- costs the provisional annotation, never bytes.
    CREATE TABLE IF NOT EXISTS share_overlay (
        path            TEXT PRIMARY KEY,
        seq             INTEGER NOT NULL,
        sequenced       INTEGER NOT NULL,
        change_type     TEXT NOT NULL,
        manifest_hash   TEXT,
        size_bytes      INTEGER NOT NULL DEFAULT 0,
        content_key_version INTEGER,
        proven_author   TEXT NOT NULL,
        materialized    INTEGER NOT NULL DEFAULT 0,
        content_hash    TEXT,
        ingested_at     INTEGER NOT NULL
    );

    -- The share leg's retained MANIFESTS (slice E — p2p.md § Cross-user
    -- shared-set transfer, the serve-side byte half): the canonical manifest
    -- bytes for content this replica sealed itself, keyed by manifest hash
    -- (hex), retained best-effort at the upload seal sites where the bytes
    -- are in hand. A manifest is self-verifying (key == blake3(bytes)), so
    -- retention needs no provenance discipline; the serve-side byte half
    -- answers manifest fetches from here and re-derives chunk BODIES from
    -- local plaintext ranges + the recorded generation, so serving never
    -- buffers whole files. `path` names the file whose seal produced the
    -- bytes (the chunk range source); a later seal re-points it, last-writer
    -- wins. Derived-recoverable: re-recording repopulates; the table dies
    -- with the set on leave/evict exactly as the row retention does.
    CREATE TABLE IF NOT EXISTS own_manifests (
        manifest_hash   TEXT PRIMARY KEY,
        bytes           BLOB NOT NULL,
        path            TEXT NOT NULL,
        content_key_version INTEGER,
        retained_at     INTEGER NOT NULL
    );

    -- The serve core's STORE-KEY INDEX (file-sync.md § Relay serving → the
    -- seat serves from the file, through one serve core): one row per chunk
    -- of a body this seat holds — its store key, the path and plaintext range
    -- it re-derives from, the plaintext hash that range must still hash to,
    -- and the generation it sealed under. Written where the seat seals an
    -- upload and where it applies a download (the manifest is in hand both
    -- times); a re-index of a path replaces that path's rows whole. Several
    -- paths may hold one key (identical content), so the key is the pair.
    -- Device-local and derived-recoverable: a lost row costs serveability of
    -- that body until it is next sealed or applied; a stale row answers none
    -- at the serve's own hash check.
    CREATE TABLE IF NOT EXISTS held_chunks (
        store_key   BLOB NOT NULL,
        path        TEXT NOT NULL,
        offset      INTEGER NOT NULL,
        len         INTEGER NOT NULL,
        plain_hash  BLOB NOT NULL,
        content_key_version INTEGER,
        PRIMARY KEY (store_key, path)
    );
    CREATE INDEX IF NOT EXISTS held_chunks_by_path ON held_chunks(path);

    -- The share leg's per-peer PULL cursor (slice E): the highest
    -- nest-sequenced seq this replica has ingested from that peer's
    -- own-authored rows, advanced only at the overlay ingest (the engine's
    -- door), read by the pump as its next `since`. Forward-only; losing a row
    -- merely re-pages rows the idempotent ingest already holds.
    CREATE TABLE IF NOT EXISTS share_pull_cursors (
        peer_actor  TEXT PRIMARY KEY,
        cursor      INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
";

// ---------------------------------------------------------------------------
// Per-actor state scoping
// ---------------------------------------------------------------------------

/// The one shared derivation of an actor's scoped state dir:
/// `<base>/<actor-id-hex>/`. Every process hosting or reading per-set engine
/// state (app, File Provider extension, sync agent) resolves through this exact
/// rule so the three can never diverge. The hex is normalized to lowercase and
/// must be exactly 64 hex chars (a 32-byte actor id) — anything else is refused
/// rather than silently producing a stray directory.
pub fn actor_state_dir(base: &Path, actor_id_hex: &str) -> Result<std::path::PathBuf> {
    Ok(base.join(normalize_actor_hex(actor_id_hex)?))
}

/// The scope component a malformed actor id resolves under — see
/// [`actor_state_dir_or_unresolved`]. Not 64 hex, so no sweep or reach query
/// ([`account_scopes_under`]) ever mistakes it for an account's scope.
pub const UNRESOLVED_ACTOR_COMPONENT: &str = "-unresolved-";

/// [`actor_state_dir`], degraded to `<base>/`[`UNRESOLVED_ACTOR_COMPONENT`]
/// on the should-never-happen malformed-hex path — a live keypair cannot
/// produce one, so a caller that must still land *somewhere* (linux's
/// `sync::sync_state_dir`/`backup_state_dir`, tui's `backup_audit::state_path`)
/// gets a scope of its own rather than failing. **Never the base itself:** the
/// base is the shared store root the actor scopes live directly under, and a
/// file landing there belongs to no account (windows' `AccountStateDir` takes
/// the same component).
pub fn actor_state_dir_or_unresolved(base: &Path, actor_id_hex: &str) -> std::path::PathBuf {
    actor_state_dir(base, actor_id_hex).unwrap_or_else(|_| base.join(UNRESOLVED_ACTOR_COMPONENT))
}

/// Best-effort erase actor `actor_id_hex`'s scoped state dir under each of
/// `bases` — one [`actor_state_dir`] + `remove_dir_all` per base. Every app's
/// own account-erasure entry point had this exact loop hand-copied, differing
/// only in which bases belong to it (linux's flat/sync/backup trio vs. tui's
/// flat/backup plus a separately-rooted W6 (account-data-plane.md § Workstreams) store base); that base-list
/// construction is genuinely per-app on-disk layout and stays with the
/// caller — this is only the identical iterate-and-remove body. A base whose
/// [`actor_state_dir`] fails to resolve, or whose dir doesn't exist, is
/// silently skipped: erasure is deliberately best-effort
/// (`account-scoping.md` § Erasure follows scope).
///
/// **Best-effort is not the same as silent, and until 2026-09-03 this was
/// both.** The removal's result was dropped on the floor (`let _ =`), so a
/// scope that could not be erased — the one outcome the whole function exists
/// to prevent — produced no line anywhere, at any level. On Windows that is
/// not untidiness but a live user-data leak: an open file cannot be deleted
/// there (`os error 32`), so `remove_dir_all` aborts on the first still-held
/// child and leaves the signed-out user's state on disk for the next sign-in
/// to re-adopt. The sibling sweep [`erase_all_account_scopes`] already names
/// its failing path for exactly this reason (`e2e-conventions.md` point 6 — a
/// failure must diagnose itself); this is the same duty on the per-actor leg.
///
/// So: a real removal logs at **info** (destroying a user's account-scoped
/// state is a deliberate, rare, irreversible act — it belongs in the log), a
/// failure logs at **warn** naming the path and the error, and an absent dir
/// stays silent. Returns the dirs that **survived** the sweep, so a caller or
/// test can assert on the outcome rather than re-listing the tree; an empty
/// vec means every base this actor had is gone.
pub fn erase_actor_state<'a>(
    bases: impl IntoIterator<Item = &'a Path>,
    actor_id_hex: &str,
) -> Vec<std::path::PathBuf> {
    let mut survivors = Vec::new();
    for base in bases {
        if let Ok(dir) = actor_state_dir(base, actor_id_hex) {
            match remove_tree_naming_held(&dir) {
                Ok(true) => tracing::info!("erase: removed account scope {}", dir.display()),
                Ok(false) => {}
                Err(held) => {
                    tracing::warn!(
                        "erase: account scope {} SURVIVED — still held: {} (on windows an \
                         open file cannot be deleted, so a still-held store leaves the \
                         signed-out user's state on disk)",
                        dir.display(),
                        describe_held(&held)
                    );
                    survivors.push(dir);
                }
            }
        }
    }
    survivors
}

/// Erase one actor's scoped stores — the per-account "remove this account"
/// affordance. Nothing outside `<base>/<actor-id-hex>/` is touched; an account
/// that never built scoped state is a no-op, not an error.
pub fn erase_account_scope(base: &Path, actor_id_hex: &str) -> Result<()> {
    let dir = actor_state_dir(base, actor_id_hex)?;
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// What a whole-base sweep actually did: how many actor scopes went, and
/// **which paths survived**.
///
/// The survivors are the point. `account-scoping.md` § Erasure follows scope's
/// corollary — *"the erase must SAY what it did"* — was applied to the
/// per-actor [`erase_actor_state`] on 2026-09-04 and not to this sibling, which
/// is the sweep a sign-out actually runs. A caller that learns only "something
/// failed" cannot tell the user *what* is still on their device, and every seat
/// that calls this turns the failure into a log line no user reads.
///
/// Empty `survivors` is the only thing that means the device is clean.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[must_use = "a sweep that is dropped un-inspected is exactly the defect this \
              type exists to prevent — a sign-out that reports success while \
              the user's data is still on the device"]
pub struct EraseSweep {
    /// Actor scopes successfully removed. A progress signal, never a
    /// head-count of accounts — see [`erase_all_account_scopes`].
    pub erased: usize,
    /// Every path this sweep tried to remove and could not. Non-empty means
    /// the signed-out user's data is still readable on this device.
    pub survivors: Vec<std::path::PathBuf>,
}

impl EraseSweep {
    /// True when nothing survived — the only clean outcome.
    pub fn is_clean(&self) -> bool {
        self.survivors.is_empty()
    }

    /// Fold another base's sweep into this one, for a caller that erases
    /// several roots and owes the user ONE answer.
    ///
    /// ⚠ **Survivors are DEDUPED by path, and that is a correctness rule, not
    /// tidiness.** `survivors.len()` is the number the user is shown
    /// (`account-scoping.md` § Erasure follows scope → *the count goes to the
    /// user*), and a survivor is a *location* — the same location reported by
    /// two sweeps is one fact about the device, not two. A caller folding
    /// sweeps over overlapping trees once told the user *"2 item(s)"* about one
    /// undeletable directory until this deduped (measured by the sign-out
    /// journey e2e). Over-reporting is the safer
    /// direction of the two, which is exactly why nothing else would have
    /// caught it.
    ///
    /// `erased` is NOT deduped: it counts removals, and two roots really can
    /// each remove a scope for the same actor — the doc on that field already
    /// says it is a progress signal rather than a head-count.
    pub fn absorb(&mut self, other: EraseSweep) {
        self.erased += other.erased;
        for path in other.survivors {
            if !self.survivors.contains(&path) {
                self.survivors.push(path);
            }
        }
    }
}

/// Erase **every** account's scoped stores — the all-accounts sign-out erase (`account-scoping.md` § Erasure follows
/// scope). Returns an [`EraseSweep`]: how many actor scopes were removed, and
/// what survived.
///
/// ⚠ **The sweep never stops early.** Until 2026-09-09 the actor-scope loop
/// `?`-returned on its first failure, so ONE undeletable file left every
/// later scope under that base unattempted — the same defect that had already
/// been fixed one level up (the FFI wrapper runs both roots deliberately), just
/// hidden one level down, where it silently cost more. On Windows an open file
/// cannot be deleted, and a scanner or indexer holding a handle is not our
/// defect to close, so a sweep that abandons the rest of its work on the first
/// such handle is exactly wrong: erase what CAN be erased, and report the rest.
///
/// Every `<base>/<64-hex>/` scope goes, and nothing else: anything that is not
/// an actor scope survives, which is precisely how install-scoped state (log
/// files, host-keyed TOFU pin stores) keeps its class-2 disposition through a
/// sign-out.
pub fn erase_all_account_scopes(base: &Path) -> EraseSweep {
    let mut sweep = EraseSweep::default();
    if base.is_dir() {
        match std::fs::read_dir(base) {
            Ok(entries) => {
                for entry in entries {
                    // A single unreadable entry is not a reason to abandon its
                    // siblings; we simply cannot name what we could not see.
                    let Ok(entry) = entry else {
                        tracing::warn!(
                            "erase: could not read an entry under {} — a scope may have been \
                             skipped unseen",
                            base.display()
                        );
                        continue;
                    };
                    // Only a well-formed actor scope — never a stray sibling directory.
                    if is_actor_scope(&entry) && sweep.remove_dir(entry.path(), "actor scope") {
                        sweep.erased += 1;
                    }
                }
            }
            // The base exists but will not enumerate, so we cannot even name the
            // scopes under it. The base itself is the honest survivor: something
            // of the user's is here and this sweep could not reach it.
            Err(e) => {
                tracing::warn!(
                    "erase: base {} SURVIVED — could not be listed: {e}",
                    base.display()
                );
                sweep.survivors.push(base.to_path_buf());
            }
        }
    }
    sweep
}

/// The actor ids whose scopes sit under `base` — exactly the `<base>/<64-hex>/`
/// dirs [`erase_all_account_scopes`] would remove, in no particular order.
///
/// This is the erase's **reach**, read before the erase runs, and it is what an
/// erasing gesture must ask about (`account-scoping.md` § Concurrent instances →
/// *An erase refuses while a sibling serves the account*). The registry is not
/// enough: the sweep removes every scope on disk whether or not the registry
/// still names it, a malformed index names nothing at all, and a store root
/// shared between apps holds accounts only a *sibling app's* registry lists.
/// Unreadable bases and entries are skipped — the same degrade-open posture as
/// the refusal this feeds.
pub fn account_scopes_under(base: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(is_actor_scope)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect()
}

/// A well-formed actor scope: a directory named by a canonical (lower-case,
/// 64-hex) actor id. One predicate, so the sweep and [`account_scopes_under`]
/// cannot disagree about what they are looking at.
fn is_actor_scope(entry: &std::fs::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| normalize_actor_hex(name).is_ok_and(|hex| hex == name))
        && entry.path().is_dir()
}

/// Remove `dir`'s whole tree, and on failure name EXACTLY what would not go.
///
/// `std::fs::remove_dir_all` returns on its first failing entry. On Windows
/// that is the norm rather than the edge: a file some live handle opened
/// without `FILE_SHARE_DELETE` (SQLite's share mode) cannot be deleted, so a
/// single still-open store used to strand every sibling file in the scope
/// too — and the only line anyone got named the scope directory, never the
/// holder, which left a sign-out residue undiagnosable (`e2e-conventions.md`
/// point 6: a failure must diagnose itself).
///
/// So after the fast path fails, walk what is left bottom-up, removing every
/// entry that CAN go, and return the entries that could not — the held files
/// themselves, not the directories that merely still contain them. An absent
/// tree is `Ok(false)` (nothing to remove), a removed one `Ok(true)`. A walk
/// that cannot list a directory reports that directory.
pub fn remove_tree_naming_held(dir: &Path) -> std::result::Result<bool, Vec<std::path::PathBuf>> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => return Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => {}
    }
    let mut held = Vec::new();
    remove_tree_walk(dir, &mut held);
    if held.is_empty() { Ok(true) } else { Err(held) }
}

/// [`remove_tree_naming_held`]'s slow path: returns whether `dir` itself went.
fn remove_tree_walk(dir: &Path, held: &mut Vec<std::path::PathBuf>) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => {
            held.push(dir.to_path_buf());
            return false;
        }
    };
    let mut all_gone = true;
    for entry in entries.flatten() {
        let path = entry.path();
        // `file_type` does not follow a symlink/junction, so a link is removed
        // as itself and its target is never walked.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let gone = if is_dir {
            remove_tree_walk(&path, held)
        } else {
            match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => {
                    held.push(path);
                    false
                }
            }
        };
        all_gone &= gone;
    }
    if !all_gone {
        return false;
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            held.push(dir.to_path_buf());
            false
        }
    }
}

/// The survivor log's naming of the held entries — capped, because a scope
/// holding thousands of files would otherwise flood the log with one line.
fn describe_held(held: &[std::path::PathBuf]) -> String {
    const SHOWN: usize = 8;
    let mut named: Vec<String> = held
        .iter()
        .take(SHOWN)
        .map(|p| p.display().to_string())
        .collect();
    if held.len() > SHOWN {
        named.push(format!("… {} more", held.len() - SHOWN));
    }
    named.join(", ")
}

impl EraseSweep {
    /// Remove a directory tree, recording it as a survivor if it will not go.
    /// Never propagates — see [`erase_all_account_scopes`]'s ⚠ note. Returns
    /// whether something was actually removed; the CALLER decides whether that
    /// counts toward [`Self::erased`], which is an actor-scope tally and not a
    /// count of everything this sweep deletes.
    ///
    /// The survivor is still the DIRECTORY — `survivors.len()` is the count
    /// the user is shown, and one scope is one place on their device — while
    /// the log names the held files inside it ([`remove_tree_naming_held`]).
    fn remove_dir(&mut self, path: std::path::PathBuf, what: &str) -> bool {
        match remove_tree_naming_held(&path) {
            Ok(true) => {
                tracing::info!("erase: removed {what} {}", path.display());
                return true;
            }
            Ok(false) => {}
            Err(held) => {
                // Name the path. A bare `io::Error` here is undiagnosable at the
                // call site: the caller knows only "the sweep failed", not WHICH
                // of an unbounded number of actor scopes, under which of the two
                // roots this erase covers. It cost a full windows e2e cycle to
                // find out that an `os error 32` reported by
                // `AccountStateDir.EraseAll` was one specific still-open file —
                // the shared sweep's own error should have said so
                // (`e2e-conventions.md` point 6: a failure must diagnose itself).
                // And name the HOLDER: the scope alone left a whole row's worth
                // of e2e runs guessing which store was still open.
                tracing::warn!(
                    "erase: {what} {} SURVIVED — still held: {} (on windows an open file \
                     cannot be deleted, so a still-held store leaves the signed-out user's \
                     state on disk)",
                    path.display(),
                    describe_held(&held)
                );
                self.survivors.push(path);
            }
        }
        false
    }
}

/// The rels in one folder's state DB whose **local content lacks a proven
/// record** — the set-level gate behind the iOS File Provider domain-removal
/// refusal (`file-sync.md` § Multi-account × File Provider, consequence 2: iOS
/// has no `.preserveDirtyUserData`, so a plain removal would let the OS discard
/// an un-uploaded edit; removal must refuse while any such row exists and let
/// the OS's pending-change retry drain it).
///
/// DB-only fidelity, on purpose: this runs in the *app* (a cross-process WAL
/// read of the extension's DB — the same sanctioned pattern as the Media-badge
/// `file_states` reads), which cannot hash the OS-owned replicated tree the way
/// [`SyncEngine::is_dehydration_safe`](crate::engine::SyncEngine) hashes a
/// resident folder. A row is un-recorded when it *carries local bytes* whose
/// record is unproven: mid-flight states (`LocallyModified` / `Uploading` /
/// `Conflicted`), or `Synced` without `recorded_content_hash` proving the head
/// matches the local identity (`upload_file` flips `Synced` *before* the
/// record lands, so a record that failed leaves exactly that shape — the same
/// fail-closed reading as `is_dehydration_safe`). `Placeholder` /
/// `Downloading` / `RemotelyModified` / `Deleted` rows carry no un-recorded
/// local bytes and never block removal.
///
/// A missing DB is an empty answer (a set never served has nothing to lose);
/// an unreadable one is an error the caller treats as "refuse" (fail closed).
pub fn set_unrecorded_rels(state_dir: &Path, folder_ref: FolderRef) -> Result<Vec<String>> {
    let path = folder_ref.state_db_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = SyncDb::open(&path)?;
    Ok(db
        .list_all()?
        .into_iter()
        .filter(entry_is_unrecorded)
        .map(|e| e.path)
        .collect())
}

fn entry_is_unrecorded(e: &SyncEntry) -> bool {
    match e.state {
        SyncState::LocallyModified | SyncState::Uploading | SyncState::Conflicted => true,
        SyncState::Synced => match (&e.local_hash, &e.recorded_content_hash) {
            (Some(local), Some(recorded)) => local != recorded,
            // Unproven head (record failed): fail closed.
            _ => true,
        },
        SyncState::RemotelyModified
        | SyncState::Downloading
        | SyncState::Placeholder
        | SyncState::Deleted
        | SyncState::LocallyDeleted => false,
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Local sync state database backed by SQLite.
pub struct SyncDb {
    conn: Connection,
}

/// The conflict kind of a permanently un-appliable catch-up change
/// (`file-sync.md` § 5) — recorded, skipped, reported candidate-free, and
/// cleared only when a later change to that path lands (`conflicts.md`
/// § Skipped catch-up changes reach the review list).
pub const CATCHUP_FAILED: &str = "catchup_failed";

/// The conflict kinds whose unresolved local row means "the next change on this
/// path is the propagated winner the user chose" — the only kinds the
/// verbatim-winner apply keys on. An allow-list on purpose: a kind left off it
/// costs an ordinary detection pass, a kind wrongly on it costs the user's
/// unpublished edit.
pub const CONFLICT_KINDS_WITH_PROPAGATED_WINNER: &[&str] = &["concurrent_edit", "delete_declined"];

/// [`CONFLICT_KINDS_WITH_PROPAGATED_WINNER`] as an SQL `IN (…)` list (the
/// kinds are compile-time literals, never input).
fn winner_kinds_sql() -> String {
    CONFLICT_KINDS_WITH_PROPAGATED_WINNER
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One local [`CATCHUP_FAILED`] row, as the report and cure paths need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedChange {
    pub id: i64,
    /// The plaintext relative path, or — [`Self::path_is_hash`] — the change
    /// row's own wire `path_hash` string.
    pub path: String,
    pub path_is_hash: bool,
    /// The change row's sealed label, verbatim (a `path_is_hash` row only).
    pub path_sealed: Option<Vec<u8>>,
    /// The content-free reason class.
    pub details: Option<String>,
    /// The nest's conflict id, once the report landed.
    pub nest_id: Option<i64>,
}

/// Per-file synchronisation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Synced,
    LocallyModified,
    RemotelyModified,
    Conflicted,
    Uploading,
    Downloading,
    Placeholder,
    Deleted,
    /// A seen placeholder the user deleted on this disk, whose delete the nest has
    /// not yet acked — the mirror of [`Self::LocallyModified`] for the delete
    /// direction (`delete-propagation.md` § *An offline placeholder delete
    /// propagates*, decision (d)). Never listed back onto the disk, never read as
    /// present, retried by every `reconcile`, tombstoned ([`Self::Deleted`]) only on
    /// the nest's ack.
    LocallyDeleted,
}

impl SyncState {
    /// Collapse this engine state onto the six-state **display** vocabulary the
    /// apps render (`fauna_core::format::SyncDisplayState`), or `None` when
    /// the file has no row to render at all (`Deleted`).
    ///
    /// The engine tracks nine states because it needs to distinguish *why* a
    /// file is out of sync (to pick the next action); a user only needs to know
    /// *where the bytes are*, so the two "diverged but not yet moved" states
    /// report the direction the engine is about to move them.
    ///
    /// Authority for this map: `docs/goal/behavior/file-sync.md` § Per-file
    /// sync-status display. Its first consumer is the converged apple engine
    /// host (§ Apple apps — convergence design); the windows sync service
    /// deliberately maps to the *OS shell-overlay* vocabulary instead
    /// (`file_status_from_state` — an OS-imposed set, not this one).
    ///
    /// `SyncDisplayState::LocalOnly` is intentionally unreachable here: a file
    /// the engine has a row for is already tracked, so "present locally, unknown
    /// to the nest" is a control-plane state, not an engine state.
    pub fn to_display(&self) -> Option<fauna_core::format::SyncDisplayState> {
        use fauna_core::format::SyncDisplayState as D;
        Some(match self {
            Self::Synced => D::Synced,
            Self::Uploading | Self::LocallyModified => D::Uploading,
            Self::Downloading | Self::RemotelyModified => D::Downloading,
            Self::Conflicted => D::Conflict,
            Self::Placeholder => D::RemoteOnly,
            // Gone here; the nest is merely still owed the record.
            Self::Deleted | Self::LocallyDeleted => return None,
        })
    }

    /// This row's **effective** state for the on-disk-presence question the overlay /
    /// display badge answers, given its recorded `size_bytes`.
    ///
    /// A **0-byte** file is the one special case. It has no bytes to hydrate, so on an
    /// on-demand root cfapi fires **no** FETCH_DATA when it is opened (measured on a live
    /// cfapi root, 2026-07-15: opening a 0-byte placeholder clears its OFFLINE/RECALL bits
    /// but delivers no callback). Its row therefore stays a `Placeholder` for the whole of
    /// its life — a `Synced`-while-absent row would be deleted by reconcile's delete-detection
    /// (data loss), so it deliberately is not one — yet it is *present-and-empty*, so every
    /// badge must read it as `Synced`. This is the single place that rule lives, applied by
    /// every derivation that has the row's size: the Windows overlay (`file_status_from_state`),
    /// the folder-badge fold (via [`SyncDb::descendant_states`]), and — when a macOS File
    /// Provider lands — [`Self::to_display`]. Every non-empty state is returned unchanged.
    pub fn effective_for_size(self, size_bytes: i64) -> Self {
        match self {
            Self::Placeholder if size_bytes == 0 => Self::Synced,
            other => other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::LocallyModified => "locally_modified",
            Self::RemotelyModified => "remotely_modified",
            Self::Conflicted => "conflicted",
            Self::Uploading => "uploading",
            Self::Downloading => "downloading",
            Self::Placeholder => "placeholder",
            Self::Deleted => "deleted",
            Self::LocallyDeleted => "locally_deleted",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "synced" => Some(Self::Synced),
            "locally_modified" => Some(Self::LocallyModified),
            "remotely_modified" => Some(Self::RemotelyModified),
            "conflicted" => Some(Self::Conflicted),
            "uploading" => Some(Self::Uploading),
            "downloading" => Some(Self::Downloading),
            "placeholder" => Some(Self::Placeholder),
            "deleted" => Some(Self::Deleted),
            "locally_deleted" => Some(Self::LocallyDeleted),
            _ => None,
        }
    }
}

/// An adoption marker's durable state on the engine's side
/// ([`SyncDb::adoption_state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptionState {
    /// Arrived on a device with no local state; adoption is in progress.
    Begun,
    /// Consumed — adopted to completion, or arrived where state was held.
    Spent,
}

/// How a row's dehydration proof (`recorded_content_hash`) was earned
/// (`file-sync.md` § Relay serving → *A holder keeps what it wrote*). In a
/// metadata-only folder the nest took no bytes, so only a body that came from
/// another holder is known to be fetchable again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofOrigin {
    /// This device recorded the head from its own bytes — and what every row
    /// that does not say reads as (the refusing direction).
    OwnRecord,
    /// The body was fetched from another holder: hydrate-on-open, a download
    /// apply, a peer-landed body the nest's row confirmed.
    Fetched,
}

impl ProofOrigin {
    fn as_column(self) -> &'static str {
        match self {
            Self::OwnRecord => "own",
            Self::Fetched => "fetched",
        }
    }

    /// NULL and any value this build does not know read as an own record.
    fn from_column(value: Option<&str>) -> Self {
        match value {
            Some("fetched") => Self::Fetched,
            _ => Self::OwnRecord,
        }
    }
}

/// A row from the `sync_entries` table.
#[derive(Debug, Clone)]
pub struct SyncEntry {
    pub path: String,
    pub local_hash: Option<ContentHash>,
    pub remote_hash: Option<ContentHash>,
    pub manifest_hash: Option<ContentHash>,
    pub state: SyncState,
    pub local_mtime: i64,
    pub remote_mtime: i64,
    pub size_bytes: i64,
    pub version_num: i64,
    pub last_synced_at: i64,
    pub pinned: bool,
    /// M2 content-key generation the chunks of this version were sealed under
    /// (cross-user shared folders); `None` for owner-only sets.
    /// The read path tries every `keys_for(version)` candidate and fails closed if absent.
    pub content_key_version: Option<u64>,
    /// Owner-only re-seal marker: `true` once this
    /// entry's recorded manifest is known sealed (`stored_hashes` present).
    /// Local cache only — a lost marker merely re-checks.
    pub owner_sealed: bool,
    /// Post-succession corpus re-seal marker: `true` once this entry's chunks are
    /// known to open under the engine's **current** owner root
    /// (`SyncEngine::reseal_predecessor_sealed`). Distinct from
    /// [`Self::owner_sealed`] — that one answers "sealed at all vs the plaintext
    /// corpus of a public-audience folder", this one answers "sealed under *which* root". Local cache only
    /// — a lost marker merely re-checks.
    pub current_root_sealed: bool,
    /// The content hash that `manifest_hash` (the recorded head) reassembles to,
    /// set only where the head is proven to match the local content (record
    /// success, hydrate-on-open, download apply). `is_dehydration_safe` requires
    /// `recorded_content_hash == disk hash` so freeing the bytes is provably
    /// lossless (a re-hydration fetches `manifest_hash`). `None` for a row
    /// whose head was never proven — e.g. a record that FAILED, which advances `local_hash` but leaves `manifest_hash` at the
    /// old base; the gate fails closed on `None`.
    pub recorded_content_hash: Option<ContentHash>,
    /// How [`Self::recorded_content_hash`] was earned — meaningful only while
    /// that proof is present. An unmarked row reads [`ProofOrigin::OwnRecord`].
    pub recorded_proof_origin: ProofOrigin,
    /// Hex hash of the **recorded head's** sealed thumbnail blob, or `None` when
    /// the head carries no thumbnail (a non-image, a pre-producer record) or
    /// when this device has not learned it yet.
    ///
    /// A device-local cache of `sync_changes.thumbnail_hash`, the same recorded
    /// head [`Self::manifest_hash`] and [`Self::size_bytes`] cache. Its consumer
    /// is the post-succession re-seal: when the thumbnailer cannot regenerate
    /// (feature off, or the upload failed) the pass MOVES this blob off the
    /// retired root instead of recording `None` over it and stranding the
    /// pointer. `None` is always safe — it means "no move available", never a
    /// wrong move.
    pub thumbnail_hash: Option<String>,
    /// The seen mark — this engine put (or a scan observed) this row's placeholder on
    /// the disk. Read only by the delete universe (`delete-propagation.md` § *An
    /// offline placeholder delete propagates*, decision (a)); written by
    /// [`SyncDb::mark_seen`] and never by [`SyncDb::upsert_entry`].
    pub seen_on_disk: bool,
    /// The identity the recorded head ([`Self::manifest_hash`]) was signed as
    /// (`writer-signed-change-records.md` ruling (11)(d)); `None` = unknown,
    /// which is offered no owner root. Written by
    /// [`SyncDb::set_head_signed_as`] and [`SyncDb::update_recorded_head`],
    /// cleared by [`SyncDb::upsert_entry`] whenever the manifest moves.
    pub head_signed_as: Option<[u8; 32]>,
}

/// The one `sync_entries` SELECT head every [`SyncEntry`] read shares, paired
/// with [`map_sync_entry_row`] — one column list + one mapping, so a reader
/// added later cannot drift from [`SyncDb::get_entry`]'s.
const SYNC_ENTRY_SELECT: &str = "SELECT path, local_hash, remote_hash, manifest_hash, state,
        local_mtime, remote_mtime, size_bytes, version_num,
        last_synced_at, pinned, content_key_version, owner_sealed,
        current_root_sealed, recorded_content_hash, thumbnail_hash, seen_on_disk,
        head_signed_as, recorded_proof_origin
 FROM sync_entries";

/// Map one [`SYNC_ENTRY_SELECT`] row to a [`SyncEntry`].
fn map_sync_entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncEntry> {
    Ok(SyncEntry {
        path: row.get(0)?,
        local_hash: opt_blob_to_hash(row.get(1)?),
        remote_hash: opt_blob_to_hash(row.get(2)?),
        manifest_hash: opt_blob_to_hash(row.get(3)?),
        state: SyncState::from_str(&row.get::<_, String>(4)?).unwrap_or(SyncState::Synced),
        local_mtime: row.get(5)?,
        remote_mtime: row.get(6)?,
        size_bytes: row.get(7)?,
        version_num: row.get(8)?,
        last_synced_at: row.get(9)?,
        pinned: row.get::<_, i64>(10)? != 0,
        content_key_version: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        owner_sealed: row.get::<_, i64>(12)? != 0,
        current_root_sealed: row.get::<_, i64>(13)? != 0,
        recorded_content_hash: opt_blob_to_hash(row.get(14)?),
        recorded_proof_origin: ProofOrigin::from_column(
            row.get::<_, Option<String>>(18)?.as_deref(),
        ),
        thumbnail_hash: row.get(15)?,
        seen_on_disk: row.get::<_, i64>(16)? != 0,
        head_signed_as: row
            .get::<_, Option<Vec<u8>>>(17)?
            .and_then(|b| b.try_into().ok()),
    })
}

/// One retained manifest (`own_manifests`) — the serve-side byte half's
/// manifest read: the canonical bytes, the path whose seal produced them (the
/// chunk range source), and the M2 generation the chunks sealed under.
#[derive(Debug, Clone)]
pub struct RetainedManifest {
    pub bytes: Vec<u8>,
    pub path: String,
    pub content_key_version: Option<u64>,
}

/// One chunk of a body this seat holds (`held_chunks`): its store key and
/// where its plaintext sits in the body, with the hash that range must still
/// hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldChunk {
    pub store_key: [u8; 32],
    pub offset: u64,
    pub len: u64,
    pub plain_hash: ContentHash,
}

/// One `held_chunks` row as the serve reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldChunkRow {
    pub path: String,
    pub chunk: HeldChunk,
    /// The M2 generation the chunk sealed under — `None` for an owner-sealed
    /// or public body.
    pub content_key_version: Option<u64>,
}

/// One row of `segment_backup_state` (Plan 6 T5).
///
/// Records what a backup coordinator has already uploaded to a
/// particular (kind, scope_id, segment_id, member_actor_id, dest_id) tuple —
/// the record count and byte size, so the next `fauna.segments.list` reply
/// can be diffed without re-fetching the segment body.
#[derive(Debug, Clone)]
pub struct MailBackupSegmentState {
    pub last_chunk_count: u64,
    pub last_byte_size: u64,
    pub last_synced_at: u64,
    /// Plaintext size of the `.meta` sidecar pushed beside this segment's
    /// `.dat`, or `None` when no pushed sidecar is known for it (a custodian
    /// view holding the `.dat` without its `.meta`). `None` is what the shared diff reads as "backfill the
    /// sidecar" — see the `segment_backup_state` schema comment.
    pub last_meta_size: Option<u64>,
}

/// A `backup_destination_seen` row — a destination this coordinator has
/// uploaded to, with the reconnect info its removal-reconcile needs once the
/// destination has left the owner's configured set.
#[derive(Debug, Clone)]
pub struct SeenBackupDestination {
    pub dest_id: String,
    pub dest_url: String,
    /// The owner's reserved custody-copy set name at that destination.
    pub folder: String,
    /// The destination's 32-byte nest pubkey — the pin the nest arm's teardown
    /// dials against.
    pub nest_id: Vec<u8>,
}

/// One segment a coordinator's own state says it placed at a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedSegment {
    pub kind: String,
    pub scope_id: Vec<u8>,
    pub segment_id: u32,
    /// Whether the `.meta` sidecar was pushed beside the `.dat`
    /// (`MailBackupSegmentState::last_meta_size` is `Some`). A teardown
    /// tombstones the sidecar's path only when this is `true`: a row written
    /// before the sidecar widening never placed one, and a tombstone for a
    /// path that was never created is bookkeeping noise on the destination.
    pub meta_pushed: bool,
}

/// What a coordinator's own state says it has placed at one destination — the
/// removal teardown's work list ([`SyncDb::list_backup_paths_for_destination`]).
#[derive(Debug, Clone, Default)]
pub struct BackupPathsAtDestination {
    /// One entry per uploaded segment.
    pub segments: Vec<PlacedSegment>,
    /// `(kind, scope_id)` per written `manifest.<kind>` mirror.
    pub manifests: Vec<(String, Vec<u8>)>,
}

/// A row from the `transfer_queue` table.
#[derive(Debug, Clone)]
pub struct TransferEntry {
    pub id: i64,
    pub path: String,
    pub direction: String,
    pub chunk_hash: ContentHash,
    pub priority: i64,
    pub status: String,
    pub retry_count: i64,
    pub created_at: i64,
    pub last_attempt_at: i64,
}

/// This engine's aggregate transfer backlog + freshness stamps
/// ([`SyncDb::transfer_backlog`]) — the per-set numbers behind the pipe's
/// `SyncStatusInfo`/`EngineInfo` fields (`sync-agent.md` § Local agent health).
///
/// `files_pending`/`bytes_pending` are the fold of the per-file **display**
/// states: a file counts exactly when its badge reads `Uploading`/`Downloading`
/// ([`SyncState::to_display`], the one owner of that map), so the aggregate can
/// never disagree with the badges a user sees. `bytes_pending` is whole-file
/// size (the "3 items, 2.4 GB" queue reading) — remaining-chunk byte counts
/// don't exist locally (manifests are remote) and would double as a progress
/// bar, which is the push channel's job (`ProgressEvent`), not the queue's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferBacklog {
    /// Tracked files whose display state is `Uploading` or `Downloading`.
    pub files_pending: u64,
    /// Total `size_bytes` of those files (whole-file, not remaining chunks).
    pub bytes_pending: u64,
    /// Last completed file-level content transfer (upload or download), epoch
    /// secs — advances only when content actually moved
    /// ([`SyncDb::mark_transfer_completed`]), never on entry-state writes; the
    /// `segment_backup_state.last_content_synced_at` honesty rule.
    pub last_transfer_at: Option<i64>,
    /// Last converge/pull pass that ended with an empty backlog, epoch secs
    /// ([`SyncDb::mark_clean_pass_if_drained`]) — "this device verified itself
    /// consistent with the nest", which advances while idle-but-checking.
    pub last_clean_pass_at: Option<i64>,
}

impl TransferBacklog {
    /// The wire's `last_sync`: when was this device last **known consistent**
    /// with the nest — the freshest of the two stamps. An idle device that keeps
    /// passing clean stays fresh; a device with a stuck backlog goes honestly
    /// stale.
    pub fn last_sync_at(&self) -> Option<i64> {
        match (self.last_transfer_at, self.last_clean_pass_at) {
            (Some(t), Some(c)) => Some(t.max(c)),
            (t, c) => t.or(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_epoch_secs() -> i64 {
    Timestamp::now_secs()
}

fn hash_to_blob(h: &ContentHash) -> Vec<u8> {
    h.digest().to_vec()
}

fn blob_to_hash(v: Vec<u8>) -> Option<ContentHash> {
    let arr: [u8; 32] = v.try_into().ok()?;
    Some(ContentHash::from_digest_raw(arr))
}

fn opt_hash_to_blob(h: &Option<ContentHash>) -> Option<Vec<u8>> {
    h.as_ref().map(hash_to_blob)
}

fn opt_blob_to_hash(v: Option<Vec<u8>>) -> Option<ContentHash> {
    v.and_then(blob_to_hash)
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// Compute the minimum delay (seconds) before retrying a transfer based on attempt count.
/// Schedule: 0, 60s, 5m, 30m, 4h (capped).
pub fn retry_backoff_secs(retry_count: i64) -> i64 {
    match retry_count {
        0 => 0,
        1 => 60,
        2 => 300,
        3 => 1800,
        _ => 14400,
    }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl SyncDb {
    /// Open (or create) the database at `path` and run migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // SQLite creates the database *file* but not intermediate *directories*.
        // On a fresh machine the sync state dir (e.g. %LOCALAPPDATA%\Fauna\sync\)
        // does not exist yet, so create it here — the doc comment promises "Open
        // (or create)" and the Windows hydration / pipe-add paths and the Linux
        // driver all rely on it (works-out-of-the-box).
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create sync db parent dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).context("open sqlite")?;
        // Wait (rather than immediately error) on a locked DB: the in-process
        // Linux driver may briefly open a state DB while a previous, just-
        // cancelled driver's engine is still winding down on its own thread
        // (re-auth across the e2e reset cycle). 5s comfortably covers that.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set busy_timeout")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Apply the genesis block, then reconcile additive columns.
    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA journal_mode=WAL;")
            .context("enable WAL mode")?;

        self.conn
            .execute_batch(CREATE_TABLES_SQL)
            .context("create sync db tables")?;

        // The additive column reconciler every genesis in the tree pairs with
        // its block (the nest's, the federation bridges'): a column the block
        // declares that a long-lived `sync.db` lacks is added here.
        fauna_core::sqlite_schema_meta::reconcile_added_columns(&self.conn, |reference| {
            Ok(reference.execute_batch(CREATE_TABLES_SQL)?)
        })
        .context("reconcile sync db columns")?;

        Ok(())
    }

    /// Query the current SQLite journal mode.
    pub fn journal_mode(&self) -> Result<String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .context("query journal_mode")?;
        Ok(mode)
    }

    // ---- device_identity ----

    /// Get the persisted device ID, if one has been generated.
    pub fn get_device_id(&self) -> Result<Option<[u8; 32]>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM device_identity WHERE key = 'device_id'")?;
        let mut rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        match rows.next() {
            Some(Ok(v)) => {
                let arr: [u8; 32] = v
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("stored device_id is not 32 bytes"))?;
                Ok(Some(arr))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Persist a device ID. Called once on first run.
    pub fn set_device_id(&self, id: &[u8; 32]) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO device_identity (key, value) VALUES ('device_id', ?1)",
                params![id.as_slice()],
            )
            .context("store device_id")?;
        Ok(())
    }

    /// Generate a random 32-byte device ID using SQLite's CSPRNG,
    /// persist it, and return it.
    pub fn generate_device_id(&self) -> Result<[u8; 32]> {
        let id: Vec<u8> = self
            .conn
            .query_row("SELECT randomblob(32)", [], |row| row.get(0))
            .context("generate random device ID")?;
        let arr: [u8; 32] = id
            .try_into()
            .map_err(|_| anyhow::anyhow!("randomblob(32) did not return 32 bytes"))?;
        self.set_device_id(&arr)?;
        Ok(arr)
    }

    /// Get the device ID, generating and persisting one if needed.
    pub fn get_or_create_device_id(&self) -> Result<[u8; 32]> {
        if let Some(id) = self.get_device_id()? {
            return Ok(id);
        }
        self.generate_device_id()
    }

    // ---- sync_entries ----

    /// Insert or update a sync entry.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_entry(
        &self,
        path: &str,
        local_hash: Option<ContentHash>,
        remote_hash: Option<ContentHash>,
        manifest_hash: Option<ContentHash>,
        state: SyncState,
        local_mtime: i64,
        remote_mtime: i64,
        size_bytes: i64,
        version_num: i64,
        content_key_version: Option<u64>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO sync_entries
                    (path, local_hash, remote_hash, manifest_hash, state,
                     local_mtime, remote_mtime, size_bytes, version_num, last_synced_at,
                     content_key_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(path) DO UPDATE SET
                    local_hash    = excluded.local_hash,
                    remote_hash   = excluded.remote_hash,
                    head_signed_as = CASE WHEN manifest_hash IS excluded.manifest_hash
                                          THEN head_signed_as ELSE NULL END,
                    manifest_hash = excluded.manifest_hash,
                    state         = excluded.state,
                    local_mtime   = excluded.local_mtime,
                    remote_mtime  = excluded.remote_mtime,
                    size_bytes    = excluded.size_bytes,
                    version_num   = excluded.version_num,
                    last_synced_at = excluded.last_synced_at,
                    content_key_version = excluded.content_key_version",
                params![
                    path,
                    opt_hash_to_blob(&local_hash),
                    opt_hash_to_blob(&remote_hash),
                    opt_hash_to_blob(&manifest_hash),
                    state.as_str(),
                    local_mtime,
                    remote_mtime,
                    size_bytes,
                    version_num,
                    now_epoch_secs(),
                    content_key_version.map(|v| v as i64),
                ],
            )
            .context("upsert sync entry")?;
        Ok(())
    }

    /// Get a sync entry by path.
    pub fn get_entry(&self, path: &str) -> Result<Option<SyncEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SYNC_ENTRY_SELECT} WHERE path = ?1"))?;
        let mut rows = stmt.query_map(params![path], map_sync_entry_row)?;
        match rows.next() {
            Some(Ok(entry)) => Ok(Some(entry)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Mark an entry's recorded manifest as known sealed (`stored_hashes`
    /// present) — the owner-only re-seal pass's per-entry done marker
    /// (`SyncEngine::reseal_owner_only_plaintext`). Device-local cache: a lost
    /// marker merely re-checks on the next pass.
    pub fn mark_owner_sealed(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET owner_sealed = 1 WHERE path = ?1",
                params![path],
            )
            .context("mark owner_sealed")?;
        Ok(())
    }

    /// Mark an entry's chunks as known sealed under the engine's **current**
    /// owner root — the post-succession corpus re-seal's per-entry done marker
    /// (`SyncEngine::reseal_predecessor_sealed`). Device-local cache: a lost
    /// marker merely re-checks, and the re-check is one chunk fetch.
    pub fn mark_current_root_sealed(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET current_root_sealed = 1 WHERE path = ?1",
                params![path],
            )
            .context("mark current_root_sealed")?;
        Ok(())
    }

    /// How many rows hold bytes at rest that
    /// [`Self::list_pending_current_root_reseal`] **declined to classify** —
    /// the beside-conjunct that makes that list's ambiguity
    /// unrepresentable.
    ///
    /// That list is empty in two different situations: nothing is owed, or
    /// nothing was *looked at*. It sees only `Synced`/`Placeholder` rows with a
    /// recorded `manifest_hash`, so two kinds of row are invisible to it while
    /// still resting under whatever root sealed them:
    ///
    /// - **`Synced`/`Placeholder` with no recorded manifest.** The `is_some()`
    ///   filter was added for a *resolution* concern — the pass cannot fetch
    ///   what it cannot name — and that is not the same question as *may this
    ///   device discard the only key that opens it*. Such a row never gains a
    ///   manifest by itself, so it never flips the predicate back: ruling 2's
    ///   self-healing argument does not reach it.
    /// - **The in-flight states** (`Uploading`/`Downloading`/`Conflicted` and
    ///   the two `*Modified`). Excluding them from the *pass* is right — their
    ///   bytes are mid-move — but a license read while one is in flight would be
    ///   deciding about content it never examined.
    ///
    /// `Deleted` is deliberately **not** counted: it has nothing at rest, which
    /// is the one honest exclusion of the four.
    ///
    /// A non-zero count means *cannot tell*, never *owed* — the caller's job is
    /// only to refuse the license, and the pass is unaffected.
    pub fn count_unclassified_at_rest(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_entries
                 WHERE state != ?1
                   AND (state NOT IN (?2, ?3) OR manifest_hash IS NULL)",
                params![
                    SyncState::Deleted.as_str(),
                    SyncState::Synced.as_str(),
                    SyncState::Placeholder.as_str()
                ],
                |row| row.get(0),
            )
            .context("count unclassified at-rest entries")?;
        Ok(n as u64)
    }

    /// Clear **every** `current_root_sealed` sentinel — the root-generation
    /// guard's hammer (`sync-agent.md` § Credential model → *Bound (3)'s
    /// enforcement design*, ruling 5, driven by
    /// [`SyncDb::adopt_sentinel_root_actor`]).
    ///
    /// The sentinel says "sealed under the **current** root", which stops being
    /// true the moment the current root changes. Nothing cleared it before, so a
    /// second succession left B-era entries stamped and the re-seal pass skipped
    /// exactly the corpus that needed it. Clearing costs one chunk fetch per
    /// entry on the next pass and nothing else — the marker is a device-local
    /// cache, never user data.
    pub fn clear_current_root_sealed(&self) -> Result<()> {
        self.conn
            .execute("UPDATE sync_entries SET current_root_sealed = 0", [])
            .context("clear current_root_sealed")?;
        Ok(())
    }

    /// Paths whose chunks are **not** yet known to open under the current owner
    /// root — the post-succession re-seal's remaining work, and the observable a
    /// completion check reads (`succession-aftermath.md` § Re-key scope). Counts
    /// entries that actually have something at rest.
    ///
    /// ⚠ **`Placeholder` is listed, and that is the point.** A cloud-only
    /// placeholder's bytes rest on the nest under whatever root sealed them, so
    /// it is *more* owed a re-seal than a materialized row, not less — and a
    /// successor restoring onto a fresh device holds nothing but placeholders,
    /// which is the case the whole aftermath exists for. Listing only `Synced`
    /// (this query's first shape) made the observable structurally blind to its
    /// own dominant input: the pass reported "nothing owed" for exactly the
    /// corpus still sealed to a retired identity, which would in turn have
    /// licensed `sync-agent.md` bound (3) to drop the only keys that can open
    /// it.
    ///
    /// The in-flight states (`Uploading`/`Downloading`/`Conflicted`/the two
    /// `*Modified`) are deliberately excluded: their at-rest bytes are mid-move,
    /// so re-sealing them would race the move that is already re-writing them.
    /// They settle into `Synced`/`Placeholder` and are picked up by the next
    /// pass. `Deleted` has nothing at rest to re-seal.
    pub fn list_pending_current_root_reseal(&self) -> Result<Vec<SyncEntry>> {
        let mut at_rest = self.list_by_state(SyncState::Synced)?;
        at_rest.extend(self.list_by_state(SyncState::Placeholder)?);
        Ok(at_rest
            .into_iter()
            .filter(|e| !e.current_root_sealed && e.manifest_hash.is_some())
            .collect())
    }

    /// Stamp the row with its just-RECORDED head: manifest + size + generation.
    /// Only called after `fauna.sync.changes.record` succeeded for exactly this
    /// content — the recorded manifest is then both the hydration anchor and the
    /// merge base, so the next fold's head comparison sees its own echo as
    /// current rather than as a stale hydrated copy
    /// (`SyncEngine::commit_recorded_head`).
    pub fn update_recorded_head(
        &self,
        path: &str,
        manifest_hash: &ContentHash,
        size_bytes: i64,
        content_key_version: Option<u64>,
        signed_as: &[u8; 32],
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET manifest_hash = ?1, size_bytes = ?2,
                        content_key_version = ?3, head_signed_as = ?5 WHERE path = ?4",
                params![
                    opt_hash_to_blob(&Some(*manifest_hash)),
                    size_bytes,
                    content_key_version.map(|v| v as i64),
                    path,
                    &signed_as[..],
                ],
            )
            .context("update recorded head")?;
        self.release_signer_bound(path)
    }

    /// Record who `path`'s head was signed as — only while the entry still
    /// holds `manifest_hash`, the head the verdict was about
    /// (`writer-signed-change-records.md` ruling (11)(d)). Returns whether a
    /// row was stamped.
    pub fn set_head_signed_as(
        &self,
        path: &str,
        manifest_hash: &ContentHash,
        signed_as: &[u8; 32],
    ) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "UPDATE sync_entries SET head_signed_as = ?1
                 WHERE path = ?2 AND manifest_hash = ?3",
                params![
                    &signed_as[..],
                    path,
                    opt_hash_to_blob(&Some(*manifest_hash))
                ],
            )
            .context("set head_signed_as")?;
        if n > 0 {
            self.release_signer_bound(path)?;
        }
        Ok(n > 0)
    }

    /// Note `path` as held by the take-over's `SIGNER_BOUND` skip (ruling
    /// (11)(e)): its head `manifest_hash` opened under none of its signer's
    /// roots, so the upload choke point refuses the path
    /// ([`Self::is_signer_bound`]).
    pub fn note_signer_bound(&self, path: &str, manifest_hash: Option<&ContentHash>) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO signer_bound_holds (path, manifest_hash) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET manifest_hash = excluded.manifest_hash",
                params![path, opt_hash_to_blob(&manifest_hash.copied())],
            )
            .context("note signer_bound hold")?;
        Ok(())
    }

    /// Whether `path` carries the `SIGNER_BOUND` hold [`Self::note_signer_bound`]
    /// wrote.
    pub fn is_signer_bound(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM signer_bound_holds WHERE path = ?1)",
                params![path],
                |row| row.get::<_, i64>(0),
            )
            .context("probe signer_bound_holds")?
            != 0)
    }

    /// Lift `path`'s `SIGNER_BOUND` hold — the path gained a head this device
    /// vouches for. A no-op when none is held.
    pub fn release_signer_bound(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM signer_bound_holds WHERE path = ?1",
                params![path],
            )
            .context("release signer_bound hold")?;
        Ok(())
    }

    /// The recorded signer of every live entry whose head is `manifest_hash`
    /// — one element per entry, `None` where none is recorded. Empty = no
    /// entry holds the manifest.
    pub fn head_signers_of_manifest(
        &self,
        manifest_hash: &ContentHash,
    ) -> Result<Vec<Option<[u8; 32]>>> {
        let mut stmt = self.conn.prepare(
            "SELECT head_signed_as FROM sync_entries
             WHERE manifest_hash = ?1 AND state != ?2",
        )?;
        let rows = stmt.query_map(
            params![
                opt_hash_to_blob(&Some(*manifest_hash)),
                SyncState::Deleted.as_str()
            ],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.and_then(|b| b.try_into().ok()));
        }
        Ok(out)
    }

    /// Whether a succession take-over pass is owed on this set — set when the
    /// reader's binding gains a nonce, cleared when a pass completes (ruling
    /// (11)(d)).
    pub fn take_over_owed(&self) -> Result<bool> {
        Ok(self.meta_get("take_over_owed")?.is_some())
    }

    /// Set or clear the marker [`Self::take_over_owed`] reads.
    pub fn set_take_over_owed(&self, owed: bool) -> Result<()> {
        if owed {
            self.meta_put("take_over_owed", "1")
        } else {
            self.meta_del("take_over_owed")
        }
    }

    /// What this engine decided about the adoption marker naming `nonce`
    /// (ruling (11)(d)): `None` = never seen; [`AdoptionState::Begun`] = it
    /// arrived on a device holding no local state, and the nest's history is
    /// being adopted (a crash part-way resumes); [`AdoptionState::Spent`] =
    /// done, or it arrived on a device that held state — never adopted again.
    pub fn adoption_state(&self, nonce: &[u8; 32]) -> Result<Option<AdoptionState>> {
        Ok(
            match self
                .meta_get(&format!("adoption:{}", fauna_core::hex32::encode(nonce)))?
                .as_deref()
            {
                Some("begun") => Some(AdoptionState::Begun),
                Some(_) => Some(AdoptionState::Spent),
                None => None,
            },
        )
    }

    /// Record [`Self::adoption_state`] for the marker naming `nonce`.
    pub fn set_adoption_state(&self, nonce: &[u8; 32], state: AdoptionState) -> Result<()> {
        self.meta_put(
            &format!("adoption:{}", fauna_core::hex32::encode(nonce)),
            match state {
                AdoptionState::Begun => "begun",
                AdoptionState::Spent => "spent",
            },
        )
    }

    /// Whether this `SyncDb` holds any entry at all — the take-over's *holds
    /// local state* (ruling (11)(d): synced, placeholder, or awaiting
    /// hydration alike).
    pub fn has_any_entry(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM sync_entries)", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("probe sync_entries")?
            != 0)
    }

    /// Stamp the row's cache of the **recorded head's** thumbnail pointer.
    ///
    /// Call it wherever this device learns what the head carries: a change it
    /// recorded itself, or a change it pulled. `None` clears the cache, which is
    /// correct — a head recorded without a thumbnail has none, and a stale
    /// pointer would send the re-seal chasing a blob the head no longer names.
    ///
    /// Deliberately its own setter rather than a parameter on
    /// [`Self::upsert_entry`]: that signature is reached from a dozen call sites
    /// that know nothing about thumbnails, and widening it would make every one
    /// of them assert something it cannot see.
    pub fn set_thumbnail_hash(&self, path: &str, thumbnail_hash: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET thumbnail_hash = ?1 WHERE path = ?2",
                params![thumbnail_hash, path],
            )
            .context("set thumbnail_hash")?;
        Ok(())
    }

    /// Stamp `recorded_content_hash` from the row's current `local_hash` — the
    /// dehydration gate's proof that the recorded head (`manifest_hash`)
    /// reassembles to the local content. Call it at each **proven-head** write
    /// (record success, hydrate-on-open, download apply), where `local_hash`
    /// already equals the content the head represents; the gate then allows a
    /// lossless free-up (`SyncEngine::is_dehydration_safe`). Deliberately reads
    /// `local_hash` rather than taking a value, so the two can never disagree —
    /// and so a caller cannot stamp a proof the row's own identity contradicts.
    /// A no-op (0 rows) if the path is gone. NULL `local_hash` copies through as
    /// NULL, keeping the gate fail-closed.
    ///
    /// `origin` says how the proof was earned and is written with it: the gate
    /// keeps an [`ProofOrigin::OwnRecord`] body in a metadata-only folder. A
    /// site that cannot say the body came from another holder passes
    /// `OwnRecord`.
    pub fn stamp_recorded_content_from_local(&self, path: &str, origin: ProofOrigin) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET recorded_content_hash = local_hash,
                        recorded_proof_origin = ?2
                  WHERE path = ?1",
                params![path, origin.as_column()],
            )
            .context("stamp recorded_content_hash from local_hash")?;
        Ok(())
    }

    /// Clear `recorded_content_hash` — the inverse of
    /// [`Self::stamp_recorded_content_from_local`]. Call it when re-pointing a row
    /// at a head whose content is **not** proven present on this device (a File
    /// Provider conflict where the merged / incoming version wins: the bytes the OS
    /// holds are the loser, so the row must carry no dehydration proof). With the
    /// proof gone, [`crate::provider_face::content_version`] falls back to the
    /// `manifest_hash`, which differs from what the OS wrote — the changed
    /// `contentVersion` is exactly what makes the OS re-fetch the winning content.
    /// A no-op (0 rows) if the path is gone.
    pub fn clear_recorded_content_hash(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET recorded_content_hash = NULL,
                        recorded_proof_origin = NULL
                  WHERE path = ?1",
                params![path],
            )
            .context("clear recorded_content_hash")?;
        Ok(())
    }

    /// Set the seen mark on every row in `paths` (decision (a) of
    /// `delete-propagation.md` § *An offline placeholder delete propagates*): this
    /// engine put their placeholders on the disk, or a scan observed them there.
    /// A path with no row is skipped — a directory entry of a population listing
    /// has none. One transaction, so a whole listing marks in one write.
    pub fn mark_seen<S: AsRef<str>>(&self, paths: &[S]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin mark_seen")?;
        {
            let mut stmt =
                tx.prepare("UPDATE sync_entries SET seen_on_disk = 1 WHERE path = ?1")?;
            for p in paths {
                stmt.execute(params![p.as_ref()])
                    .context("mark seen_on_disk")?;
            }
        }
        tx.commit().context("commit mark_seen")?;
        Ok(())
    }

    /// Clear one row's seen mark — its placeholder is provably NOT on the disk by
    /// the engine's own account (a remotely moved head resurrecting a
    /// [`SyncState::LocallyDeleted`] row as a fresh, never-listed placeholder).
    pub fn clear_seen(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET seen_on_disk = 0 WHERE path = ?1",
                params![path],
            )
            .context("clear seen_on_disk")?;
        Ok(())
    }

    /// Clear **every** seen mark — decision (e)'s hygiene: the host is about to
    /// remove (or the OS already removed) every cloud-only placeholder of this set,
    /// and that removal is the product's own act, never a delete. Called BEFORE the
    /// removal: a crash in between leaves placeholders on the disk unmarked, and the
    /// next scan re-marks them; the reverse order would leave marks without files.
    pub fn clear_seen_all(&self) -> Result<()> {
        self.conn
            .execute("UPDATE sync_entries SET seen_on_disk = 0", [])
            .context("clear every seen_on_disk")?;
        Ok(())
    }

    /// Set or clear a row's pin — the user's *keep on this device* for one file,
    /// where the platform keeps no pin of its own (the linux FUSE root;
    /// `on-demand-files.md` § Linux FUSE binding: pins live in the row). `false`
    /// (no write) when the path has no row: a pin needs a tracked file.
    /// `upsert_entry`'s `ON CONFLICT` omits the column, so a pin survives every
    /// later state write.
    pub fn set_pinned(&self, path: &str, pinned: bool) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "UPDATE sync_entries SET pinned = ?1 WHERE path = ?2",
                params![i64::from(pinned), path],
            )
            .context("set pinned")?;
        Ok(n > 0)
    }

    /// The paths of every pinned `Placeholder` row — the pinned files whose bytes
    /// are not on this device yet, each owed an eager hydration.
    pub fn pinned_placeholder_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM sync_entries WHERE pinned = 1 AND state = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![SyncState::Placeholder.as_str()], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Update just the state field for an entry.
    pub fn update_state(&self, path: &str, state: SyncState) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET state = ?1 WHERE path = ?2",
                params![state.as_str(), path],
            )
            .context("update sync state")?;
        Ok(())
    }

    /// Every tracked entry, in path order — the per-file **display** read
    /// (`SyncState::to_display()`), which needs all states at once rather than
    /// one query per state. Excludes nothing: `Deleted` rows come back too, and
    /// the display map drops them (a deleted file renders no row).
    pub fn list_all(&self) -> Result<Vec<SyncEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SYNC_ENTRY_SELECT} ORDER BY path"))?;
        let rows = stmt.query_map([], map_sync_entry_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// List all entries with the given state.
    pub fn list_by_state(&self, state: SyncState) -> Result<Vec<SyncEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SYNC_ENTRY_SELECT} WHERE state = ?1"))?;
        let rows = stmt.query_map(params![state.as_str()], map_sync_entry_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The [`SyncState`] of every tracked descendant of a folder, for the Windows
    /// shell overlay's **folder-badge** severity fold (`apps/windows.md`
    /// § Shell Extension). `folder_rel` is the folder's folder-relative path
    /// (forward slashes, no leading slash; `""` = the sync-root folder, whose
    /// descendants are every row). Returns the `state` of each row strictly
    /// *beneath* the folder — the prefix `"{folder_rel}/"` — never the folder's own
    /// row (folders have none) and never a sibling that merely shares the folder's
    /// name as a prefix (`sub2/x` is not under `sub`). `Deleted` rows are returned
    /// too; the caller's fold treats them as untracked.
    ///
    /// Scans only the subtree, not the table: `path` is the `sync_entries` PRIMARY
    /// KEY, so the half-open range `["{rel}/", "{rel}0")` (`'0'` is the byte after
    /// `'/'`) is served from the index — and it is wildcard-safe, unlike `LIKE` /
    /// `GLOB` over a path that may itself contain their metacharacters.
    pub fn descendant_states(&self, folder_rel: &str) -> Result<Vec<SyncState>> {
        // `state` **and** `size_bytes`: a 0-byte placeholder folds into the badge as `Synced`
        // ([`SyncState::effective_for_size`]), so the folder aggregate must see the *effective*
        // state — else a folder holding only present-and-empty files would read `CloudOnly`.
        let raw: Vec<(String, i64)> = if folder_rel.is_empty() {
            // The sync-root folder: every tracked file is a descendant.
            let mut stmt = self
                .conn
                .prepare("SELECT state, size_bytes FROM sync_entries")?;
            let mapped = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            // Half-open prefix range over the `path` PRIMARY KEY. `'0'` (0x30) is the
            // byte immediately after `'/'` (0x2F), so `["{base}/", "{base}0")` is
            // exactly the set of paths beginning with `"{base}/"` — the subtree,
            // never the folder's own row and never a `{base}…`-but-not-`{base}/…`
            // prefix sibling.
            let base = folder_rel.trim_end_matches('/');
            let lo = format!("{base}/");
            let hi = format!("{base}0");
            let mut stmt = self.conn.prepare(
                "SELECT state, size_bytes FROM sync_entries WHERE path >= ?1 AND path < ?2",
            )?;
            let mapped = stmt.query_map(params![lo, hi], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(raw
            .into_iter()
            .map(|(s, size)| {
                SyncState::from_str(&s)
                    .unwrap_or(SyncState::Synced)
                    .effective_for_size(size)
            })
            .collect())
    }

    /// Hard-remove a sync entry by path — no tombstone, the row is gone.
    ///
    /// Distinct from [`Self::delete_entry`], which tombstones (state →
    /// `Deleted`, kept for conflict detection against later local changes). The
    /// on-demand placeholder fold has no local state to reconcile against, so a
    /// tombstone would serve no purpose and would only block a later re-create
    /// (via `record_placeholders_from_changes`'s already-tracked guard); a hard
    /// remove keeps the `Placeholder` set a faithful mirror of the live remote
    /// folder.
    pub fn remove_entry(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sync_entries WHERE path = ?1", params![path])
            .context("remove sync entry")?;
        Ok(())
    }

    /// Mark a sync entry as deleted (tombstone).
    pub fn delete_entry(&self, path: &str) -> Result<()> {
        let now = now_epoch_secs();
        self.conn
            .execute(
                "UPDATE sync_entries SET state = ?1, deleted_at = ?2 WHERE path = ?3",
                params![SyncState::Deleted.as_str(), now, path],
            )
            .context("mark sync entry deleted")?;
        Ok(())
    }

    /// Purge tombstones older than `max_age_secs` seconds.
    pub fn purge_tombstones(&self, max_age_secs: i64) -> Result<usize> {
        let cutoff = now_epoch_secs() - max_age_secs;
        let count = self
            .conn
            .execute(
                "DELETE FROM sync_entries WHERE state = ?1 AND deleted_at < ?2",
                params![SyncState::Deleted.as_str(), cutoff],
            )
            .context("purge tombstones")?;
        Ok(count)
    }

    // ---- transfer_queue ----

    /// Enqueue a transfer (upload or download). Returns the row id.
    pub fn enqueue_transfer(
        &self,
        path: &str,
        direction: &str,
        chunk_hash: ContentHash,
        priority: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO transfer_queue (path, direction, chunk_hash, priority, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                path,
                direction,
                hash_to_blob(&chunk_hash),
                priority,
                now_epoch_secs(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List pending transfers for a given direction.
    pub fn pending_transfers(&self, direction: &str) -> Result<Vec<TransferEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, direction, chunk_hash, priority, status, retry_count, created_at, last_attempt_at
             FROM transfer_queue
             WHERE status = 'pending' AND direction = ?1
             ORDER BY priority DESC, id ASC",
        )?;
        let rows = stmt.query_map(params![direction], |row| {
            let hash_blob: Vec<u8> = row.get(3)?;
            Ok(TransferEntry {
                id: row.get(0)?,
                path: row.get(1)?,
                direction: row.get(2)?,
                chunk_hash: blob_to_hash(hash_blob)
                    .unwrap_or(ContentHash::from_digest_raw([0u8; 32])),
                priority: row.get(4)?,
                status: row.get(5)?,
                retry_count: row.get(6)?,
                created_at: row.get(7)?,
                last_attempt_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Remove a completed transfer from the queue.
    pub fn complete_transfer(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM transfer_queue WHERE id = ?1", params![id])
            .context("complete transfer")?;
        Ok(())
    }

    /// Increment retry count and record the attempt time.
    pub fn increment_retry(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE transfer_queue SET retry_count = retry_count + 1, last_attempt_at = ?1 WHERE id = ?2",
            params![now_epoch_secs(), id],
        ).context("increment retry")?;
        Ok(())
    }

    /// List pending transfers eligible for retry (backoff elapsed since last attempt).
    pub fn eligible_transfers(&self, direction: &str) -> Result<Vec<TransferEntry>> {
        let now = now_epoch_secs();
        let all = self.pending_transfers(direction)?;
        Ok(all
            .into_iter()
            .filter(|t| {
                let backoff = retry_backoff_secs(t.retry_count);
                now - t.last_attempt_at >= backoff
            })
            .collect())
    }

    // ---- backlog projection ----

    /// Aggregate this engine's transfer backlog + freshness stamps — see
    /// [`TransferBacklog`] for the exact semantics of every field. Read-only and
    /// cheap (covering index `idx_sync_entries_state`), designed for a second
    /// read connection polling while the engine holds the write side (WAL + the
    /// 5s busy_timeout `SyncDb::open` sets for exactly this).
    pub fn transfer_backlog(&self) -> Result<TransferBacklog> {
        let (files, bytes) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM sync_entries
             WHERE state IN (?1, ?2, ?3, ?4)",
            params![
                SyncState::LocallyModified.as_str(),
                SyncState::Uploading.as_str(),
                SyncState::RemotelyModified.as_str(),
                SyncState::Downloading.as_str(),
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(TransferBacklog {
            files_pending: files.max(0) as u64,
            bytes_pending: bytes.max(0) as u64,
            last_transfer_at: self.get_anchor_value("last_transfer_at")?,
            last_clean_pass_at: self.get_anchor_value("last_clean_pass_at")?,
        })
    }

    /// What this engine's folder holds: (file count, total bytes) over every
    /// tracked row regardless of transfer state — the
    /// `LocationInfo.file_count`/`total_bytes` projection. Neither tombstone state
    /// counts: `Deleted` is gone everywhere, `LocallyDeleted` is gone here with the
    /// record merely still owed.
    pub fn tracked_totals(&self) -> Result<(u64, u64)> {
        let (files, bytes) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM sync_entries
             WHERE state NOT IN (?1, ?2)",
            params![
                SyncState::Deleted.as_str(),
                SyncState::LocallyDeleted.as_str()
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok((files.max(0) as u64, bytes.max(0) as u64))
    }

    /// Stamp "a file-level content transfer just completed" (epoch secs, kept
    /// monotonic). Called at the same points that emit
    /// [`ProgressEvent::FileDone`](crate::progress::ProgressEvent) — never from
    /// entry-state writes, so the stamp can't lie the way
    /// `sync_entries.last_synced_at` would (it advances on the transition *into*
    /// `Uploading` too; see the `segment_backup_state` schema comment).
    pub fn mark_transfer_completed(&self) -> Result<()> {
        self.set_anchor_value_max("last_transfer_at", now_epoch_secs())
    }

    /// Stamp "a converge/pull pass just completed AND left nothing pending" —
    /// but only when the backlog is actually empty right now; a pass that leaves
    /// work behind proves nothing about consistency and stamps nothing. Call at
    /// the end of a clean converge (watch+upload side) or caught-up pull apply
    /// (download side).
    pub fn mark_clean_pass_if_drained(&self) -> Result<()> {
        let backlog = self.transfer_backlog()?;
        if backlog.files_pending == 0 {
            self.set_anchor_value_max("last_clean_pass_at", now_epoch_secs())?;
        }
        Ok(())
    }

    // ---- sync_anchor ----

    /// Read a named `sync_anchor` stamp (`None` when never set).
    fn get_anchor_value(&self, key: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_anchor WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, i64>(0))?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            _ => Ok(None),
        }
    }

    /// Upsert a named `sync_anchor` stamp, keeping it monotonic (`MAX` with the
    /// stored value guards against wall-clock steps going backwards).
    fn set_anchor_value_max(&self, key: &str, value: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO sync_anchor (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
                params![key, value],
            )
            .with_context(|| format!("set sync anchor '{key}'"))?;
        Ok(())
    }

    /// Get the last-seen sequence number (returns 0 if not set).
    pub fn get_anchor(&self) -> Result<i64> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_anchor WHERE key = 'seq'")?;
        let mut rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        match rows.next() {
            Some(Ok(v)) => Ok(v),
            _ => Ok(0),
        }
    }

    /// Set the last-seen sequence number.
    pub fn set_anchor(&self, seq: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO sync_anchor (key, value) VALUES ('seq', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![seq],
            )
            .context("set sync anchor")?;
        Ok(())
    }

    // ---- sync_conflicts ----

    /// Record a sync conflict for a file path. Returns the local row id.
    pub fn record_conflict(
        &self,
        path: &str,
        conflict_type: &str,
        details: Option<&str>,
    ) -> Result<i64> {
        let now = now_epoch_secs();
        self.conn.execute(
            "INSERT INTO sync_conflicts (path, conflict_type, details, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![path, conflict_type, details, now],
        ).context("record conflict")?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record a permanently un-appliable catch-up change ([`CATCHUP_FAILED`]),
    /// **once per path**: returns `None` when an unresolved skip row is already
    /// on record for `path` (a head re-judge folding the same change again),
    /// else the new row with its id.
    ///
    /// `path` is the plaintext relative path, or — `path_is_hash` — the change
    /// row's own wire `path_hash` when its sealed path never opened here, with
    /// `path_sealed` that row's label verbatim (`None` = a seal-less change).
    pub fn record_skipped_change(
        &self,
        path: &str,
        path_is_hash: bool,
        path_sealed: Option<&[u8]>,
        details: Option<&str>,
    ) -> Result<Option<SkippedChange>> {
        let on_record: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_conflicts
                 WHERE path = ?1 AND conflict_type = ?2 AND resolved_at IS NULL",
                params![path, CATCHUP_FAILED],
                |row| row.get(0),
            )
            .context("query skip row on record")?;
        if on_record > 0 {
            return Ok(None);
        }
        self.conn
            .execute(
                "INSERT INTO sync_conflicts
                     (path, conflict_type, details, created_at, path_is_hash, path_sealed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    path,
                    CATCHUP_FAILED,
                    details,
                    now_epoch_secs(),
                    path_is_hash,
                    path_sealed
                ],
            )
            .context("record skipped change")?;
        Ok(Some(SkippedChange {
            id: self.conn.last_insert_rowid(),
            path: path.to_string(),
            path_is_hash,
            path_sealed: path_sealed.map(<[u8]>::to_vec),
            details: details.map(str::to_string),
            nest_id: None,
        }))
    }

    /// Stamp the nest's conflict id on a local row once its report landed.
    pub fn set_conflict_nest_id(&self, id: i64, nest_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_conflicts SET nest_id = ?1 WHERE id = ?2",
                params![nest_id, id],
            )
            .context("set conflict nest_id")?;
        Ok(())
    }

    /// Unresolved skip rows whose report has not landed yet (no `nest_id`) and
    /// that can be filed at all — a seal-less `path_is_hash` row has neither a
    /// plaintext path nor a seal to report, and stays local.
    pub fn list_unreported_catchup_failures(&self) -> Result<Vec<SkippedChange>> {
        self.query_skips(
            "SELECT id, path, path_is_hash, path_sealed, details, nest_id FROM sync_conflicts
             WHERE conflict_type = ?1 AND resolved_at IS NULL AND nest_id IS NULL
               AND (path_is_hash = 0 OR path_sealed IS NOT NULL)
             ORDER BY id",
            params![CATCHUP_FAILED],
        )
    }

    /// The cure: a later change to this path landed, so every unresolved skip
    /// row filed under it — by plaintext `path`, or by `path_hash_hex` when its
    /// sealed path never opened — is resolved locally. Returns the cured rows;
    /// those carrying a `nest_id` owe the nest a candidate-free resolve.
    pub fn cure_catchup_failures_for_path(
        &self,
        path: &str,
        path_hash_hex: &str,
    ) -> Result<Vec<SkippedChange>> {
        let cured = self.query_skips(
            "SELECT id, path, path_is_hash, path_sealed, details, nest_id FROM sync_conflicts
             WHERE conflict_type = ?1 AND resolved_at IS NULL
               AND ((path_is_hash = 0 AND path = ?2) OR (path_is_hash = 1 AND path = ?3))
             ORDER BY id",
            params![CATCHUP_FAILED, path, path_hash_hex],
        )?;
        let now = now_epoch_secs();
        for row in &cured {
            self.conn
                .execute(
                    "UPDATE sync_conflicts SET resolved_at = ?1 WHERE id = ?2",
                    params![now, row.id],
                )
                .context("cure skip row")?;
        }
        Ok(cured)
    }

    /// Cured skip rows whose nest row is still open: the candidate-free
    /// `conflicts.resolve` for their `nest_id` has not landed yet.
    pub fn list_catchup_failures_owing_nest_resolve(&self) -> Result<Vec<SkippedChange>> {
        self.query_skips(
            "SELECT id, path, path_is_hash, path_sealed, details, nest_id FROM sync_conflicts
             WHERE conflict_type = ?1 AND resolved_at IS NOT NULL
               AND nest_id IS NOT NULL AND nest_resolved_at IS NULL
             ORDER BY id",
            params![CATCHUP_FAILED],
        )
    }

    /// Record that the nest row of a cured skip was resolved.
    pub fn set_conflict_nest_resolved(&self, id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_conflicts SET nest_resolved_at = ?1 WHERE id = ?2",
                params![now_epoch_secs(), id],
            )
            .context("set conflict nest_resolved_at")?;
        Ok(())
    }

    fn query_skips(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<SkippedChange>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(SkippedChange {
                id: row.get(0)?,
                path: row.get(1)?,
                path_is_hash: row.get(2)?,
                path_sealed: row.get(3)?,
                details: row.get(4)?,
                nest_id: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("list skip rows")
    }

    /// List all unresolved sync conflicts.
    #[allow(clippy::type_complexity)]
    pub fn list_unresolved_conflicts(
        &self,
    ) -> Result<Vec<(i64, String, String, Option<String>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, conflict_type, details, created_at FROM sync_conflicts WHERE resolved_at IS NULL ORDER BY created_at DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("list conflicts")
    }

    /// Whether `path` has an unresolved local conflict of any kind (read-only).
    pub fn has_unresolved_conflict_for_path(&self, path: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_conflicts WHERE path = ?1 AND resolved_at IS NULL",
                params![path],
                |row| row.get(0),
            )
            .context("query unresolved conflict for path")?;
        Ok(count > 0)
    }

    /// Whether `path` has an unresolved local conflict of a kind a propagated
    /// winner exists for ([`CONFLICT_KINDS_WITH_PROPAGATED_WINNER`]) —
    /// read-only.
    ///
    /// The catch-up apply path checks this *before* writing an incoming change:
    /// such a row means the incoming `modify` is the propagated winner, so the
    /// winner is taken verbatim (no 3-way merge) and the conflict is cleared
    /// only after the write lands. Kept separate from
    /// [`Self::resolve_winner_conflicts_for_path`] so a failed write doesn't
    /// leave the conflict cleared but the winner unapplied.
    ///
    /// ⚠ Never any kind: a [`CATCHUP_FAILED`] row has no winner, and arming the
    /// verbatim apply on one overwrote unpublished local edits with no conflict
    /// row and no retained loser (`conflicts.md` § Skipped catch-up changes
    /// reach the review list).
    pub fn has_unresolved_winner_conflict_for_path(&self, path: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM sync_conflicts
                     WHERE path = ?1 AND resolved_at IS NULL AND conflict_type IN ({})",
                    winner_kinds_sql()
                ),
                params![path],
                |row| row.get(0),
            )
            .context("query unresolved winner conflict for path")?;
        Ok(count > 0)
    }

    /// Clear the unresolved winner-bearing conflicts for `path` (apply-winner
    /// on catch-up; same kinds as
    /// [`Self::has_unresolved_winner_conflict_for_path`]).
    ///
    /// When the nest propagates a chosen winner, every device sees an ordinary
    /// `changes.list` `modify` for the path; the device applies the winning
    /// manifest and then calls this to drop its local conflict row. Unlike
    /// [`Self::resolve_conflict`] this keys on the path (the engine does not
    /// know the nest-side conflict id) and is a no-op — not an error — when no
    /// unresolved conflict exists, since most applied changes never conflicted.
    /// A [`CATCHUP_FAILED`] row is left alone: it has its own cure
    /// ([`Self::cure_catchup_failures_for_path`]), which owes the nest a
    /// resolve. Returns `true` iff a row was cleared.
    pub fn resolve_winner_conflicts_for_path(&self, path: &str) -> Result<bool> {
        let now = now_epoch_secs();
        let updated = self
            .conn
            .execute(
                &format!(
                    "UPDATE sync_conflicts SET resolved_at = ?1
                     WHERE path = ?2 AND resolved_at IS NULL AND conflict_type IN ({})",
                    winner_kinds_sql()
                ),
                params![now, path],
            )
            .context("resolve winner conflicts for path")?;
        Ok(updated > 0)
    }

    /// Mark a conflict as resolved by ID. Errors if already resolved or not found.
    pub fn resolve_conflict(&self, conflict_id: i64) -> Result<()> {
        let now = now_epoch_secs();
        let updated = self
            .conn
            .execute(
                "UPDATE sync_conflicts SET resolved_at = ?1 WHERE id = ?2 AND resolved_at IS NULL",
                params![now, conflict_id],
            )
            .context("resolve conflict")?;
        if updated == 0 {
            anyhow::bail!("conflict {conflict_id} not found or already resolved");
        }
        Ok(())
    }

    // ---- pause/resume ----

    /// Check if the daemon is paused.
    pub fn is_paused(&self) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_anchor WHERE key = 'paused'")?;
        let mut rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        match rows.next() {
            Some(Ok(1)) => Ok(true),
            _ => Ok(false),
        }
    }

    /// Set the daemon paused state.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        let val: i64 = if paused { 1 } else { 0 };
        self.conn
            .execute(
                "INSERT INTO sync_anchor (key, value) VALUES ('paused', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![val],
            )
            .context("set paused state")?;
        Ok(())
    }

    // ---- meta ----

    /// One `meta` row's value, or `None` when the key was never written.
    ///
    /// The typed accessors below (and
    /// [`crate::succession_progress`], which lives in its own module because
    /// its value is a serialized record rather than a scalar) all go through
    /// this pair, so the "absent key is not an error" reading is written once.
    pub(crate) fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        match stmt.query_row([key], |row| row.get::<_, String>(0)) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Write one `meta` row, replacing any existing value.
    pub(crate) fn meta_put(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete one `meta` row; deleting an absent key is a no-op, not an error
    /// (the same "absent key is not an error" reading [`Self::meta_get`] takes).
    pub(crate) fn meta_del(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// The audience shape this device last CONVERGED this folder's corpus to —
    /// `"sealed"` | `"plaintext"` — the steady-state gate of
    /// `SyncEngine::converge_corpus_to_audience` (folders re-model phase 4).
    /// `None` = never converged (a fresh db): read it as
    /// **`"sealed"`** (the default corpus shape), so no engine pays a full
    /// corpus walk on first open — only a folder whose
    /// projected audience actually flipped ever sees a mismatch.
    pub fn corpus_audience(&self) -> Result<Option<String>> {
        self.meta_get("corpus_audience")
    }

    /// Record the audience shape the corpus was just converged to (the write
    /// half of [`Self::corpus_audience`]). Written only AFTER the convergence
    /// pass returns cleanly, so a crash mid-pass re-drives it.
    pub fn set_corpus_audience(&self, shape: &str) -> Result<()> {
        self.meta_put("corpus_audience", shape)
    }

    /// What this seat remembers about its folder's audience attestations —
    /// the device-held replay floor `encryption-at-rest.md` § Readable classes
    /// → *The declassification is owner-ATTESTED* names (`AttestationMemory`,
    /// serialized by its own `to_meta` / `from_meta`; a row that does not parse
    /// fails closed there, not here). One row per `SyncDb`, because a `SyncDb`
    /// is one seat on one folder — the same scope `corpus_audience` has. `None`
    /// = a seat with no history.
    pub fn audience_attestation_memory(&self) -> Result<Option<String>> {
        self.meta_get("audience_attestation_memory")
    }

    /// Persist the memory the verifier handed back (the write half of
    /// [`Self::audience_attestation_memory`]). Written after **every** judged
    /// list read — armed or sealed — because the sealed verdict is the one
    /// that burns, and a burn that does not persist is a replay window.
    pub fn set_audience_attestation_memory(&self, row: &str) -> Result<()> {
        self.meta_put("audience_attestation_memory", row)
    }

    /// The website-serving shape this device last CONVERGED this folder's
    /// corpus to — `"served"` | `"off"` — the steady-state gate of
    /// `SyncEngine::converge_corpus_to_website` (`web-content-hosting.md`
    /// § Content model: a SEALED folder's back-catalogue reaches `web_files`
    /// only via client-driven re-records, because the nest holds no names for
    /// it (S9) and so cannot fold it into the enable-time backfill).
    /// `None` = never converged (a fresh db): read it as **`"off"`**, so no engine pays a
    /// corpus walk on first open — only a
    /// folder whose projected toggle actually reached the served shape ever
    /// sees a mismatch.
    pub fn corpus_website(&self) -> Result<Option<String>> {
        self.meta_get("corpus_website")
    }

    /// Record the website-serving shape the corpus was just converged to (the
    /// write half of [`Self::corpus_website`]). Written only AFTER the pass
    /// returns cleanly, so a crash mid-pass re-drives it.
    pub fn set_corpus_website(&self, shape: &str) -> Result<()> {
        self.meta_put("corpus_website", shape)
    }

    /// What this seat's reader of its set's change log last **admitted by** —
    /// the trigger state of the head re-judge (`mls-group-key-material.md`
    /// § M2 → *Writer-signed change records*, ruling (8)(f)). The engine holds
    /// its writer roster in memory only, so without this a restart would read
    /// every writer as newly gained. Opaque tokens, the engine's to mint and
    /// compare (one per identity a row may be signed as, one per predecessor
    /// root this host holds). `None` = never stored: the first run of a build
    /// that judges again, on a `SyncDb` that is one seat on one set.
    pub fn reader_admitted_by(&self) -> Result<Option<std::collections::BTreeSet<String>>> {
        Ok(self
            .meta_get("reader_admitted_by")?
            .map(|row| row.split_whitespace().map(str::to_string).collect()))
    }

    /// Store what the reader admits by now (the write half of
    /// [`Self::reader_admitted_by`]). The caller sets
    /// [`Self::set_head_rejudge_owed`] FIRST when the new set gained
    /// something, so a crash between the two re-detects the gain rather than
    /// forgetting it.
    pub fn set_reader_admitted_by(
        &self,
        tokens: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let row = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        self.meta_put("reader_admitted_by", &row.join(" "))
    }

    /// Whether a head re-judge pass is owed on this set — set when what the
    /// reader admits by gains something, cleared only when a pass completes,
    /// so one that fails part-way runs again (ruling (8)(f)).
    pub fn head_rejudge_owed(&self) -> Result<bool> {
        Ok(self.meta_get("head_rejudge_owed")?.is_some())
    }

    /// Set or clear the marker [`Self::head_rejudge_owed`] reads.
    pub fn set_head_rejudge_owed(&self, owed: bool) -> Result<()> {
        if owed {
            self.meta_put("head_rejudge_owed", "1")
        } else {
            self.meta_del("head_rejudge_owed")
        }
    }

    /// Clear one entry's `owner_sealed` marker — the declassify pass runs this
    /// for every path it re-records plaintext, because the marker means "this
    /// path's recorded manifest is known sealed" and that just stopped being
    /// true. A stale marker here is not cosmetic: the flip-back re-seal skips
    /// marked entries, so a marker that outlives the declassify would leave
    /// the path resting plaintext forever after the owner re-seals the folder.
    pub fn clear_owner_sealed(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sync_entries SET owner_sealed = 0 WHERE path = ?1",
                params![path],
            )
            .context("clear owner_sealed")?;
        Ok(())
    }

    /// Clear EVERY entry's `owner_sealed` marker — the flip-back convergence
    /// (`converge_corpus_to_audience`, target sealed) runs this before the
    /// re-seal pass, because a corpus that lived through a public window holds
    /// markers from before the declassify (its own pass clears the ones it
    /// touched, but a marker set on another device's schedule — or a pass
    /// interrupted midway — can survive). Folder-scoped by construction: one
    /// `SyncDb` is one folder. A cleared marker merely re-checks (one manifest
    /// fetch); a stale one strands a plaintext path.
    pub fn clear_all_owner_sealed(&self) -> Result<()> {
        self.conn
            .execute("UPDATE sync_entries SET owner_sealed = 0", [])
            .context("clear all owner_sealed")?;
        Ok(())
    }

    /// Get the last recorded binary version, if any.
    pub fn get_last_version(&self) -> Result<Option<String>> {
        self.meta_get("last_version")
    }

    /// Record the current binary version in the meta table.
    pub fn set_last_version(&self, version: &str) -> Result<()> {
        self.meta_put("last_version", version)
    }

    /// The last **authoritative** sync mode the nest answered for this set's
    /// seat (`config::resolve_device_mode`'s cache; `file-sync.md` § 4, ratified
    /// 2026-08-02). The engine's own two-value spelling (`"sync"` / `"backup"` —
    /// `SyncMode::as_cache_str`).
    /// `None` = the nest never answered for this seat, which is what makes a
    /// failed read resolve `Unresolved` (decline + hold) rather than guess.
    pub fn get_cached_sync_mode(&self) -> Result<Option<String>> {
        self.meta_get("last_authoritative_sync_mode")
    }

    /// Persist the fresh authoritative answer (see [`Self::get_cached_sync_mode`]).
    /// Written only when the nest actually answered — never from a cache
    /// read-back, never from a default.
    pub fn set_cached_sync_mode(&self, row_str: &str) -> Result<()> {
        self.meta_put("last_authoritative_sync_mode", row_str)
    }

    /// The folder's content residency as its engine last read it —
    /// `Some(true)` metadata-only, `Some(false)` full, `None` never read. The
    /// dehydration gate reads it when a body is freed (`file-sync.md` § Relay
    /// serving → *A holder keeps what it wrote*), so a caller holding only
    /// this DB answers the same gate as the engine; `None` keeps every
    /// own-record body.
    pub fn residency_reading(&self) -> Result<Option<bool>> {
        Ok(match self.meta_get("metadata_only_residency")?.as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        })
    }

    /// Persist the engine's residency reading (see [`Self::residency_reading`]).
    pub fn set_residency_reading(&self, metadata_only: bool) -> Result<()> {
        self.meta_put(
            "metadata_only_residency",
            if metadata_only { "1" } else { "0" },
        )
    }

    /// Forget the residency reading — the engine's reading is *unknown* (a
    /// cross-nest set no home nest has stamped), so [`Self::residency_reading`]
    /// answers `None` and the dehydration gate keeps every own-record body.
    pub fn clear_residency_reading(&self) -> Result<()> {
        self.meta_del("metadata_only_residency")
    }

    // ---- segment_backup_state / segment_backup_manifest_state (Plan 6 T5) ----

    /// Insert or update one (kind, scope_id, segment_id, member_actor_id, dest_id)
    /// row. Plan 6 T5 — called after each successful segment upload by
    /// a coordinator's pass. For the mail kind, pass
    /// `scope_id` for both `scope_id` and `member_actor_id`.
    ///
    /// `meta_size` is the plaintext size of the `.meta` sidecar pushed beside
    /// the `.dat`; `None` records a `.dat`-only push, which the shared diff
    /// then reads as "sidecar owed" — a caller passes `None` only to describe
    /// a corpus it knows is incomplete (a custodian view holding the `.dat`
    /// without its meta), never as a shortcut.
    #[allow(clippy::too_many_arguments)]
    pub fn put_segment_backup_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
        segment_id: u32,
        chunk_count: u64,
        byte_size: u64,
        meta_size: Option<u64>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO segment_backup_state
                    (kind, scope_id, segment_id, member_actor_id, dest_id,
                     last_synced_at, last_chunk_count, last_byte_size, last_meta_size)
                 VALUES (?1, ?2, ?3, ?2, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(kind, scope_id, segment_id, member_actor_id, dest_id)
                 DO UPDATE SET
                    last_synced_at   = excluded.last_synced_at,
                    last_chunk_count = excluded.last_chunk_count,
                    last_byte_size   = excluded.last_byte_size,
                    last_meta_size   = excluded.last_meta_size",
                params![
                    kind,
                    scope_id.as_slice(),
                    segment_id as i64,
                    destination_id,
                    now_epoch_secs(),
                    chunk_count as i64,
                    byte_size as i64,
                    meta_size.map(|m| m as i64),
                ],
            )
            .context("put segment_backup_state")?;
        Ok(())
    }

    /// Fetch one (kind='mail', scope_id, segment_id, member_actor_id=scope_id, dest_id) row.
    pub fn get_segment_backup_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
        segment_id: u32,
    ) -> Result<Option<MailBackupSegmentState>> {
        let mut stmt = self.conn.prepare(
            "SELECT last_chunk_count, last_byte_size, last_synced_at, last_meta_size
             FROM segment_backup_state
             WHERE kind            = ?1
               AND scope_id        = ?2
               AND segment_id      = ?3
               AND member_actor_id = ?2
               AND dest_id         = ?4",
        )?;
        let mut rows = stmt.query_map(
            params![kind, scope_id.as_slice(), segment_id as i64, destination_id,],
            |row| {
                let chunk_count: i64 = row.get(0)?;
                let byte_size: i64 = row.get(1)?;
                let synced_at: i64 = row.get(2)?;
                let meta_size: Option<i64> = row.get(3)?;
                Ok((chunk_count, byte_size, synced_at, meta_size))
            },
        )?;
        match rows.next() {
            Some(Ok((chunk_count, byte_size, synced_at, meta_size))) => {
                Ok(Some(MailBackupSegmentState {
                    last_chunk_count: chunk_count as u64,
                    last_byte_size: byte_size as u64,
                    last_synced_at: synced_at as u64,
                    last_meta_size: meta_size.map(|m| m as u64),
                }))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List every segment row for one (kind='mail', scope_id, member_actor_id=scope_id, dest_id)
    /// tuple, keyed by `segment_id`. Plan 6 T5: the coordinator diffs this
    /// against the source's `fauna.segments.list` reply.
    pub fn list_segment_backup_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
    ) -> Result<std::collections::HashMap<u32, MailBackupSegmentState>> {
        let mut stmt = self.conn.prepare(
            "SELECT segment_id, last_chunk_count, last_byte_size, last_synced_at, last_meta_size
             FROM segment_backup_state
             WHERE kind            = ?1
               AND scope_id        = ?2
               AND member_actor_id = ?2
               AND dest_id         = ?3",
        )?;
        let rows = stmt.query_map(params![kind, scope_id.as_slice(), destination_id,], |row| {
            let segment_id: i64 = row.get(0)?;
            let chunk_count: i64 = row.get(1)?;
            let byte_size: i64 = row.get(2)?;
            let synced_at: i64 = row.get(3)?;
            let meta_size: Option<i64> = row.get(4)?;
            Ok((segment_id, chunk_count, byte_size, synced_at, meta_size))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (segment_id, chunk_count, byte_size, synced_at, meta_size) = row?;
            out.insert(
                segment_id as u32,
                MailBackupSegmentState {
                    last_chunk_count: chunk_count as u64,
                    last_byte_size: byte_size as u64,
                    last_synced_at: synced_at as u64,
                    last_meta_size: meta_size.map(|m| m as u64),
                },
            );
        }
        Ok(out)
    }

    /// Delete one (kind='mail', scope_id, segment_id, member_actor_id=scope_id, dest_id) row.
    /// Plan 6 T5: the coordinator drops rows for compacted-out segments.
    pub fn delete_segment_backup_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
        segment_id: u32,
    ) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM segment_backup_state
                 WHERE kind            = ?1
                   AND scope_id        = ?2
                   AND segment_id      = ?3
                   AND member_actor_id = ?2
                   AND dest_id         = ?4",
                params![kind, scope_id.as_slice(), segment_id as i64, destination_id,],
            )
            .context("delete segment_backup_state")?;
        Ok(())
    }

    /// Insert or update the manifest-mirror state row for a
    /// (kind='mail', scope_id, member_actor_id=scope_id, dest_id) tuple.
    ///
    /// `content_moved` is whether THIS pass actually uploaded or dropped a
    /// segment (the caller's `any_uploaded || any_dropped`) — distinct from
    /// "the manifest mirror was rewritten", which also fires on pure
    /// bookkeeping (an owner's first pass with zero segments). Only a
    /// `content_moved` write advances `last_content_synced_at`; a
    /// bookkeeping-only write leaves it at its prior value (or NULL,
    /// unset). `last_synced_at` always advances — it is the manifest
    /// mirror's own timestamp, unconditionally correct as bookkeeping.
    pub fn put_segment_backup_manifest_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
        manifest_blake3: &[u8; 32],
        content_moved: bool,
    ) -> Result<()> {
        let now = now_epoch_secs();
        self.conn
            .execute(
                "INSERT INTO segment_backup_manifest_state
                    (kind, scope_id, member_actor_id, dest_id,
                     last_synced_at, manifest_blake3, last_content_synced_at)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(kind, scope_id, member_actor_id, dest_id)
                 DO UPDATE SET
                    last_synced_at  = excluded.last_synced_at,
                    manifest_blake3 = excluded.manifest_blake3,
                    last_content_synced_at = CASE WHEN ?7 THEN excluded.last_content_synced_at
                                                   ELSE last_content_synced_at END",
                params![
                    kind,
                    scope_id.as_slice(),
                    destination_id,
                    now,
                    manifest_blake3.as_slice(),
                    content_moved.then_some(now),
                    content_moved,
                ],
            )
            .context("put segment_backup_manifest_state")?;
        Ok(())
    }

    /// Fetch the last-uploaded manifest-mirror BLAKE3 for a
    /// (kind='mail', scope_id, member_actor_id=scope_id, dest_id) tuple, if any.
    pub fn get_segment_backup_manifest_state(
        &self,
        destination_id: &str,
        scope_id: &[u8; 32],
        kind: &str,
    ) -> Result<Option<[u8; 32]>> {
        let mut stmt = self.conn.prepare(
            "SELECT manifest_blake3
             FROM segment_backup_manifest_state
             WHERE kind            = ?1
               AND scope_id        = ?2
               AND member_actor_id = ?2
               AND dest_id         = ?3",
        )?;
        let mut rows = stmt
            .query_map(params![kind, scope_id.as_slice(), destination_id,], |row| {
                row.get::<_, Vec<u8>>(0)
            })?;
        match rows.next() {
            Some(Ok(blob)) => {
                let arr: [u8; 32] = blob
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("stored manifest blake3 is not 32 bytes"))?;
                Ok(Some(arr))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Most-recent `segment_backup_manifest_state.last_content_synced_at`
    /// across every `(kind, scope_id, member_actor_id)` for one `dest_id`.
    /// Feeds the status projection's `last_upload_time`
    /// (`docs/goal/behavior/backup-destinations.md` § Per-destination status read: "max
    /// (segment_backup_manifest_state.last_content_synced_at) across
    /// (kind,scope) for this dest_id"). Deliberately **not**
    /// `last_synced_at` — that column also advances on a pure
    /// manifest-bookkeeping write (an owner's first pass with zero
    /// segments), which would read as "just synced" for an owner nothing
    /// of whose content has ever moved. `None` when no row has a non-NULL
    /// `last_content_synced_at` yet (SQL `MAX` skips NULLs; over zero
    /// matching rows it is NULL either way).
    pub fn max_manifest_synced_at_for_dest(&self, destination_id: &str) -> Result<Option<u64>> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT MAX(last_content_synced_at) FROM segment_backup_manifest_state WHERE dest_id = ?1",
            params![destination_id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(v.map(|x| x as u64))
    }

    // ---- backup_destination_seen (Track B removal-reconcile) ----

    /// Remember a destination the coordinator has uploaded to, with the
    /// reconnect info (`dest_url` + reserved `folder`) its removal-reconcile
    /// needs once the destination leaves the `fauna.state.backup` destinations list and no
    /// live `DestinationBinding` carries it. Upserted each pass; idempotent.
    ///
    /// `nest_id` is the destination's 32-byte nest pubkey, which the nest
    /// arm's teardown pins its federation dial against.
    pub fn record_backup_destination_seen(
        &self,
        destination_id: &str,
        dest_url: &str,
        folder: &str,
        nest_id: &[u8],
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO backup_destination_seen (dest_id, dest_url, folder, nest_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(dest_id) DO UPDATE SET
                    dest_url   = excluded.dest_url,
                    folder   = excluded.folder,
                    nest_id    = excluded.nest_id,
                    updated_at = excluded.updated_at",
                params![destination_id, dest_url, folder, nest_id, now_epoch_secs()],
            )
            .context("record backup_destination_seen")?;
        Ok(())
    }

    /// Every destination the coordinator has uploaded to. Removal-reconcile
    /// diffs this against the configured destinations to find departed ones.
    pub fn list_backup_destinations_seen(&self) -> Result<Vec<SeenBackupDestination>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dest_id, dest_url, folder, nest_id FROM backup_destination_seen")?;
        let rows = stmt.query_map([], |row| {
            Ok(SeenBackupDestination {
                dest_id: row.get::<_, String>(0)?,
                dest_url: row.get::<_, String>(1)?,
                folder: row.get::<_, String>(2)?,
                nest_id: row.get::<_, Vec<u8>>(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every `(kind, scope_id, segment_id)` this coordinator has uploaded to
    /// `destination_id`, plus every `(kind, scope_id)` whose manifest mirror it
    /// has written — the removal teardown's work list.
    ///
    /// It is deliberately **this** state and not a destination-side enumeration:
    /// the departed destination's live-custody read is a USER-class kind only the
    /// owner's client can call, so the nest arm can only tear down what its own
    /// rows say it put there. That is exact for every path this nest uploaded,
    /// which since the slice-5 flip is every path in the set.
    pub fn list_backup_paths_for_destination(
        &self,
        destination_id: &str,
    ) -> Result<BackupPathsAtDestination> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, scope_id, segment_id, last_meta_size
             FROM segment_backup_state WHERE dest_id = ?1",
        )?;
        let rows = stmt.query_map(params![destination_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        let mut segments = Vec::new();
        for r in rows {
            let (kind, scope_id, segment_id, meta_size) = r?;
            segments.push(PlacedSegment {
                kind,
                scope_id,
                segment_id: segment_id as u32,
                meta_pushed: meta_size.is_some(),
            });
        }

        let mut stmt = self.conn.prepare(
            "SELECT kind, scope_id FROM segment_backup_manifest_state WHERE dest_id = ?1",
        )?;
        let rows = stmt.query_map(params![destination_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut manifests = Vec::new();
        for r in rows {
            manifests.push(r?);
        }

        Ok(BackupPathsAtDestination {
            segments,
            manifests,
        })
    }

    /// Forget a destination entirely: its `backup_destination_seen` row plus
    /// every `segment_backup_state` / `segment_backup_manifest_state` /
    /// `folder_backup_state` row keyed on it. Called by removal-reconcile once
    /// the destination-side custody is torn down, so a later pass neither
    /// re-records nor re-reconciles it.
    pub fn forget_backup_destination(&self, destination_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM segment_backup_state WHERE dest_id = ?1",
                params![destination_id],
            )
            .context("forget segment_backup_state")?;
        self.conn
            .execute(
                "DELETE FROM segment_backup_manifest_state WHERE dest_id = ?1",
                params![destination_id],
            )
            .context("forget segment_backup_manifest_state")?;
        self.conn
            .execute(
                "DELETE FROM folder_backup_state WHERE dest_id = ?1",
                params![destination_id],
            )
            .context("forget folder_backup_state")?;
        self.conn
            .execute(
                "DELETE FROM folder_withhold_checkpoint WHERE dest_id = ?1",
                params![destination_id],
            )
            .context("forget folder_withhold_checkpoint")?;
        self.conn
            .execute(
                "DELETE FROM backup_destination_seen WHERE dest_id = ?1",
                params![destination_id],
            )
            .context("forget backup_destination_seen")?;
        Ok(())
    }

    // ---- folder_backup_state (ordinary-folder destination coverage) ----

    /// Insert or update one mirrored path's cursor: the manifest last pushed to
    /// `destination_id` for `(folder_id, path_hash)`. Called after each
    /// successful path upload **and custody record** — the same
    /// record-before-state-advance ordering the segment pass carries.
    pub fn put_folder_backup_state(
        &self,
        destination_id: &str,
        folder_id: i64,
        path_hash: &[u8],
        manifest_hash: &[u8],
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO folder_backup_state
                    (dest_id, folder_id, path_hash, manifest_hash, last_synced_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(dest_id, folder_id, path_hash) DO UPDATE SET
                    manifest_hash  = excluded.manifest_hash,
                    last_synced_at = excluded.last_synced_at",
                params![
                    destination_id,
                    folder_id,
                    path_hash,
                    manifest_hash,
                    now_epoch_secs()
                ],
            )
            .context("put folder_backup_state")?;
        Ok(())
    }

    /// Every mirrored path for one `(destination, folder)`, keyed by the source
    /// `path_hash` — what the pass diffs the folder's live head against.
    pub fn list_folder_backup_state(
        &self,
        destination_id: &str,
        folder_id: i64,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path_hash, manifest_hash FROM folder_backup_state
             WHERE dest_id = ?1 AND folder_id = ?2",
        )?;
        let rows = stmt.query_map(params![destination_id, folder_id], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Drop one mirrored path's cursor (its destination custody was tombstoned).
    pub fn delete_folder_backup_state(
        &self,
        destination_id: &str,
        folder_id: i64,
        path_hash: &[u8],
    ) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM folder_backup_state
                 WHERE dest_id = ?1 AND folder_id = ?2 AND path_hash = ?3",
                params![destination_id, folder_id, path_hash],
            )
            .context("delete folder_backup_state")?;
        Ok(())
    }

    /// Every folder id this coordinator has mirrored to `destination_id` — the
    /// detach reconcile diffs this against the live coverage rows to find
    /// detached folders whose destination-side custody still needs tearing down.
    pub fn list_folder_backup_folder_ids(&self, destination_id: &str) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT folder_id FROM folder_backup_state WHERE dest_id = ?1")?;
        let rows = stmt.query_map(params![destination_id], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- folder_withhold_checkpoint (skip the manifest re-open
    // for an already-mirrored path while the withheld set is unchanged) ----

    /// The withheld-set digest this `(destination, folder)` pair last fully
    /// checked its already-mirrored paths against, or `None` before the first
    /// pass (or after its coverage was torn down and the memo dropped with
    /// it).
    pub fn get_folder_withhold_checkpoint(
        &self,
        destination_id: &str,
        folder_id: i64,
    ) -> Result<Option<Vec<u8>>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT withhold_digest FROM folder_withhold_checkpoint
                 WHERE dest_id = ?1 AND folder_id = ?2",
                params![destination_id, folder_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .context("get folder_withhold_checkpoint")
    }

    /// Record that every already-mirrored path in `(destination, folder)` has
    /// now been confirmed clean against `digest`. Called once a
    /// `run_folder_once` pass's per-path loop finishes without error, so a
    /// pass that aborts partway (the missing-manifest arm) never stamps a
    /// digest against paths it never actually re-checked this pass.
    pub fn put_folder_withhold_checkpoint(
        &self,
        destination_id: &str,
        folder_id: i64,
        digest: &[u8],
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO folder_withhold_checkpoint
                    (dest_id, folder_id, withhold_digest, checked_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(dest_id, folder_id) DO UPDATE SET
                    withhold_digest = excluded.withhold_digest,
                    checked_at      = excluded.checked_at",
                params![destination_id, folder_id, digest, now_epoch_secs()],
            )
            .context("put folder_withhold_checkpoint")?;
        Ok(())
    }

    /// Drop one `(destination, folder)`'s checkpoint -- called everywhere its
    /// `folder_backup_state` rows are torn down (detach, destination removal),
    /// so a later re-attach under the same ids starts from a clean memo rather
    /// than one describing paths that no longer exist.
    pub fn delete_folder_withhold_checkpoint(
        &self,
        destination_id: &str,
        folder_id: i64,
    ) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM folder_withhold_checkpoint
                 WHERE dest_id = ?1 AND folder_id = ?2",
                params![destination_id, folder_id],
            )
            .context("delete folder_withhold_checkpoint")?;
        Ok(())
    }

    // ---- own_change_log (share-leg row retention, B2) ----

    /// Retain one of this replica's own just-recorded change rows — the
    /// write-through the engine performs at its recording funnel's `Ok(seq)`.
    ///
    /// Idempotent on `seq`: the nest assigns each seq exactly once and a row
    /// is immutable once sequenced, so a replayed write (a retried caller) is
    /// a no-op rather than an error or a second copy.
    ///
    /// **This is also the own-pending leg's in-place upgrade** (B2.5 —
    /// `p2p-shared-set-build.md` § *Build design — the row half*): the same write retires the
    /// path's pending (`seq IS NULL`) row, whatever its manifest — a record
    /// this replica just landed for the path IS its newest local truth, so
    /// any pending row for it (this upload's own mint, or a stale one an
    /// abandoned offline edit left behind) is obsolete the moment the ack
    /// names a seq. One transaction: the store never observes a moment with
    /// both the pending and the sequenced form of one change.
    pub fn retain_own_change(&self, row: &OwnChangeRow) -> Result<()> {
        if row.seq.is_none() {
            anyhow::bail!(
                "retain_own_change requires a nest-assigned seq — a pending row is minted \
                 via mint_pending_own_change, never retained"
            );
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin own-change retention")?;
        tx.execute(
            "DELETE FROM own_change_log WHERE path = ?1 AND seq IS NULL",
            params![row.path],
        )
        .context("retire pending own-change row on upgrade")?;
        tx.execute(
            "INSERT INTO own_change_log
                (seq, path, path_hash, path_sealed, manifest_hash, size_bytes,
                 change_type, created_at, content_key_version, thumbnail_hash,
                 derived_through, is_resolution, author_actor_id, device_id,
                 signature, signer_key, signer_cert)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17)
             ON CONFLICT(seq) DO NOTHING",
            params![
                row.seq,
                row.path,
                row.path_hash,
                row.path_sealed,
                row.manifest_hash,
                row.size_bytes,
                row.change_type,
                row.created_at,
                row.content_key_version.map(|v| v as i64),
                row.thumbnail_hash,
                row.derived_through,
                row.is_resolution,
                row.author_actor_id,
                row.device_id,
                row.signature,
                row.signer_key,
                row.signer_cert,
            ],
        )
        .context("retain own change row")?;
        tx.commit().context("commit own-change retention")
    }

    /// Mint (or refresh) the path's own-PENDING retention row — the
    /// offline-authored leg's reserved `seq IS NULL` shape (B2.5). Minted only
    /// at the engine's own local-change detection, with the manifest computed
    /// from local plaintext at mint time (`p2p-shared-set-build.md` § *Build design — the row
    /// half*, the own-pending bullet); `retain_own_change` upgrades it in
    /// place at the funnel's `Ok(seq)`.
    ///
    /// At most ONE pending row per path: a re-mint (the reconnect re-drive
    /// re-sealing the same bytes, or a further offline edit producing a new
    /// manifest) replaces the path's previous pending row — the newest local
    /// state is the only one worth serving. One transaction, so a serve read
    /// never sees zero-or-two pending rows for the path mid-replace.
    pub fn mint_pending_own_change(&self, row: &OwnChangeRow) -> Result<()> {
        if row.seq.is_some() {
            anyhow::bail!(
                "mint_pending_own_change is the seq-less mint — a sequenced row goes through \
                 retain_own_change"
            );
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin pending own-change mint")?;
        tx.execute(
            "DELETE FROM own_change_log WHERE path = ?1 AND seq IS NULL",
            params![row.path],
        )
        .context("replace pending own-change row")?;
        tx.execute(
            "INSERT INTO own_change_log
                (seq, path, path_hash, path_sealed, manifest_hash, size_bytes,
                 change_type, created_at, content_key_version, thumbnail_hash,
                 derived_through, is_resolution, author_actor_id, device_id,
                 signature, signer_key, signer_cert)
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16)",
            params![
                row.path,
                row.path_hash,
                row.path_sealed,
                row.manifest_hash,
                row.size_bytes,
                row.change_type,
                row.created_at,
                row.content_key_version.map(|v| v as i64),
                row.thumbnail_hash,
                row.derived_through,
                row.is_resolution,
                row.author_actor_id,
                row.device_id,
                row.signature,
                row.signer_key,
                row.signer_cert,
            ],
        )
        .context("mint pending own-change row")?;
        tx.commit().context("commit pending own-change mint")
    }

    /// This replica's own-PENDING rows (`seq IS NULL`), oldest mint first, at
    /// most `max_rows`. Pending rows have no cursor position in the nest's
    /// sequence, so the store serves them only on the TAIL page (sequenced
    /// results < max_rows) — this read is that tail's source.
    pub fn own_pending_changes(&self, max_rows: u32) -> Result<Vec<OwnChangeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, path, path_hash, path_sealed, manifest_hash, size_bytes,
                    change_type, created_at, content_key_version, thumbnail_hash,
                    derived_through, is_resolution, author_actor_id, device_id,
                    signature, signer_key, signer_cert
             FROM own_change_log
             WHERE seq IS NULL
             ORDER BY id
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![max_rows], map_own_change_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Withdraw every own-PENDING row sealed under a content-key generation
    /// below `floor` — a floor the host has just read says those seals are
    /// behind, so the peer door stops serving them (`on-demand-files.md`
    /// § Shared sets on a capability host, decision 2′ (d)). Sequenced rows and
    /// unstamped (owner-only) rows are untouched. Returns how many were
    /// withdrawn; the path itself stays pending and re-mints at its re-seal.
    pub fn retire_own_pending_below(&self, floor: u64) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM own_change_log
                 WHERE seq IS NULL AND content_key_version IS NOT NULL
                   AND content_key_version < ?1",
                params![floor as i64],
            )
            .context("withdraw own-pending rows below the content-key floor")
    }

    /// This replica's retained own-authored rows with `seq` strictly greater
    /// than `since`, in seq order, at most `max_rows` — the
    /// `ShareStore::changes_since` read shape. Sequenced rows only (a NULL
    /// `seq` is the own-pending leg's reserved shape and never serves from
    /// this query).
    pub fn own_changes_since(&self, since: i64, max_rows: u32) -> Result<Vec<OwnChangeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, path, path_hash, path_sealed, manifest_hash, size_bytes,
                    change_type, created_at, content_key_version, thumbnail_hash,
                    derived_through, is_resolution, author_actor_id, device_id,
                    signature, signer_key, signer_cert
             FROM own_change_log
             WHERE seq IS NOT NULL AND seq > ?1
             ORDER BY seq
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, max_rows], map_own_change_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- relayed_change_log (share-leg relay retention, the relayed-row lift) ----

    /// Retain another writer's VERIFIED row for relay, byte-exact (schema
    /// comment). The caller has judged it through the shared reader; this
    /// store judges nothing.
    ///
    /// A sequenced row (`seq` = the nest's) is idempotent on `seq` and retires
    /// that writer's pending row for the path in the same transaction — the
    /// nest's word supersedes the offline one. A pending row (`seq = None`)
    /// replaces that writer's previous pending row for the path: the newest is
    /// the only one worth relaying.
    pub fn retain_relayed_change(&self, row: &RelayedChangeRow) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin relayed-change retention")?;
        tx.execute(
            "DELETE FROM relayed_change_log
             WHERE author_actor_id = ?1 AND path_hash = ?2 AND seq IS NULL",
            params![row.author_actor_id, row.path_hash],
        )
        .context("retire the writer's pending relayed row for the path")?;
        tx.execute(
            "INSERT INTO relayed_change_log
                (seq, author_actor_id, path_hash, row, signer_cert)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(seq) DO NOTHING",
            params![
                row.seq,
                row.author_actor_id,
                row.path_hash,
                row.row,
                row.signer_cert
            ],
        )
        .context("retain relayed change row")?;
        tx.commit().context("commit relayed-change retention")
    }

    /// Relayed SEQUENCED rows with `seq` strictly greater than `since`, in seq
    /// order, at most `max_rows` — merged by the share store with
    /// [`Self::own_changes_since`] into one seq-ordered page.
    pub fn relayed_changes_since(
        &self,
        since: i64,
        max_rows: u32,
    ) -> Result<Vec<RelayedChangeRow>> {
        self.relayed_rows(
            "SELECT seq, author_actor_id, path_hash, row, signer_cert
             FROM relayed_change_log
             WHERE seq IS NOT NULL AND seq > ?1
             ORDER BY seq
             LIMIT ?2",
            params![since, max_rows],
        )
    }

    /// Relayed PENDING rows (`seq IS NULL`), oldest first, at most `max_rows` —
    /// served on the tail page beside [`Self::own_pending_changes`].
    pub fn relayed_pending_changes(&self, max_rows: u32) -> Result<Vec<RelayedChangeRow>> {
        self.relayed_rows(
            "SELECT seq, author_actor_id, path_hash, row, signer_cert
             FROM relayed_change_log
             WHERE seq IS NULL
             ORDER BY id
             LIMIT ?1",
            params![max_rows],
        )
    }

    fn relayed_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<RelayedChangeRow>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(RelayedChangeRow {
                seq: row.get(0)?,
                author_actor_id: row.get(1)?,
                path_hash: row.get(2)?,
                row: row.get(3)?,
                signer_cert: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- own_manifests (share-leg serve-side byte half, slice E) ----

    /// Retain one sealed manifest's canonical bytes, keyed by its hash (hex).
    /// Last-writer-wins on the key; callers treat failure as best-effort
    /// (schema comment: derived-recoverable).
    pub fn retain_manifest(
        &self,
        manifest_hash_hex: &str,
        bytes: &[u8],
        path: &str,
        content_key_version: Option<u64>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO own_manifests
                    (manifest_hash, bytes, path, content_key_version, retained_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest_hash_hex,
                    bytes,
                    path,
                    content_key_version,
                    now_epoch_secs()
                ],
            )
            .context("retain manifest bytes")?;
        Ok(())
    }

    /// One retained manifest by hash (hex), or `None` — the serve-side byte
    /// half's manifest read.
    pub fn retained_manifest(&self, manifest_hash_hex: &str) -> Result<Option<RetainedManifest>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT bytes, path, content_key_version FROM own_manifests
                 WHERE manifest_hash = ?1",
                params![manifest_hash_hex],
                |row| {
                    Ok(RetainedManifest {
                        bytes: row.get(0)?,
                        path: row.get(1)?,
                        content_key_version: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                },
            )
            .optional()
            .context("read retained manifest")
    }

    // ---- held_chunks (the serve core's store-key index) ----

    /// Index the body now at `path`: replace every row for the path with one
    /// per chunk, in one transaction — so a re-sealed or re-applied path never
    /// keeps a previous body's keys alongside its own.
    pub fn index_held_body(
        &self,
        path: &str,
        content_key_version: Option<u64>,
        chunks: &[HeldChunk],
    ) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin held-body index")?;
        tx.execute("DELETE FROM held_chunks WHERE path = ?1", params![path])
            .context("retire the path's previous held chunks")?;
        {
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO held_chunks
                    (store_key, path, offset, len, plain_hash, content_key_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for c in chunks {
                insert
                    .execute(params![
                        c.store_key.as_slice(),
                        path,
                        c.offset as i64,
                        c.len as i64,
                        c.plain_hash.digest().as_slice(),
                        content_key_version.map(|v| v as i64),
                    ])
                    .context("index held chunk")?;
            }
        }
        tx.commit().context("commit held-body index")
    }

    /// Every held body that names `store_key` — usually zero or one; identical
    /// content at several paths yields one row each.
    pub fn held_chunks(&self, store_key: &[u8; 32]) -> Result<Vec<HeldChunkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, offset, len, plain_hash, content_key_version FROM held_chunks
             WHERE store_key = ?1",
        )?;
        let rows = stmt.query_map(params![store_key.as_slice()], |row| {
            let plain: Vec<u8> = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                plain,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (path, offset, len, plain, ckv) = r?;
            // A malformed row is skipped, never served: the serve checks the
            // range against this hash, so a row without one is unusable.
            let Ok(plain) = <[u8; 32]>::try_from(plain.as_slice()) else {
                continue;
            };
            out.push(HeldChunkRow {
                path,
                chunk: HeldChunk {
                    store_key: *store_key,
                    offset: offset as u64,
                    len: len as u64,
                    plain_hash: ContentHash::from_digest_raw(plain),
                },
                content_key_version: ckv.map(|v| v as u64),
            });
        }
        Ok(out)
    }

    /// The store keys the index names for the body at `path`, in file order —
    /// what a witness reads to name a file's chunks on the nest's store.
    pub fn held_store_keys(&self, path: &str) -> Result<Vec<[u8; 32]>> {
        let mut stmt = self
            .conn
            .prepare("SELECT store_key FROM held_chunks WHERE path = ?1 ORDER BY offset")?;
        let rows = stmt.query_map(params![path], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(key) = <[u8; 32]>::try_from(r?.as_slice()) {
                out.push(key);
            }
        }
        Ok(out)
    }

    /// Every entry whose recorded head names `manifest`, for the serve-side
    /// re-derivation of a manifest whose best-effort retention write failed.
    /// Usually zero or one row; identical content at two paths can yield more.
    pub fn entries_by_manifest(&self, manifest: &ContentHash) -> Result<Vec<SyncEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SYNC_ENTRY_SELECT} WHERE manifest_hash = ?1"))?;
        let rows = stmt.query_map(
            params![opt_hash_to_blob(&Some(*manifest))],
            map_sync_entry_row,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

impl SyncDb {
    // ---- share_writer_roster (share-leg cached writer roster, B2) ----

    /// Replace the cached writer roster with `members` — `(actor_hex,
    /// is_writer)` pairs from ONE successful, complete roster read.
    /// Atomic replace-all: the read is authoritative for the whole set, so
    /// departed members drop out by absence. Actor ids are normalized to
    /// lowercase on write (the wire's spelling) so the consult never misses
    /// on case. Callers must NOT call this on a failed read — keeping the
    /// stale roster is the ruling's staleness posture (schema comment).
    ///
    /// `chains` is the same read's proven predecessor chains — `(writer_hex,
    /// predecessors nearest first)` — stored beside the writer each belongs
    /// to and replaced with the members (`share_writer_predecessors`). A
    /// predecessor gets no member row from its chain: it answers
    /// [`Self::cached_share_writer`] only when `members` lists it itself.
    ///
    /// `owner` is the same read's one owner row, if it named one
    /// (`WriterRoster::owner`): the member row of that actor is marked, and a
    /// reader whose binding names no owner judges by it
    /// ([`Self::cached_share_roster`]).
    pub fn cache_share_writer_roster(
        &self,
        members: &[(String, bool)],
        chains: &[(String, Vec<String>)],
        owner: Option<&str>,
    ) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("begin roster replace")?;
        tx.execute("DELETE FROM share_writer_roster", [])
            .context("clear share_writer_roster")?;
        tx.execute("DELETE FROM share_writer_predecessors", [])
            .context("clear share_writer_predecessors")?;
        for (writer_hex, predecessors) in chains {
            for (position, predecessor_hex) in predecessors.iter().enumerate() {
                tx.execute(
                    "INSERT OR REPLACE INTO share_writer_predecessors
                        (writer_id, position, predecessor_id) VALUES (?1, ?2, ?3)",
                    params![
                        writer_hex.to_lowercase(),
                        position as i64,
                        predecessor_hex.to_lowercase()
                    ],
                )
                .context("insert share_writer_predecessors row")?;
            }
        }
        let now = now_epoch_secs();
        for (actor_hex, is_writer) in members {
            tx.execute(
                "INSERT OR REPLACE INTO share_writer_roster
                    (actor_id, is_writer, refreshed_at, is_owner) VALUES (?1, ?2, ?3, ?4)",
                params![
                    actor_hex.to_lowercase(),
                    is_writer,
                    now,
                    owner.is_some_and(|o| o.eq_ignore_ascii_case(actor_hex))
                ],
            )
            .context("insert share_writer_roster row")?;
        }
        tx.commit().context("commit roster replace")
    }

    /// The pull cursor for `peer_hex` — the pump's next `since` against that
    /// peer (`share_pull_cursors`). `0` when no page was ever ingested.
    pub fn share_pull_cursor(&self, peer_hex: &str) -> Result<i64> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT cursor FROM share_pull_cursors WHERE peer_actor = ?1",
                params![peer_hex.to_lowercase()],
                |row| row.get(0),
            )
            .optional()
            .context("read share pull cursor")?
            .unwrap_or(0))
    }

    /// Advance `peer_hex`'s pull cursor to `cursor` — forward-only (MAX
    /// semantics), so a re-ingested stale page can never rewind the pump.
    pub fn advance_share_pull_cursor(&self, peer_hex: &str, cursor: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO share_pull_cursors (peer_actor, cursor, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(peer_actor) DO UPDATE SET
                    cursor = MAX(cursor, excluded.cursor),
                    updated_at = excluded.updated_at",
                params![peer_hex.to_lowercase(), cursor, now_epoch_secs()],
            )
            .context("advance share pull cursor")?;
        Ok(())
    }

    /// The ingest consult: is `actor_hex` a cached WRITER for this set?
    /// Fail-closed — an absent row (unknown actor, or no roster ever cached)
    /// answers `false`, exactly the `peer_is_cached_writer` bool the share
    /// leg's `screen_peer_row` takes and the per-actor answer its
    /// `judge_peer_row` consults (there is deliberately no third answer).
    pub fn cached_share_writer(&self, actor_hex: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let got: Option<bool> = self
            .conn
            .query_row(
                "SELECT is_writer FROM share_writer_roster WHERE actor_id = ?1",
                params![actor_hex.to_lowercase()],
                |row| row.get(0),
            )
            .optional()
            .context("read share_writer_roster")?;
        Ok(got.unwrap_or(false))
    }

    /// The whole cached roster as the peer door's judge reads it: the cached
    /// WRITERS (`actor_hex`), and each writer's proven predecessors nearest
    /// first — `(writer_hex, predecessors)`, a writer with none absent — and
    /// the read's owner, when exactly one cached row is marked. All empty when
    /// no roster was ever cached (or the set is unshared).
    pub fn cached_share_roster(&self) -> Result<CachedShareRoster> {
        let mut owners = self
            .conn
            .prepare("SELECT actor_id FROM share_writer_roster WHERE is_owner != 0")
            .context("prepare share_writer_roster owner read")?;
        let mut owners = owners
            .query_map([], |row| row.get::<_, String>(0))
            .context("read share_writer_roster owner")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read share_writer_roster owner row")?;
        let owner = if owners.len() == 1 {
            owners.pop()
        } else {
            None
        };
        let mut writers = self
            .conn
            .prepare("SELECT actor_id FROM share_writer_roster WHERE is_writer != 0")
            .context("prepare share_writer_roster read")?;
        let writers = writers
            .query_map([], |row| row.get::<_, String>(0))
            .context("read share_writer_roster")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read share_writer_roster row")?;
        let mut rows = self
            .conn
            .prepare(
                "SELECT writer_id, predecessor_id FROM share_writer_predecessors
                 ORDER BY writer_id, position",
            )
            .context("prepare share_writer_predecessors read")?;
        let rows = rows
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("read share_writer_predecessors")?;
        let mut chains: Vec<(String, Vec<String>)> = Vec::new();
        for row in rows {
            let (writer, predecessor) = row.context("read share_writer_predecessors row")?;
            match chains.last_mut() {
                Some((last, chain)) if *last == writer => chain.push(predecessor),
                _ => chains.push((writer, vec![predecessor])),
            }
        }
        Ok(CachedShareRoster {
            writers,
            chains,
            owner,
        })
    }
}

/// The share leg's cached roster, whole ([`SyncDb::cached_share_roster`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedShareRoster {
    /// The cached writers, lowercase hex.
    pub writers: Vec<String>,
    /// `(writer, its proven predecessors nearest first)`, lowercase hex.
    pub chains: Vec<(String, Vec<String>)>,
    /// The cached read's owner (its one `role == "owner"` row), lowercase hex.
    pub owner: Option<String>,
}

impl SyncDb {
    // ---- share_overlay (share-leg provisional overlay, B2) ----

    /// Land (or refresh) a path's provisional overlay row — latest-per-path.
    ///
    /// Returns `false` (no write) when the incoming row is provably staler
    /// than what the overlay already holds: both nest-sequenced and the
    /// incoming seq lower. Everything else replaces — within one serve
    /// session rows arrive in seq order, and an un-sequenced row has no
    /// cross-replica seq to compare.
    pub fn upsert_share_overlay(&self, row: &ShareOverlayRow) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let existing: Option<(bool, i64)> = self
            .conn
            .query_row(
                "SELECT sequenced, seq FROM share_overlay WHERE path = ?1",
                params![row.path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read share_overlay for staleness check")?;
        if let Some((existing_sequenced, existing_seq)) = existing
            && existing_sequenced
            && row.sequenced
            && row.seq < existing_seq
        {
            return Ok(false);
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO share_overlay
                    (path, seq, sequenced, change_type, manifest_hash, size_bytes,
                     content_key_version, proven_author, materialized, content_hash,
                     ingested_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    row.path,
                    row.seq,
                    row.sequenced,
                    row.change_type,
                    row.manifest_hash,
                    row.size_bytes,
                    row.content_key_version.map(|v| v as i64),
                    row.proven_author.to_lowercase(),
                    row.materialized,
                    row.content_hash,
                    now_epoch_secs(),
                ],
            )
            .context("upsert share_overlay row")?;
        Ok(true)
    }

    /// The path's provisional overlay row, if any.
    pub fn get_share_overlay(&self, path: &str) -> Result<Option<ShareOverlayRow>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT path, seq, sequenced, change_type, manifest_hash, size_bytes,
                        content_key_version, proven_author, materialized, content_hash
                 FROM share_overlay WHERE path = ?1",
                params![path],
                overlay_row_from_sql,
            )
            .optional()
            .context("read share_overlay row")
    }

    /// Every provisional overlay row — the read-surface annotation source
    /// (provisional badge; a provisional `delete` hides its path).
    pub fn list_share_overlay(&self) -> Result<Vec<ShareOverlayRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, seq, sequenced, change_type, manifest_hash, size_bytes,
                    content_key_version, proven_author, materialized, content_hash
             FROM share_overlay ORDER BY path",
        )?;
        let rows = stmt.query_map([], overlay_row_from_sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Stamp a path's overlay row materialized, with the written content's
    /// hash (hex) when bytes were written — what a later confirm uses to
    /// stamp the withheld dehydration proof without re-reading disk. `None`
    /// for the already-current arms that fetched nothing.
    pub fn mark_share_overlay_materialized(
        &self,
        path: &str,
        content_hash: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE share_overlay SET materialized = 1, content_hash = ?2
                 WHERE path = ?1",
                params![path, content_hash],
            )
            .context("mark share_overlay materialized")?;
        Ok(())
    }

    /// Retire a path's provisional annotation — the reconcile's only verb
    /// (confirm and supersede both end here; reconcile never edits a row).
    pub fn remove_share_overlay(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM share_overlay WHERE path = ?1", params![path])
            .context("remove share_overlay row")?;
        Ok(())
    }
}

fn overlay_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareOverlayRow> {
    Ok(ShareOverlayRow {
        path: row.get(0)?,
        seq: row.get(1)?,
        sequenced: row.get(2)?,
        change_type: row.get(3)?,
        manifest_hash: row.get(4)?,
        size_bytes: row.get(5)?,
        content_key_version: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        proven_author: row.get(7)?,
        materialized: row.get(8)?,
        content_hash: row.get(9)?,
    })
}

/// One provisional peer-ingested row (`share_overlay` — the share leg's
/// read-side overlay, B2). The path's latest provisional state as one peer
/// served it; retired wholesale by the reconcile when the nest's sequenced
/// row for the path arrives.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareOverlayRow {
    pub path: String,
    /// The serving peer's row seq (nest seq when `sequenced`; the peer's own
    /// numbering otherwise).
    pub seq: i64,
    pub sequenced: bool,
    /// `"create"` | `"modify"` | `"delete"`.
    pub change_type: String,
    /// Hex; `None` for a provisional delete.
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    pub content_key_version: Option<u64>,
    /// The channel-proven serving actor, lowercase hex.
    pub proven_author: String,
    /// Whether this row's bytes were materialized to disk.
    pub materialized: bool,
    /// Hex content hash of the materialized bytes (set with `materialized`).
    pub content_hash: Option<String>,
}

/// One retained own-authored change row (`own_change_log` — the share leg's
/// ROW-half retention, B2). Field spellings mirror the `SyncChange` wire shape
/// (hex hashes, lowercase change types) so the serve-side conversion is a
/// field map; this crate deliberately does not depend on `fauna-protocol`, so
/// the wire type itself never appears here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwnChangeRow {
    /// The nest-assigned sequence number this row landed under — `None` for
    /// an own-PENDING row (offline-authored, minted before any nest sequenced
    /// it; B2.5). A pending row's wire form spells this `seq: 0` +
    /// `sequenced: false`; the store, not this struct, owns that translation.
    pub seq: Option<i64>,
    /// Plaintext relative path.
    pub path: String,
    /// Hex BLAKE3 of the path (the wire's `path_hash` spelling).
    pub path_hash: String,
    /// The sealed path sibling recorded alongside, verbatim.
    pub path_sealed: Option<Vec<u8>>,
    /// Hex manifest hash; `None` for deletes.
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    /// `"create"` | `"modify"` | `"delete"`.
    pub change_type: String,
    /// Local epoch-millis stamp at retention. Informational — the nest's own
    /// `created_at` is the authoritative one; receivers order by `seq`.
    pub created_at: i64,
    pub content_key_version: Option<u64>,
    pub thumbnail_hash: Option<String>,
    pub derived_through: Option<i64>,
    pub is_resolution: Option<bool>,
    /// This replica's own actor id, lowercase hex — the author by
    /// construction (only own records reach the retention funnel).
    pub author_actor_id: String,
    /// This replica's device id, hex.
    pub device_id: String,
    /// The writer signature over the row as served — `None` when its host
    /// held no signer (served unsigned, pre-flip).
    pub signature: Option<Vec<u8>>,
    /// The key [`Self::signature`] verifies under.
    pub signer_key: Option<Vec<u8>>,
    /// The delegated signer's cert, canonical embed-as-bytes (`None` for a
    /// direct signer).
    pub signer_cert: Option<Vec<u8>>,
}

/// One retained relayed row (`relayed_change_log` — the share leg's relay
/// retention). Opaque here: `row` is the served `SyncChange`'s canonical
/// dag-cbor, byte-exact so its writer signature still verifies wherever it is
/// served next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RelayedChangeRow {
    /// The nest's seq for a sequenced row; `None` for a writer's pending row.
    pub seq: Option<i64>,
    /// The row's signed writer, lowercase hex.
    pub author_actor_id: String,
    /// The row's `path_hash`, hex — with the author, a pending row's key.
    pub path_hash: String,
    /// Canonical dag-cbor of the row as served.
    pub row: Vec<u8>,
    /// The delegated signer's cert, canonical embed-as-bytes.
    pub signer_cert: Option<Vec<u8>>,
}

/// Row mapper for the `own_change_log` SELECTs (`own_changes_since` /
/// `own_pending_changes` — both select the same 17 columns in this order).
fn map_own_change_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OwnChangeRow> {
    Ok(OwnChangeRow {
        seq: row.get(0)?,
        path: row.get(1)?,
        path_hash: row.get(2)?,
        path_sealed: row.get(3)?,
        manifest_hash: row.get(4)?,
        size_bytes: row.get(5)?,
        change_type: row.get(6)?,
        created_at: row.get(7)?,
        content_key_version: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        thumbnail_hash: row.get(9)?,
        derived_through: row.get(10)?,
        is_resolution: row.get(11)?,
        author_actor_id: row.get(12)?,
        device_id: row.get(13)?,
        signature: row.get(14)?,
        signer_key: row.get(15)?,
        signer_cert: row.get(16)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(byte: u8) -> ContentHash {
        ContentHash::from_digest_raw([byte; 32])
    }

    const ACTOR_A: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const ACTOR_B: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    fn held(key: u8, offset: u64) -> HeldChunk {
        HeldChunk {
            store_key: [key; 32],
            offset,
            len: 10,
            plain_hash: test_hash(key ^ 0xFF),
        }
    }

    /// Re-indexing a path replaces its rows whole — the previous body's keys
    /// stop resolving there — while another path holding the same key keeps
    /// its own row.
    #[test]
    fn a_reindexed_path_drops_its_previous_keys_and_shared_keys_stay_per_path() {
        let db = SyncDb::open_in_memory().unwrap();
        db.index_held_body("a", None, &[held(1, 0), held(2, 10)])
            .unwrap();
        db.index_held_body("b", Some(3), &[held(2, 0)]).unwrap();

        let shared = db.held_chunks(&[2; 32]).unwrap();
        assert_eq!(shared.len(), 2, "both paths hold key 2");

        db.index_held_body("a", None, &[held(4, 0)]).unwrap();
        assert!(db.held_chunks(&[1; 32]).unwrap().is_empty());
        let rows = db.held_chunks(&[2; 32]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "b");
        assert_eq!(rows[0].chunk, held(2, 0));
        assert_eq!(rows[0].content_key_version, Some(3));
        assert_eq!(db.held_chunks(&[4; 32]).unwrap()[0].chunk, held(4, 0));
    }

    #[test]
    fn actor_state_dir_is_the_lowercased_hex_subdir_and_refuses_junk() {
        let base = Path::new("/base");
        // The one derivation all three processes share: `<base>/<hex>`, case-normalized.
        assert_eq!(
            actor_state_dir(base, &ACTOR_A.to_ascii_uppercase()).unwrap(),
            base.join(ACTOR_A)
        );
        // Anything that isn't a 32-byte hex id is refused, never a stray dir.
        assert!(actor_state_dir(base, "abc").is_err());
        assert!(actor_state_dir(base, &"zz".repeat(32)).is_err());
        assert!(actor_state_dir(base, "../escape").is_err());
    }

    /// *Erasure follows scope* (`account-scoping.md`): the all-accounts erase
    /// drops every actor's scoped stores, and install-scoped state (logs,
    /// host-keyed TOFU pins) survives. A store sitting flat at the base is no
    /// account's — the pre-scoping layout it would once have been is refused,
    /// not adopted — so the sweep leaves it exactly like any other install file.
    #[test]
    fn erase_all_account_scopes_drops_every_scope_but_not_install_state() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("files");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(base.join("logs")).unwrap();
        std::fs::write(base.join("logs/app.log"), b"x").unwrap();
        std::fs::write(base.join("nest-pins.json"), b"{}").unwrap();
        std::fs::write(base.join("fauna.db"), b"flat").unwrap();

        let a_dir = actor_state_dir(&base, ACTOR_A).unwrap();
        let b_dir = actor_state_dir(&base, ACTOR_B).unwrap();
        for dir in [&a_dir, &b_dir] {
            std::fs::create_dir_all(dir.join("segment-backup")).unwrap();
            SyncDb::open(dir.join("fauna.db")).unwrap();
        }

        let sweep = erase_all_account_scopes(&base);
        assert_eq!(sweep.erased, 2, "both actor scopes");
        assert!(
            sweep.is_clean(),
            "nothing should have survived a clean sweep: {:?}",
            sweep.survivors
        );
        assert!(!a_dir.exists());
        assert!(!b_dir.exists());

        // Install-scoped state is untouched — it describes the device, not a person.
        assert!(base.join("logs/app.log").exists());
        assert!(base.join("nest-pins.json").exists());
        assert!(
            base.join("fauna.db").exists(),
            "a flat file is no account's scope"
        );
    }

    /// A malformed actor id lands under the `-unresolved-` component, never the
    /// shared base, and that component is not an account scope any sweep or
    /// reach query names.
    #[test]
    fn a_malformed_actor_id_resolves_under_the_unresolved_component() {
        let base = Path::new("/base");
        assert_eq!(
            actor_state_dir_or_unresolved(base, ACTOR_A),
            base.join(ACTOR_A)
        );
        assert_eq!(
            actor_state_dir_or_unresolved(base, "not-hex"),
            base.join(UNRESOLVED_ACTOR_COMPONENT)
        );
        assert!(normalize_actor_hex(UNRESOLVED_ACTOR_COMPONENT).is_err());
    }

    /// The erase's reach, read before the erase: exactly the scopes
    /// [`erase_all_account_scopes`] would remove — every well-formed actor dir,
    /// and nothing that merely looks close. An erasing gesture's refusal
    /// probes this list (`account-scoping.md` § Concurrent instances), so a
    /// name the sweep erases and this omits is an account erased under a live
    /// sibling with no question asked.
    #[test]
    fn account_scopes_under_names_exactly_what_the_sweep_would_erase() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("files");
        for dir in [ACTOR_A, ACTOR_B] {
            std::fs::create_dir_all(actor_state_dir(&base, dir).unwrap()).unwrap();
        }
        // Near misses the sweep leaves alone: upper-case hex, short hex, a file
        // with an actor-shaped name, and ordinary install-scoped dirs.
        std::fs::create_dir_all(base.join(ACTOR_A.to_ascii_uppercase())).unwrap();
        std::fs::create_dir_all(base.join(&ACTOR_B[..62])).unwrap();
        std::fs::write(base.join("c".repeat(64)), b"not a dir").unwrap();
        std::fs::create_dir_all(base.join("logs")).unwrap();

        let mut listed = account_scopes_under(&base);
        listed.sort();
        let mut expected = vec![ACTOR_A.to_string(), ACTOR_B.to_string()];
        expected.sort();
        assert_eq!(listed, expected);

        assert_eq!(
            erase_all_account_scopes(&base).erased,
            listed.len(),
            "the list and the sweep must agree about what an actor scope is"
        );
        assert!(
            account_scopes_under(&tmp.path().join("absent")).is_empty(),
            "a base that does not exist holds no account"
        );
    }

    /// One undeletable directory is ONE item on the user's line, however many
    /// sweeps walked over it — a caller folding overlapping sweeps told the
    /// user *"2 item(s)"* about a single surviving scope until `absorb` deduped.
    ///
    /// `erased` is deliberately NOT deduped in the same breath — it counts
    /// removals, not places.
    #[test]
    fn absorbing_an_overlapping_sweep_counts_a_survivor_once() {
        let a = std::path::PathBuf::from("/nowhere/aa11");
        let b = std::path::PathBuf::from("/nowhere/bb22");

        let mut sweep = EraseSweep {
            erased: 1,
            survivors: vec![a.clone()],
        };
        sweep.absorb(EraseSweep {
            erased: 1,
            survivors: vec![a.clone(), b.clone()],
        });

        assert_eq!(
            sweep.survivors,
            vec![a, b],
            "the same location seen twice is one fact about the device, and \
             survivors.len() is the number the user is shown"
        );
        assert_eq!(
            sweep.erased, 2,
            "removals still add up — they are not places"
        );
        assert!(!sweep.is_clean());
    }

    /// **A failure on one actor scope must not abandon the others, and the
    /// survivor must be named** (`account-scoping.md` § Erasure follows scope,
    /// the ⚠ *the erase must SAY what it did* corollary).
    ///
    /// Until 2026-09-09 the actor-scope loop `?`-returned on its first failing
    /// `remove_dir_all`, so a single undeletable file left every *later* scope
    /// under that base untouched — and the caller got one opaque `io::Error`
    /// naming neither how much had been done nor what was left. That is the
    /// production shape on Windows, where an open file cannot be deleted and a
    /// scanner or indexer holding a handle is nobody's defect to close.
    ///
    /// Windows is where it bites and POSIX is where this test can run, so the
    /// fault injection differs by necessity: `unlink` on POSIX happily removes
    /// an open file, so a held handle proves nothing here. A **read-only
    /// parent** is the portable equivalent — `remove_dir_all` cannot unlink a
    /// child out of a directory it may not write — and it exercises the exact
    /// same arm.
    #[test]
    #[cfg(unix)]
    fn a_scope_that_will_not_go_is_reported_and_does_not_abandon_the_others() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("files");
        std::fs::create_dir_all(&base).unwrap();

        // A is undeletable: its own directory is read-only, so the file inside
        // it cannot be unlinked. B is ordinary.
        let a = actor_state_dir(&base, ACTOR_A).unwrap();
        let b = actor_state_dir(&base, ACTOR_B).unwrap();
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("mls.db"), b"ratchets").unwrap();
        std::fs::write(b.join("mls.db"), b"ratchets").unwrap();
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Running as root defeats the injection entirely (root ignores the write
        // bit), and a test that silently proves nothing is worse than no test.
        if std::fs::remove_file(a.join("mls.db")).is_ok() {
            std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: this process can write through a read-only dir (root?)");
            return;
        }

        let sweep = erase_all_account_scopes(&base);

        // The whole point: B went even though A came first in nobody-knows-what
        // readdir order, and A is NAMED rather than merely counted as missing.
        assert!(!b.exists(), "the other actor's scope must still be erased");
        assert!(a.exists(), "the read-only scope is expected to survive");
        assert_eq!(
            sweep.survivors,
            vec![a.clone()],
            "the survivor must be named, so a seat can tell the user WHAT is \
             still on their device"
        );
        assert_eq!(sweep.erased, 1, "one scope did go");
        assert!(!sweep.is_clean(), "a sweep with survivors is not clean");

        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// **A held file must cost the erase only itself, and must be NAMED.**
    /// `std::fs::remove_dir_all` returns on its first failure, so one file a
    /// live handle still holds (SQLite opens without `FILE_SHARE_DELETE`, so
    /// the delete is `os error 32`) left every sibling file in that scope on
    /// disk too — and the log named only the scope dir, never the holder, so
    /// a windows sign-out that left the user's data behind could not say which
    /// store was still open (measured 2026-09-29: eight survivors per windows
    /// e2e run, holder unknown).
    #[test]
    #[cfg(windows)]
    fn a_held_file_survives_alone_and_is_named() {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately without
        // FILE_SHARE_DELETE — the share mode SQLite opens its files with.
        const SHARE_READ_WRITE: u32 = 0x1 | 0x2;

        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("files");
        let a = actor_state_dir(&base, ACTOR_A).unwrap();
        std::fs::create_dir_all(a.join("nested")).unwrap();
        std::fs::write(a.join("free.txt"), b"x").unwrap();
        std::fs::write(a.join("nested").join("also-free.txt"), b"x").unwrap();
        std::fs::create_dir_all(a.join("zz-held")).unwrap();
        let held = a.join("zz-held").join("store.db");
        std::fs::write(&held, b"x").unwrap();
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .open(&held)
            .unwrap();

        assert_eq!(
            remove_tree_naming_held(&a),
            Err(vec![held.clone()]),
            "the one held file, and only it, must be named"
        );
        assert!(held.exists());
        assert!(
            !a.join("free.txt").exists(),
            "an unheld sibling must still go"
        );
        assert!(
            !a.join("nested").exists(),
            "an unheld subtree must still go"
        );

        // The survivor is still the scope, so the user's count is unchanged.
        let sweep = erase_all_account_scopes(&base);
        assert_eq!(sweep.survivors, vec![a.clone()]);

        drop(handle);
        assert_eq!(remove_tree_naming_held(&a), Ok(true));
        assert!(!a.exists());
    }

    /// The fallback walk is not a second, looser erase: an absent tree is
    /// still `Ok`, and a clean tree goes whole.
    #[test]
    fn remove_tree_naming_held_removes_a_clean_tree_and_tolerates_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("scope");
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("f"), b"x").unwrap();
        assert_eq!(remove_tree_naming_held(&dir), Ok(true));
        assert!(!dir.exists());
        assert_eq!(remove_tree_naming_held(&dir), Ok(false));
    }

    /// Removing ONE account erases that actor's stores and nothing else — the
    /// per-account remove affordance, not sign-out.
    #[test]
    fn erase_account_scope_removes_only_that_actor() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("files");
        let a = actor_state_dir(&base, ACTOR_A).unwrap();
        let b = actor_state_dir(&base, ACTOR_B).unwrap();
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("fauna.db"), b"a").unwrap();
        std::fs::write(b.join("fauna.db"), b"b").unwrap();

        erase_account_scope(&base, ACTOR_A).unwrap();
        assert!(!a.exists());
        assert!(b.join("fauna.db").exists());

        // Erasing an account that never had scoped state is a no-op, not an error.
        erase_account_scope(&base, ACTOR_A).unwrap();
    }

    /// The iOS domain-removal gate's engine-seam pin (`file-sync.md`
    /// § Multi-account × File Provider, consequence 2): exactly the rows whose
    /// local bytes lack a proven record report as un-recorded — mid-flight
    /// states and the record-failed `Synced` shape — while placeholder /
    /// remote-only rows never block a removal. A set with no DB has nothing to
    /// lose.
    #[test]
    fn set_unrecorded_rels_reports_unproven_local_content_only() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();

        // No DB yet: nothing un-recorded.
        let docs = FolderRef::Local(7);
        assert!(set_unrecorded_rels(state_dir, docs).unwrap().is_empty());

        let db = SyncDb::open(docs.state_db_path(state_dir)).unwrap();
        let up = |path: &str, state: SyncState, local: Option<ContentHash>| {
            db.upsert_entry(
                path,
                local,
                None,
                Some(test_hash(9)),
                state,
                0,
                0,
                1,
                1,
                None,
            )
            .unwrap();
        };

        // Un-recorded: an OS edit mid-upload, a plain dirty row, a conflict,
        // and the record-failed shape (`Synced` flipped pre-record, no proof).
        up("dirty.txt", SyncState::LocallyModified, Some(test_hash(1)));
        up("inflight.txt", SyncState::Uploading, Some(test_hash(2)));
        up("clash.txt", SyncState::Conflicted, Some(test_hash(3)));
        up("record-failed.txt", SyncState::Synced, Some(test_hash(4)));

        // Recorded / no local bytes: proven-head Synced, an un-hydrated
        // placeholder, a remote-moved row, a mid-download row, a tombstone.
        up("proven.txt", SyncState::Synced, Some(test_hash(5)));
        db.stamp_recorded_content_from_local("proven.txt", ProofOrigin::OwnRecord)
            .unwrap();
        up("placeholder.txt", SyncState::Placeholder, None);
        up(
            "remote-moved.txt",
            SyncState::RemotelyModified,
            Some(test_hash(6)),
        );
        up("downloading.txt", SyncState::Downloading, None);
        up("gone.txt", SyncState::Deleted, None);

        let mut rels = set_unrecorded_rels(state_dir, docs).unwrap();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "clash.txt",
                "dirty.txt",
                "inflight.txt",
                "record-failed.txt"
            ]
        );
    }

    /// The engine→display map (8→6), `file-sync.md` § Per-file sync-status
    /// display. In-flight states report the *direction* of travel, so the badge
    /// a user sees while an edit is being pushed is `Uploading`, not a distinct
    /// "modified" state; `Deleted` has no row to render at all.
    #[test]
    fn to_display_collapses_the_engine_states_onto_six() {
        use fauna_core::format::SyncDisplayState as D;

        assert_eq!(SyncState::Synced.to_display(), Some(D::Synced));
        assert_eq!(SyncState::Uploading.to_display(), Some(D::Uploading));
        assert_eq!(SyncState::Downloading.to_display(), Some(D::Downloading));
        assert_eq!(SyncState::Conflicted.to_display(), Some(D::Conflict));
        assert_eq!(SyncState::Placeholder.to_display(), Some(D::RemoteOnly));
        // The two "diverged, not yet moved" states collapse onto the direction
        // the engine is about to move them.
        assert_eq!(SyncState::LocallyModified.to_display(), Some(D::Uploading));
        assert_eq!(
            SyncState::RemotelyModified.to_display(),
            Some(D::Downloading)
        );
        // Hidden: the file is gone, so the client renders no row.
        assert_eq!(SyncState::Deleted.to_display(), None);
        // Hidden too: the user deleted it here and the nest is still owed the
        // record (`delete-propagation.md` § *An offline placeholder delete
        // propagates*, decision (d)).
        assert_eq!(SyncState::LocallyDeleted.to_display(), None);
    }

    /// `LocallyDeleted` persists under its own name, carries no un-recorded local
    /// bytes, and is not a tracked file — the mirror of `LocallyModified` for the
    /// delete direction (decision (d)).
    #[test]
    fn a_locally_deleted_row_round_trips_and_is_not_tracked() {
        let db = SyncDb::open_in_memory().unwrap();
        assert_eq!(
            SyncState::from_str(SyncState::LocallyDeleted.as_str()),
            Some(SyncState::LocallyDeleted)
        );
        db.upsert_entry(
            "kept.txt",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            5,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "gone.txt",
            None,
            None,
            None,
            SyncState::Placeholder,
            0,
            0,
            7,
            1,
            None,
        )
        .unwrap();
        db.update_state("gone.txt", SyncState::LocallyDeleted)
            .unwrap();
        let row = db.get_entry("gone.txt").unwrap().unwrap();
        assert_eq!(row.state, SyncState::LocallyDeleted);
        assert!(!entry_is_unrecorded(&row), "no local bytes to lose");
        assert_eq!(
            db.tracked_totals().unwrap(),
            (1, 5),
            "a file the user deleted here is not a tracked file"
        );
    }

    /// The seen mark (decision (a)): off by default on every row, set per path,
    /// cleared wholesale — and invisible to every other column's writer, so an
    /// `upsert_entry` re-point keeps it.
    #[test]
    fn the_seen_mark_defaults_off_marks_per_path_and_clears_wholesale() {
        let db = SyncDb::open_in_memory().unwrap();
        for rel in ["a.txt", "b.txt", "c.txt"] {
            db.upsert_entry(
                rel,
                None,
                None,
                None,
                SyncState::Placeholder,
                0,
                0,
                1,
                1,
                None,
            )
            .unwrap();
        }
        let seen = |p: &str| db.get_entry(p).unwrap().unwrap().seen_on_disk;
        assert!(!seen("a.txt"), "a folded row is never-seen");

        db.mark_seen(&["a.txt", "b.txt", "not-a-row.txt"]).unwrap();
        assert!(seen("a.txt") && seen("b.txt") && !seen("c.txt"));
        assert!(
            db.list_by_state(SyncState::Placeholder)
                .unwrap()
                .iter()
                .any(|e| e.path == "a.txt" && e.seen_on_disk),
            "every full-row reader carries the mark"
        );

        // A re-point (the fold's moved head) writes every other column, never the mark.
        db.upsert_entry(
            "a.txt",
            None,
            None,
            None,
            SyncState::Placeholder,
            0,
            9,
            2,
            1,
            None,
        )
        .unwrap();
        assert!(seen("a.txt"), "upsert keeps the mark");

        db.clear_seen("b.txt").unwrap();
        assert!(!seen("b.txt") && seen("a.txt"));

        db.clear_seen_all().unwrap();
        assert!(!seen("a.txt") && !seen("b.txt") && !seen("c.txt"));
    }

    /// The aggregate backlog is the fold of the per-file display states: a file
    /// counts as pending exactly when its badge would read `Uploading` or
    /// `Downloading` (`to_display`, the one owner of that map) — an aggregate
    /// that disagreed with the badges a user sees per file would be incoherent.
    /// The expectation is *derived from* `to_display` in the loop, so this test
    /// fails if either side of the coupling drifts.
    #[test]
    fn transfer_backlog_is_the_display_transfer_fold() {
        use fauna_core::format::SyncDisplayState as D;
        let tmp = tempfile::tempdir().unwrap();
        let db = SyncDb::open(tmp.path().join("state.db")).unwrap();

        const ALL: [SyncState; 9] = [
            SyncState::Synced,
            SyncState::LocallyModified,
            SyncState::RemotelyModified,
            SyncState::Conflicted,
            SyncState::Uploading,
            SyncState::Downloading,
            SyncState::Placeholder,
            SyncState::Deleted,
            SyncState::LocallyDeleted,
        ];
        let mut want_files = 0u64;
        let mut want_bytes = 0u64;
        for (i, st) in ALL.iter().enumerate() {
            let size = 1i64 << i; // distinguishable per-state sizes
            db.upsert_entry(&format!("f{i}"), None, None, None, *st, 0, 0, size, 1, None)
                .unwrap();
            if matches!(st.to_display(), Some(D::Uploading | D::Downloading)) {
                want_files += 1;
                want_bytes += size as u64;
            }
        }
        // Not vacuous: the four transfer states are LocallyModified, Uploading,
        // RemotelyModified, Downloading.
        assert_eq!(want_files, 4);

        let b = db.transfer_backlog().unwrap();
        assert_eq!(b.files_pending, want_files);
        assert_eq!(b.bytes_pending, want_bytes);
        assert_eq!(b.last_transfer_at, None, "no transfer ever completed");
    }

    /// `last_transfer_at` advances only on an explicit completion mark — never as
    /// a side effect of entry writes. `sync_entries.last_synced_at` is stamped on
    /// *every* upsert (including the transition *into* `Uploading`), which is
    /// exactly the dishonesty the segment-backup `last_content_synced_at` column
    /// exists to avoid (see the schema comment there); this pins the same rule
    /// for the file-transfer projection.
    #[test]
    fn last_transfer_at_stamps_only_on_explicit_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let db = SyncDb::open(tmp.path().join("state.db")).unwrap();

        // Entry writes stamp last_synced_at, but must not invent a transfer.
        db.upsert_entry(
            "a.txt",
            None,
            None,
            None,
            SyncState::Uploading,
            0,
            0,
            7,
            1,
            None,
        )
        .unwrap();
        assert_eq!(db.transfer_backlog().unwrap().last_transfer_at, None);

        let before = now_epoch_secs();
        db.mark_transfer_completed().unwrap();
        let first = db
            .transfer_backlog()
            .unwrap()
            .last_transfer_at
            .expect("stamped");
        assert!(first >= before && first <= now_epoch_secs() + 1);

        // A later mark never moves the stamp backwards.
        db.mark_transfer_completed().unwrap();
        let second = db.transfer_backlog().unwrap().last_transfer_at.unwrap();
        assert!(second >= first);
    }

    /// The wire's `last_sync` answers "when was this device last known consistent
    /// with the nest", so it is the freshest of TWO facts: the last completed
    /// content transfer, and the last converge/pull pass that ended with an empty
    /// backlog. An idle-but-checking device stays fresh (clean passes advance it);
    /// a device with a stuck backlog goes honestly stale (a pass that leaves work
    /// behind stamps nothing).
    #[test]
    fn last_sync_is_the_freshest_of_transfer_and_clean_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let db = SyncDb::open(tmp.path().join("state.db")).unwrap();

        assert_eq!(db.transfer_backlog().unwrap().last_sync_at(), None);

        // A pass over a non-empty backlog is NOT consistency — no stamp.
        db.upsert_entry(
            "a.txt",
            None,
            None,
            None,
            SyncState::LocallyModified,
            0,
            0,
            7,
            1,
            None,
        )
        .unwrap();
        db.mark_clean_pass_if_drained().unwrap();
        assert_eq!(db.transfer_backlog().unwrap().last_sync_at(), None);

        // The transfer completes; the transfer stamp carries last_sync.
        db.upsert_entry(
            "a.txt",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            7,
            1,
            None,
        )
        .unwrap();
        db.mark_transfer_completed().unwrap();
        let after_transfer = db.transfer_backlog().unwrap();
        assert_eq!(
            after_transfer.last_sync_at(),
            after_transfer.last_transfer_at
        );

        // A clean pass over the now-empty backlog stamps, and last_sync is the max
        // of the two facts.
        db.mark_clean_pass_if_drained().unwrap();
        let b = db.transfer_backlog().unwrap();
        let clean = b.last_clean_pass_at.expect("clean pass stamped");
        assert!(b.last_sync_at().unwrap() >= after_transfer.last_sync_at().unwrap());
        assert_eq!(
            b.last_sync_at().unwrap(),
            clean.max(b.last_transfer_at.unwrap())
        );
    }

    /// `tracked_totals` is the "what does this folder hold" projection for
    /// `LocationInfo.file_count`/`total_bytes`: every tracked row except
    /// `Deleted` (which has no file to count), regardless of transfer state.
    #[test]
    fn tracked_totals_count_all_but_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let db = SyncDb::open(tmp.path().join("state.db")).unwrap();

        db.upsert_entry(
            "s.txt",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            100,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "u.txt",
            None,
            None,
            None,
            SyncState::Uploading,
            0,
            0,
            10,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "p.txt",
            None,
            None,
            None,
            SyncState::Placeholder,
            0,
            0,
            1000,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "d.txt",
            None,
            None,
            None,
            SyncState::Deleted,
            0,
            0,
            9999,
            1,
            None,
        )
        .unwrap();

        let (files, bytes) = db.tracked_totals().unwrap();
        assert_eq!(files, 3);
        assert_eq!(bytes, 1110);
    }

    #[test]
    fn open_creates_missing_parent_directories() {
        // Regression: on a fresh machine the sync state dir
        // (e.g. %LOCALAPPDATA%\Fauna\sync\) does not exist yet, and SQLite's
        // Connection::open creates the *file* but not intermediate *directories*.
        // The doc comment promises "Open (or create)", and the
        // hydration/pipe-add paths rely on it, so opening into a not-yet-existing
        // parent must succeed (works-out-of-the-box invariant).
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("nested").join("sync").join("state.db");
        assert!(!db_path.parent().unwrap().exists());

        let db = SyncDb::open(&db_path).unwrap();

        // The DB is usable and the file landed where requested.
        let id = db.get_or_create_device_id().unwrap();
        assert_ne!(id, [0u8; 32]);
        assert!(db_path.exists());
    }

    /// The Windows folder-badge query (`apps/windows.md` § Shell Extension):
    /// a folder's status folds over its tracked descendants, so the DB must return
    /// exactly the rows strictly *beneath* a folder — never the folder's own row,
    /// never a prefix-sibling (`sub2/x` is not under `sub`), and the sync-root
    /// folder (`""`) aggregates everything.
    #[test]
    fn descendant_states_folds_only_the_subtree() {
        let db = SyncDb::open_in_memory().unwrap();
        // Non-zero size: a 0-byte placeholder folds to `Synced` via `effective_for_size`
        // (covered by `descendant_states_folds_a_zero_byte_placeholder_to_synced`); here every
        // row must keep its literal state so the subtree-scoping assertions stay meaningful.
        let seed = |path: &str, state: SyncState| {
            db.upsert_entry(path, None, None, None, state, 0, 0, 10, 1, None)
                .unwrap();
        };
        seed("a.txt", SyncState::Synced); // a top-level file
        seed("sub/b.txt", SyncState::Placeholder); // under "sub"
        seed("sub/c/d.txt", SyncState::Conflicted); // deeper under "sub"
        seed("sub2/e.txt", SyncState::Uploading); // prefix-SIBLING: NOT under "sub"

        let sorted = |mut v: Vec<SyncState>| {
            v.sort_by_key(|s| s.as_str());
            v
        };

        // Under "sub": the two nested rows only — excludes "sub2/e.txt" and "a.txt".
        assert_eq!(
            sorted(db.descendant_states("sub").unwrap()),
            sorted(vec![SyncState::Placeholder, SyncState::Conflicted])
        );
        // A trailing slash on the folder path is tolerated (same subtree).
        assert_eq!(
            sorted(db.descendant_states("sub/").unwrap()),
            sorted(vec![SyncState::Placeholder, SyncState::Conflicted])
        );
        // The sync-root folder ("") aggregates every tracked row.
        assert_eq!(
            sorted(db.descendant_states("").unwrap()),
            sorted(vec![
                SyncState::Synced,
                SyncState::Placeholder,
                SyncState::Conflicted,
                SyncState::Uploading,
            ])
        );
        // A folder with no descendants is empty (the caller renders NotTracked).
        assert!(db.descendant_states("nope").unwrap().is_empty());
        // The prefix-sibling resolves only its own subtree.
        assert_eq!(
            db.descendant_states("sub2").unwrap(),
            vec![SyncState::Uploading]
        );
    }

    /// A 0-byte placeholder folds into the folder badge as `Synced`
    /// ([`SyncState::effective_for_size`]) — it is present-and-empty, so a folder holding only
    /// empty files reads `Synced`, not `CloudOnly`, while a non-empty placeholder beside it
    /// stays `Placeholder`.
    #[test]
    fn descendant_states_folds_a_zero_byte_placeholder_to_synced() {
        let db = SyncDb::open_in_memory().unwrap();
        db.upsert_entry(
            "sub/empty.txt",
            None,
            None,
            None,
            SyncState::Placeholder,
            0,
            0,
            0, // size 0 → folds to Synced
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "sub/full.txt",
            None,
            None,
            None,
            SyncState::Placeholder,
            0,
            0,
            11, // non-empty → stays Placeholder
            1,
            None,
        )
        .unwrap();

        let mut states = db.descendant_states("sub").unwrap();
        states.sort_by_key(|s| s.as_str());
        assert_eq!(
            states,
            vec![SyncState::Placeholder, SyncState::Synced],
            "the 0-byte placeholder folds to Synced; the non-empty one stays Placeholder",
        );
    }

    /// The one special case in the size-aware badge rule: a 0-byte placeholder is
    /// present-and-empty, so it reads `Synced`. Every other (state, size) is unchanged.
    #[test]
    fn effective_for_size_maps_only_the_zero_byte_placeholder() {
        assert_eq!(
            SyncState::Placeholder.effective_for_size(0),
            SyncState::Synced,
            "a 0-byte placeholder is present-and-empty → Synced",
        );
        assert_eq!(
            SyncState::Placeholder.effective_for_size(11),
            SyncState::Placeholder,
            "a non-empty placeholder has real bytes to hydrate → unchanged",
        );
        // Every other state is unchanged regardless of size — a 0-byte Synced/Conflicted/…
        // still means exactly what it always did.
        for state in [
            SyncState::Synced,
            SyncState::LocallyModified,
            SyncState::RemotelyModified,
            SyncState::Conflicted,
            SyncState::Uploading,
            SyncState::Downloading,
            SyncState::Deleted,
        ] {
            assert_eq!(state.effective_for_size(0), state, "{state:?} @ size 0");
            assert_eq!(state.effective_for_size(99), state, "{state:?} @ size 99");
        }
    }

    /// The genesis's additive reconcile — the mechanism that replaced the
    /// hand-written `ALTER … ADD COLUMN` chain. Drop every column SQLite lets
    /// go of (nullable or defaulted, not a key, not indexed) from a genesis
    /// `sync.db`, reopen, and every table comes back at the block's shape: a
    /// column added to [`CREATE_TABLES_SQL`] reaches a long-lived database with
    /// no step written for it.
    #[test]
    fn reopening_reconciles_every_droppable_column_back() {
        use fauna_core::sqlite_schema_meta::{column_defs, managed_tables};
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("sync.db");
        drop(SyncDb::open(&db_path).unwrap());

        let reference = Connection::open_in_memory().unwrap();
        reference.execute_batch(CREATE_TABLES_SQL).unwrap();
        let shape = |c: &Connection, t: &str| {
            let mut cols: Vec<_> = column_defs(c, t)
                .unwrap()
                .into_iter()
                .map(|d| (d.name, d.ty, d.notnull, d.dflt))
                .collect();
            cols.sort();
            cols
        };
        let mut dropped = 0;
        {
            let conn = Connection::open(&db_path).unwrap();
            for table in managed_tables(&reference).unwrap() {
                for col in column_defs(&reference, &table).unwrap() {
                    if col.notnull && col.dflt.is_none() {
                        continue;
                    }
                    let sql = format!("ALTER TABLE \"{table}\" DROP COLUMN \"{}\"", col.name);
                    if conn.execute_batch(&sql).is_ok() {
                        dropped += 1;
                    }
                }
            }
        }
        assert!(dropped > 0, "the probe must actually drop columns");

        drop(SyncDb::open(&db_path).unwrap());
        let conn = Connection::open(&db_path).unwrap();
        for table in managed_tables(&reference).unwrap() {
            assert_eq!(
                shape(&conn, &table),
                shape(&reference, &table),
                "`{table}` is reconciled back to the genesis shape"
            );
        }
    }

    #[test]
    fn device_id_generation_and_persistence() {
        let db = SyncDb::open_in_memory().unwrap();

        // No device ID initially
        assert!(db.get_device_id().unwrap().is_none());

        // Generate one
        let id = db.get_or_create_device_id().unwrap();
        assert_ne!(id, [0u8; 32]); // extremely unlikely to be all zeros

        // Subsequent calls return the same ID
        let id2 = db.get_or_create_device_id().unwrap();
        assert_eq!(id, id2);

        // Direct get also returns it
        let id3 = db.get_device_id().unwrap().unwrap();
        assert_eq!(id, id3);
    }

    #[test]
    fn two_databases_get_different_device_ids() {
        let db1 = SyncDb::open_in_memory().unwrap();
        let db2 = SyncDb::open_in_memory().unwrap();

        let id1 = db1.get_or_create_device_id().unwrap();
        let id2 = db2.get_or_create_device_id().unwrap();

        // Random IDs should differ (collision probability: 2^-256)
        assert_ne!(id1, id2);
    }

    #[test]
    fn sync_entry_roundtrip() {
        let db = SyncDb::open_in_memory().unwrap();

        // Insert (with an M2 content-key generation stamp, to prove it round-trips)
        db.upsert_entry(
            "photos/cat.jpg",
            Some(test_hash(0xAA)),
            Some(test_hash(0xBB)),
            Some(test_hash(0xCC)),
            SyncState::Synced,
            1000,
            2000,
            4096,
            1,
            Some(3),
        )
        .unwrap();

        // Get
        let entry = db.get_entry("photos/cat.jpg").unwrap().unwrap();
        assert_eq!(entry.path, "photos/cat.jpg");
        assert_eq!(entry.local_hash, Some(test_hash(0xAA)));
        assert_eq!(entry.remote_hash, Some(test_hash(0xBB)));
        assert_eq!(entry.manifest_hash, Some(test_hash(0xCC)));
        assert_eq!(entry.state, SyncState::Synced);
        assert_eq!(entry.local_mtime, 1000);
        assert_eq!(entry.remote_mtime, 2000);
        assert_eq!(entry.size_bytes, 4096);
        assert_eq!(entry.version_num, 1);
        assert_eq!(entry.content_key_version, Some(3));

        // Update state
        db.update_state("photos/cat.jpg", SyncState::LocallyModified)
            .unwrap();
        let entry = db.get_entry("photos/cat.jpg").unwrap().unwrap();
        assert_eq!(entry.state, SyncState::LocallyModified);
    }

    #[test]
    fn list_by_state() {
        let db = SyncDb::open_in_memory().unwrap();

        db.upsert_entry(
            "a.txt",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            100,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "b.txt",
            None,
            None,
            None,
            SyncState::LocallyModified,
            0,
            0,
            200,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "c.txt",
            None,
            None,
            None,
            SyncState::LocallyModified,
            0,
            0,
            300,
            1,
            None,
        )
        .unwrap();
        db.upsert_entry(
            "d.txt",
            None,
            None,
            None,
            SyncState::Conflicted,
            0,
            0,
            400,
            1,
            None,
        )
        .unwrap();

        let synced = db.list_by_state(SyncState::Synced).unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].path, "a.txt");

        let modified = db.list_by_state(SyncState::LocallyModified).unwrap();
        assert_eq!(modified.len(), 2);
        let paths: Vec<&str> = modified.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"b.txt"));
        assert!(paths.contains(&"c.txt"));

        let conflicted = db.list_by_state(SyncState::Conflicted).unwrap();
        assert_eq!(conflicted.len(), 1);
        assert_eq!(conflicted[0].path, "d.txt");

        let uploading = db.list_by_state(SyncState::Uploading).unwrap();
        assert_eq!(uploading.len(), 0);
    }

    #[test]
    fn transfer_queue_roundtrip() {
        let db = SyncDb::open_in_memory().unwrap();

        let id1 = db
            .enqueue_transfer("file1.bin", "upload", test_hash(0x11), 5)
            .unwrap();
        let id2 = db
            .enqueue_transfer("file2.bin", "upload", test_hash(0x22), 10)
            .unwrap();
        let _id3 = db
            .enqueue_transfer("file3.bin", "download", test_hash(0x33), 1)
            .unwrap();

        // Pending uploads (ordered by priority DESC)
        let uploads = db.pending_transfers("upload").unwrap();
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[0].id, id2); // higher priority first
        assert_eq!(uploads[1].id, id1);

        // Pending downloads
        let downloads = db.pending_transfers("download").unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].path, "file3.bin");

        // Complete one, verify it's gone from pending
        db.complete_transfer(id1).unwrap();
        let uploads = db.pending_transfers("upload").unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].id, id2);

        // Complete the other
        db.complete_transfer(id2).unwrap();
        let uploads = db.pending_transfers("upload").unwrap();
        assert_eq!(uploads.len(), 0);
    }

    #[test]
    fn delete_entry() {
        let db = SyncDb::open_in_memory().unwrap();

        db.upsert_entry(
            "remove_me.txt",
            Some(test_hash(0xFF)),
            None,
            None,
            SyncState::Synced,
            0,
            0,
            64,
            1,
            None,
        )
        .unwrap();

        // Verify it exists
        assert!(db.get_entry("remove_me.txt").unwrap().is_some());

        // Delete marks as tombstone
        db.delete_entry("remove_me.txt").unwrap();

        // Entry still exists but is in Deleted state
        let entry = db.get_entry("remove_me.txt").unwrap().unwrap();
        assert_eq!(entry.state, SyncState::Deleted);
    }

    #[test]
    fn anchor_roundtrip() {
        let db = SyncDb::open_in_memory().unwrap();

        // Not set yet — should return 0
        assert_eq!(db.get_anchor().unwrap(), 0);

        // Set to 42
        db.set_anchor(42).unwrap();
        assert_eq!(db.get_anchor().unwrap(), 42);

        // Overwrite with 100
        db.set_anchor(100).unwrap();
        assert_eq!(db.get_anchor().unwrap(), 100);
    }

    #[test]
    fn tombstone_lifecycle() {
        let db = SyncDb::open_in_memory().unwrap();
        db.upsert_entry(
            "foo.txt",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            100,
            1,
            None,
        )
        .unwrap();

        // Mark as deleted
        db.delete_entry("foo.txt").unwrap();
        let entry = db.get_entry("foo.txt").unwrap().unwrap();
        assert_eq!(entry.state, SyncState::Deleted);

        // Purge with 0 TTL should remove it (entry was just created, so deleted_at <= now)
        std::thread::sleep(std::time::Duration::from_millis(1100)); // ensure > 1s has passed
        let purged = db.purge_tombstones(0).unwrap();
        assert_eq!(purged, 1);
        assert!(db.get_entry("foo.txt").unwrap().is_none());
    }

    #[test]
    fn conflict_tracking() {
        let db = SyncDb::open_in_memory().unwrap();

        db.record_conflict("file.txt", "merge_markers", Some("3 conflicts"))
            .unwrap();
        db.record_conflict("other.txt", "fork", None).unwrap();

        let conflicts = db.list_unresolved_conflicts().unwrap();
        assert_eq!(conflicts.len(), 2);

        // Resolve one
        db.resolve_conflict(conflicts[0].0).unwrap();
        let remaining = db.list_unresolved_conflicts().unwrap();
        assert_eq!(remaining.len(), 1);

        // Resolving it again is refused
        assert!(db.resolve_conflict(conflicts[0].0).is_err());
    }

    #[test]
    fn pause_resume_roundtrip() {
        let db = SyncDb::open_in_memory().unwrap();

        // Not paused initially
        assert!(!db.is_paused().unwrap());

        db.set_paused(true).unwrap();
        assert!(db.is_paused().unwrap());

        db.set_paused(false).unwrap();
        assert!(!db.is_paused().unwrap());
    }

    #[test]
    fn transfer_retry_backoff() {
        assert_eq!(retry_backoff_secs(0), 0);
        assert_eq!(retry_backoff_secs(1), 60);
        assert_eq!(retry_backoff_secs(2), 300);
        assert_eq!(retry_backoff_secs(3), 1800);
        assert_eq!(retry_backoff_secs(4), 14400);
        assert_eq!(retry_backoff_secs(10), 14400);
    }

    #[test]
    fn eligible_transfers_respects_backoff() {
        let db = SyncDb::open_in_memory().unwrap();

        let id = db
            .enqueue_transfer("file.txt", "upload", test_hash(1), 0)
            .unwrap();

        // First attempt (retry_count=0): always eligible
        assert_eq!(db.eligible_transfers("upload").unwrap().len(), 1);

        // Increment retry (simulates a failure just now)
        db.increment_retry(id).unwrap();

        // retry_count=1 requires 60s backoff — last_attempt_at is "now", so NOT eligible
        assert_eq!(db.eligible_transfers("upload").unwrap().len(), 0);
    }

    #[test]
    fn db_uses_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let db = SyncDb::open(dir.path().join("sync.db")).unwrap();
        assert_eq!(
            db.journal_mode().unwrap(),
            "wal",
            "database should use WAL journal mode"
        );
    }

    #[test]
    fn resolve_winner_conflicts_for_path_clears_unresolved_row() {
        let db = SyncDb::open_in_memory().unwrap();

        db.record_conflict("winner.txt", "concurrent_edit", Some("2 conflicts"))
            .unwrap();
        db.record_conflict("other.txt", "delete_declined", None)
            .unwrap();

        // The catch-up apply path checks before writing: winner.txt conflicts,
        // never-conflicted.txt does not.
        assert!(
            db.has_unresolved_winner_conflict_for_path("winner.txt")
                .unwrap()
        );
        assert!(
            !db.has_unresolved_winner_conflict_for_path("never-conflicted.txt")
                .unwrap()
        );

        // Catch-up applies the propagated winner for winner.txt: the local
        // unresolved conflict on that path is cleared, and the method reports
        // that it cleared one.
        assert!(db.resolve_winner_conflicts_for_path("winner.txt").unwrap());
        let remaining = db.list_unresolved_conflicts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].1, "other.txt");

        // Idempotent: a second apply for the same path (already resolved)
        // clears nothing and reports false.
        assert!(!db.resolve_winner_conflicts_for_path("winner.txt").unwrap());

        // A path with no recorded conflict reports false, not an error — the
        // common case on catch-up is applying a change the device never
        // conflicted on.
        assert!(
            !db.resolve_winner_conflicts_for_path("never-conflicted.txt")
                .unwrap()
        );
    }

    /// A skip row carries no winner: it neither arms the verbatim apply nor
    /// is cleared by it — only its own cure resolves it.
    #[test]
    fn a_skip_row_is_never_a_winner_conflict() {
        let db = SyncDb::open_in_memory().unwrap();
        db.record_skipped_change("a.txt", false, None, Some("path escapes the sync root"))
            .unwrap()
            .expect("first record");

        assert!(db.has_unresolved_conflict_for_path("a.txt").unwrap());
        assert!(!db.has_unresolved_winner_conflict_for_path("a.txt").unwrap());
        assert!(!db.resolve_winner_conflicts_for_path("a.txt").unwrap());
        assert_eq!(db.list_unresolved_conflicts().unwrap().len(), 1);
    }

    /// Once per path; reported once its `nest_id` lands; a seal-less hash row
    /// is never listed for report; the cure matches plaintext and hash rows and
    /// leaves a nest resolve owed until it is stamped.
    #[test]
    fn skip_rows_report_once_and_cure_by_path_or_hash() {
        let db = SyncDb::open_in_memory().unwrap();
        let plain = db
            .record_skipped_change("a.txt", false, None, Some("r1"))
            .unwrap()
            .unwrap();
        assert!(
            db.record_skipped_change("a.txt", false, None, Some("r1"))
                .unwrap()
                .is_none(),
            "a second record on the same path is refused while the first is unresolved"
        );
        let hash_hex = "ab".repeat(32);
        let sealed = db
            .record_skipped_change(&hash_hex, true, Some(b"blob"), Some("r2"))
            .unwrap()
            .unwrap();
        db.record_skipped_change("cd", true, None, Some("r3"))
            .unwrap()
            .unwrap();

        let unreported = db.list_unreported_catchup_failures().unwrap();
        assert_eq!(
            unreported.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![plain.id, sealed.id],
            "the seal-less hash row stays local"
        );
        assert_eq!(unreported[1].path_sealed.as_deref(), Some(&b"blob"[..]));

        db.set_conflict_nest_id(plain.id, 41).unwrap();
        assert_eq!(
            db.list_unreported_catchup_failures().unwrap().len(),
            1,
            "a landed report is not re-sent"
        );

        let cured = db
            .cure_catchup_failures_for_path("a.txt", &"00".repeat(32))
            .unwrap();
        assert_eq!(cured.len(), 1);
        assert_eq!(cured[0].nest_id, Some(41));
        let cured = db
            .cure_catchup_failures_for_path("b.txt", &hash_hex)
            .unwrap();
        assert_eq!(
            cured.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![sealed.id]
        );

        let owed = db.list_catchup_failures_owing_nest_resolve().unwrap();
        assert_eq!(
            owed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![plain.id]
        );
        db.set_conflict_nest_resolved(plain.id).unwrap();
        assert!(
            db.list_catchup_failures_owing_nest_resolve()
                .unwrap()
                .is_empty()
        );
        assert!(
            db.record_skipped_change("a.txt", false, None, Some("r1"))
                .unwrap()
                .is_some(),
            "a cured path may be skipped again"
        );
    }

    // ---- segment_backup_state / segment_backup_manifest_state (Plan 6 T5) ----

    fn actor(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn segment_hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn segment_backup_state_put_get_round_trip() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(1);
        // chunk_count=5, byte_size=12345, sidecar 321 bytes
        db.put_segment_backup_state("dest-A", &actor, "mail", 7, 5, 12345, Some(321))
            .unwrap();

        let got = db
            .get_segment_backup_state("dest-A", &actor, "mail", 7)
            .unwrap()
            .expect("inserted row should be visible");
        assert_eq!(got.last_chunk_count, 5);
        assert_eq!(got.last_byte_size, 12345);
        assert_eq!(got.last_meta_size, Some(321));
        assert!(got.last_synced_at > 0);
    }

    /// The sidecar half is its own column with its own NULL meaning: a row
    /// written `.dat`-only (a custodian view holding the `.dat` without its
    /// meta) reads back `None`, and the teardown work list reports the
    /// same fact as `meta_pushed`, so a tombstone is only ever aimed at a
    /// sidecar path that was actually created.
    #[test]
    fn segment_backup_state_tracks_whether_the_sidecar_was_pushed() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(6);
        db.put_segment_backup_state("dst", &actor, "mail", 1, 2, 100, None)
            .unwrap();
        db.put_segment_backup_state("dst", &actor, "mail", 2, 2, 100, Some(40))
            .unwrap();

        let listed = db.list_segment_backup_state("dst", &actor, "mail").unwrap();
        assert_eq!(listed[&1].last_meta_size, None);
        assert_eq!(listed[&2].last_meta_size, Some(40));

        let mut placed = db
            .list_backup_paths_for_destination("dst")
            .unwrap()
            .segments;
        placed.sort_by_key(|p| p.segment_id);
        assert_eq!(
            placed,
            vec![
                PlacedSegment {
                    kind: "mail".into(),
                    scope_id: actor.to_vec(),
                    segment_id: 1,
                    meta_pushed: false,
                },
                PlacedSegment {
                    kind: "mail".into(),
                    scope_id: actor.to_vec(),
                    segment_id: 2,
                    meta_pushed: true,
                },
            ]
        );

        // A later pass that pushes the sidecar upgrades the row in place.
        db.put_segment_backup_state("dst", &actor, "mail", 1, 2, 100, Some(41))
            .unwrap();
        assert_eq!(
            db.get_segment_backup_state("dst", &actor, "mail", 1)
                .unwrap()
                .unwrap()
                .last_meta_size,
            Some(41)
        );
    }

    #[test]
    fn segment_backup_state_missing_returns_none() {
        let db = SyncDb::open_in_memory().unwrap();
        let got = db
            .get_segment_backup_state("dst", &actor(2), "mail", 0)
            .unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn segment_backup_state_put_replaces_on_conflict() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(3);
        db.put_segment_backup_state("dst", &actor, "mail", 42, 3, 100, Some(10))
            .unwrap();
        db.put_segment_backup_state("dst", &actor, "mail", 42, 7, 200, Some(10))
            .unwrap();
        let got = db
            .get_segment_backup_state("dst", &actor, "mail", 42)
            .unwrap()
            .unwrap();
        assert_eq!(got.last_chunk_count, 7);
        assert_eq!(got.last_byte_size, 200);
    }

    #[test]
    fn segment_backup_state_list_filters_by_tuple() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor_a = actor(0xAA);
        let actor_b = actor(0xBB);

        // Same (dest, actor, kind) → 3 rows.
        for seg in [1u32, 2, 3] {
            db.put_segment_backup_state(
                "dst",
                &actor_a,
                "mail",
                seg,
                seg as u64,
                100 * seg as u64,
                Some(10),
            )
            .unwrap();
        }
        // Different actor — must not leak in.
        db.put_segment_backup_state("dst", &actor_b, "mail", 1, 9, 999, Some(10))
            .unwrap();
        // Different kind — must not leak in (conv kind; but DAO hardcodes 'mail', so this
        // would write a 'conv' kind row which list_segment_backup_state won't return).
        // We use the raw connection path here by inserting via a different kind would require
        // a future put_segment_backup_state generic DAO; for now verify the mail filter works.
        // Different destination — must not leak in.
        db.put_segment_backup_state("dst-2", &actor_a, "mail", 1, 8, 777, Some(10))
            .unwrap();

        let listed = db
            .list_segment_backup_state("dst", &actor_a, "mail")
            .unwrap();
        assert_eq!(listed.len(), 3);
        for seg in [1u32, 2, 3] {
            let row = listed.get(&seg).unwrap_or_else(|| {
                panic!("segment {seg} missing from list");
            });
            assert_eq!(row.last_chunk_count, seg as u64);
            assert_eq!(row.last_byte_size, 100 * seg as u64);
        }
    }

    #[test]
    fn segment_backup_state_delete_removes_row() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(5);
        db.put_segment_backup_state("dst", &actor, "mail", 9, 4, 500, Some(10))
            .unwrap();
        assert!(
            db.get_segment_backup_state("dst", &actor, "mail", 9)
                .unwrap()
                .is_some()
        );
        db.delete_segment_backup_state("dst", &actor, "mail", 9)
            .unwrap();
        assert!(
            db.get_segment_backup_state("dst", &actor, "mail", 9)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn segment_backup_manifest_state_put_get_round_trip() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(7);
        let hash = segment_hash(0xAB);

        assert!(
            db.get_segment_backup_manifest_state("dst", &actor, "mail")
                .unwrap()
                .is_none()
        );

        db.put_segment_backup_manifest_state("dst", &actor, "mail", &hash, true)
            .unwrap();
        let got = db
            .get_segment_backup_manifest_state("dst", &actor, "mail")
            .unwrap()
            .unwrap();
        assert_eq!(got, hash);

        // Replace.
        let hash2 = segment_hash(0xCD);
        db.put_segment_backup_manifest_state("dst", &actor, "mail", &hash2, true)
            .unwrap();
        let got2 = db
            .get_segment_backup_manifest_state("dst", &actor, "mail")
            .unwrap()
            .unwrap();
        assert_eq!(got2, hash2);

        // Different kind: the manifest DAO hardcodes kind='mail' in the INSERT
        // but uses the passed kind in the WHERE. A 'conv' lookup finds nothing.
        assert!(
            db.get_segment_backup_manifest_state("dst", &actor, "conv")
                .unwrap()
                .is_none()
        );
    }

    /// A bookkeeping-only manifest write (`content_moved: false` — the shape
    /// of an owner's first pass with zero segments, `manifest_changed` firing
    /// on `None != Some(empty_hash)` with nothing to upload or drop) must
    /// NOT advance the UI-facing status timestamp — that would read as "Last
    /// synced: just now" for an owner nothing of whose content has ever
    /// moved (`docs/goal/behavior/backup-destinations.md` § Per-destination status read,
    /// superseding an earlier rejected option (c) with a cleaner split:
    /// `last_synced_at` keeps advancing as pure bookkeeping — proven by the
    /// direct `get_segment_backup_manifest_state` round-trip above — while
    /// `max_manifest_synced_at_for_dest`, which is what the status row
    /// actually reads, must stay `None`).
    #[test]
    fn bookkeeping_only_manifest_write_does_not_advance_status_timestamp() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(9);

        assert_eq!(db.max_manifest_synced_at_for_dest("dst").unwrap(), None);

        // The manifest hash IS recorded (bookkeeping succeeds) even though
        // content_moved is false — this is the "first pass, zero segments"
        // shape: `manifest_changed` fires (None != Some(hash)), but nothing
        // was uploaded or dropped.
        let hash = segment_hash(0x00);
        db.put_segment_backup_manifest_state("dst", &actor, "mail", &hash, false)
            .unwrap();
        assert_eq!(
            db.get_segment_backup_manifest_state("dst", &actor, "mail")
                .unwrap(),
            Some(hash),
            "the manifest mirror's own hash-tracking state is unconditional bookkeeping"
        );
        assert_eq!(
            db.max_manifest_synced_at_for_dest("dst").unwrap(),
            None,
            "no real content moved yet ⇒ the status-facing timestamp must still read 'never'"
        );

        // A later pass that DOES move real content advances the status
        // timestamp — and a still-later bookkeeping-only write (e.g. the
        // owner deleted their only message, dropping back to zero) must not
        // erase it: `last_content_synced_at` keeps its last real value.
        let hash2 = segment_hash(0x11);
        db.put_segment_backup_manifest_state("dst", &actor, "mail", &hash2, true)
            .unwrap();
        let after_upload = db.max_manifest_synced_at_for_dest("dst").unwrap();
        assert!(after_upload.is_some(), "content moved ⇒ timestamp advances");

        let hash3 = segment_hash(0x22);
        db.put_segment_backup_manifest_state("dst", &actor, "mail", &hash3, false)
            .unwrap();
        assert_eq!(
            db.max_manifest_synced_at_for_dest("dst").unwrap(),
            after_upload,
            "a later bookkeeping-only write must not disturb the last REAL sync time"
        );
    }

    #[test]
    fn backup_destination_seen_record_list_forget() {
        let db = SyncDb::open_in_memory().unwrap();
        let actor = actor(3);

        assert!(db.list_backup_destinations_seen().unwrap().is_empty());

        // Upsert two destinations; re-recording updates the url (idempotent key).
        db.record_backup_destination_seen("dest-A", "https://a.example/", "__mail", &[7u8; 32])
            .unwrap();
        db.record_backup_destination_seen("dest-B", "https://b.example/", "__mail", &[8u8; 32])
            .unwrap();
        db.record_backup_destination_seen("dest-A", "https://a2.example/", "__mail", &[9u8; 32])
            .unwrap();

        let mut seen = db.list_backup_destinations_seen().unwrap();
        seen.sort_by(|a, b| a.dest_id.cmp(&b.dest_id));
        let rendered: Vec<_> = seen
            .iter()
            .map(|s| {
                (
                    s.dest_id.as_str(),
                    s.dest_url.as_str(),
                    s.folder.as_str(),
                    s.nest_id.clone(),
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                // dest-A's second write re-pins it: the pin follows the
                // destination row the owner's registry currently names.
                ("dest-A", "https://a2.example/", "__mail", vec![9u8; 32]),
                ("dest-B", "https://b.example/", "__mail", vec![8u8; 32]),
            ]
        );

        // forget drops the seen row AND the dest's segment/manifest state rows.
        db.put_segment_backup_state("dest-A", &actor, "mail", 0, 1, 100, Some(10))
            .unwrap();
        db.put_segment_backup_manifest_state("dest-A", &actor, "mail", &segment_hash(0x11), true)
            .unwrap();
        db.forget_backup_destination("dest-A").unwrap();

        let remaining = db.list_backup_destinations_seen().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].dest_id, "dest-B");
        assert_eq!(remaining[0].dest_url, "https://b.example/");
        assert!(
            db.list_segment_backup_state("dest-A", &actor, "mail")
                .unwrap()
                .is_empty(),
            "forget clears the dest's segment_backup_state",
        );
        assert!(
            db.get_segment_backup_manifest_state("dest-A", &actor, "mail")
                .unwrap()
                .is_none(),
            "forget clears the dest's manifest state",
        );
    }

    /// A `sync.db` written by a pre-folders-rename build (its
    /// `backup_destination_seen` table carries a `file_set` column) is
    /// REFUSED: the column-rename step that used to migrate it was retired
    /// 2026-09-24 by the compat-remnant sweep at its universal scope
    /// (`version-compatibility.md` § Dimension 2 — no pre-rename database
    /// rests anywhere). `CREATE TABLE IF NOT EXISTS` cannot re-shape the
    /// existing table, and the genesis's additive reconcile refuses the
    /// missing `folder` column (`NOT NULL` without a default), so the open
    /// itself fails naming it. The pin is the refusal: a rename step quietly
    /// re-added would be a remnant serving nothing.
    #[test]
    fn pre_rename_sync_db_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE backup_destination_seen (
                     dest_id    TEXT    PRIMARY KEY,
                     dest_url   TEXT    NOT NULL,
                     file_set   TEXT    NOT NULL,
                     updated_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
        }

        let err = match SyncDb::open(&path) {
            Ok(_) => panic!("a pre-rename table must be refused at open"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("folder"),
            "the refusal must name the missing column, got: {err}"
        );
    }

    /// A fresh `sync.db` declares the pin `NOT NULL` from its genesis.
    #[test]
    fn fresh_sync_db_declares_seen_nest_id_not_null() {
        let db = SyncDb::open_in_memory().unwrap();
        let err = db
            .conn
            .execute(
                "INSERT INTO backup_destination_seen (dest_id, dest_url, folder, nest_id, updated_at)
                 VALUES ('d', 'u', 'f', NULL, 1)",
                [],
            )
            .expect_err("a pin-less row must be refused");
        assert!(format!("{err}").contains("NOT NULL"), "got: {err}");
    }

    // ---- own_change_log (share-leg row retention, B2) ----

    fn own_row(seq: i64, path: &str) -> OwnChangeRow {
        OwnChangeRow {
            seq: Some(seq),
            path: path.to_string(),
            path_hash: "ab".repeat(32),
            path_sealed: Some(vec![0x5E; 8]),
            manifest_hash: Some("cd".repeat(32)),
            size_bytes: 1234,
            change_type: "create".to_string(),
            created_at: 1_700_000_000_000,
            content_key_version: Some(3),
            thumbnail_hash: Some("ef".repeat(32)),
            derived_through: Some(41),
            is_resolution: Some(false),
            author_actor_id: ACTOR_A.to_string(),
            device_id: "d1".repeat(16),
            signature: Some(vec![0x51; 64]),
            signer_key: Some(vec![0x4B; 32]),
            signer_cert: Some(vec![0xCE; 12]),
        }
    }

    /// A served row must be the RECORDED row — every field survives the
    /// round trip verbatim, so the serve-side conversion has nothing to
    /// guess (the synthesis trap this table exists to refuse).
    #[test]
    fn own_change_round_trips_verbatim() {
        let db = SyncDb::open_in_memory().unwrap();
        assert!(db.own_changes_since(0, 100).unwrap().is_empty());

        let row = own_row(7, "photos/holiday.mp4");
        db.retain_own_change(&row).unwrap();
        let got = db.own_changes_since(0, 100).unwrap();
        assert_eq!(got, vec![row], "the retained row IS the served row");

        // A delete row carries no manifest and survives the same way.
        let del = OwnChangeRow {
            manifest_hash: None,
            change_type: "delete".to_string(),
            content_key_version: None,
            thumbnail_hash: None,
            ..own_row(8, "photos/holiday.mp4")
        };
        db.retain_own_change(&del).unwrap();
        assert_eq!(db.own_changes_since(7, 100).unwrap(), vec![del]);
    }

    /// `since` is strict, order is seq, and `max_rows` bounds the page — the
    /// exact `ShareStore::changes_since` read contract.
    #[test]
    fn own_changes_since_pages_in_seq_order() {
        let db = SyncDb::open_in_memory().unwrap();
        // Insert out of order; the read must not care.
        for seq in [5, 2, 9, 3] {
            db.retain_own_change(&own_row(seq, &format!("f{seq}")))
                .unwrap();
        }
        let seqs = |rows: &[OwnChangeRow]| {
            rows.iter()
                .map(|r| r.seq.expect("own_changes_since serves sequenced rows only"))
                .collect::<Vec<_>>()
        };

        assert_eq!(seqs(&db.own_changes_since(2, 10).unwrap()), vec![3, 5, 9]);
        assert_eq!(
            seqs(&db.own_changes_since(0, 2).unwrap()),
            vec![2, 3],
            "the page cap bounds the reply; the rest comes on the next page"
        );
        assert!(db.own_changes_since(9, 10).unwrap().is_empty());
    }

    // ---- share_overlay (share-leg provisional overlay, B2) ----

    fn overlay_row(path: &str, seq: i64, sequenced: bool) -> ShareOverlayRow {
        ShareOverlayRow {
            path: path.to_string(),
            seq,
            sequenced,
            change_type: "create".to_string(),
            manifest_hash: Some("cd".repeat(32)),
            size_bytes: 512,
            content_key_version: Some(1),
            proven_author: ACTOR_B.to_string(),
            materialized: false,
            content_hash: None,
        }
    }

    /// Latest-per-path: a later ingest replaces; the ONE provable staleness
    /// (both nest-sequenced, lower seq) is skipped — everything else has no
    /// cross-replica order to compare and last-ingested wins.
    #[test]
    fn overlay_is_latest_per_path_with_the_one_staleness_skip() {
        let db = SyncDb::open_in_memory().unwrap();
        assert!(
            db.upsert_share_overlay(&overlay_row("a.mp4", 5, true))
                .unwrap()
        );
        assert!(
            db.upsert_share_overlay(&overlay_row("a.mp4", 7, true))
                .unwrap()
        );
        assert_eq!(db.get_share_overlay("a.mp4").unwrap().unwrap().seq, 7);

        // Provably staler: sequenced-vs-sequenced with a lower seq — skipped.
        assert!(
            !db.upsert_share_overlay(&overlay_row("a.mp4", 6, true))
                .unwrap()
        );
        assert_eq!(db.get_share_overlay("a.mp4").unwrap().unwrap().seq, 7);

        // An un-sequenced row has no comparable seq — it replaces.
        assert!(
            db.upsert_share_overlay(&overlay_row("a.mp4", 2, false))
                .unwrap()
        );
        let row = db.get_share_overlay("a.mp4").unwrap().unwrap();
        assert!(!row.sequenced);
        assert_eq!(row.seq, 2);
    }

    /// The materialization stamp and the reconcile's retire verb.
    #[test]
    fn overlay_materialize_stamp_and_retire() {
        let db = SyncDb::open_in_memory().unwrap();
        db.upsert_share_overlay(&overlay_row("b.txt", 3, true))
            .unwrap();
        db.mark_share_overlay_materialized("b.txt", Some(&"ef".repeat(32)))
            .unwrap();
        let row = db.get_share_overlay("b.txt").unwrap().unwrap();
        assert!(row.materialized);
        assert_eq!(row.content_hash.as_deref(), Some(&*"ef".repeat(32)));
        assert_eq!(db.list_share_overlay().unwrap().len(), 1);

        db.remove_share_overlay("b.txt").unwrap();
        assert!(db.get_share_overlay("b.txt").unwrap().is_none());
        assert!(db.list_share_overlay().unwrap().is_empty());
    }

    /// Retention is idempotent on seq: the nest assigns each seq once and a
    /// sequenced row is immutable, so a retried write is a no-op — never a
    /// duplicate, never an error.
    #[test]
    fn a_replayed_retention_write_is_a_no_op() {
        let db = SyncDb::open_in_memory().unwrap();
        db.retain_own_change(&own_row(4, "a.txt")).unwrap();
        // Same seq, different contents — the first write wins untouched.
        db.retain_own_change(&own_row(4, "b.txt")).unwrap();
        let got = db.own_changes_since(0, 10).unwrap();
        assert_eq!(got.len(), 1, "one row per seq");
        assert_eq!(got[0].path, "a.txt", "the recorded row stays immutable");
    }

    // ---- relayed_change_log (the relayed-row lift) ----

    fn relayed(seq: Option<i64>, author: &str, path_byte: &str, body: u8) -> RelayedChangeRow {
        RelayedChangeRow {
            seq,
            author_actor_id: author.to_string(),
            path_hash: path_byte.repeat(32),
            row: vec![body; 16],
            signer_cert: Some(vec![0xCE; 4]),
        }
    }

    /// Sequenced relayed rows page like own rows (strict `since`, seq order,
    /// idempotent on seq); a writer's pending row is latest per (writer, path)
    /// and retires when that writer's sequenced row for the path lands — while
    /// ANOTHER writer's pending row for the same path stands.
    #[test]
    fn relayed_rows_page_by_seq_and_pending_is_latest_per_writer_and_path() {
        let db = SyncDb::open_in_memory().unwrap();
        db.retain_relayed_change(&relayed(Some(9), "a1", "p1", 1))
            .unwrap();
        db.retain_relayed_change(&relayed(Some(4), "a1", "p2", 2))
            .unwrap();
        db.retain_relayed_change(&relayed(Some(9), "a1", "p1", 3))
            .unwrap();
        let got = db.relayed_changes_since(4, 10).unwrap();
        assert_eq!(
            got,
            vec![relayed(Some(9), "a1", "p1", 1)],
            "strict since, first write wins"
        );

        db.retain_relayed_change(&relayed(None, "a1", "p3", 4))
            .unwrap();
        db.retain_relayed_change(&relayed(None, "a1", "p3", 5))
            .unwrap();
        db.retain_relayed_change(&relayed(None, "b2", "p3", 6))
            .unwrap();
        let pending = db.relayed_pending_changes(10).unwrap();
        assert_eq!(
            pending,
            vec![relayed(None, "a1", "p3", 5), relayed(None, "b2", "p3", 6)]
        );
        assert!(db.relayed_changes_since(9, 10).unwrap().is_empty());

        db.retain_relayed_change(&relayed(Some(12), "a1", "p3", 7))
            .unwrap();
        assert_eq!(
            db.relayed_pending_changes(10).unwrap(),
            vec![relayed(None, "b2", "p3", 6)],
            "the nest's word supersedes that writer's offline row, no one else's"
        );
    }

    // ---- own_change_log, the own-PENDING shape (B2.5) ----

    fn pending_row(path: &str, manifest_byte: &str) -> OwnChangeRow {
        OwnChangeRow {
            seq: None,
            manifest_hash: Some(manifest_byte.repeat(32)),
            thumbnail_hash: None,
            ..own_row(0, path)
        }
    }

    /// At most one pending row per path: a re-mint (reconnect re-drive, or a
    /// further offline edit with a new manifest) replaces the previous one —
    /// only the newest local state is worth serving. Pending rows are
    /// invisible to the sequenced read.
    #[test]
    fn a_pending_mint_is_latest_per_path() {
        let db = SyncDb::open_in_memory().unwrap();
        db.mint_pending_own_change(&pending_row("cabin/draft.txt", "aa"))
            .unwrap();
        db.mint_pending_own_change(&pending_row("cabin/draft.txt", "bb"))
            .unwrap();

        let pending = db.own_pending_changes(10).unwrap();
        assert_eq!(pending.len(), 1, "one pending row per path");
        assert_eq!(pending[0].manifest_hash, Some("bb".repeat(32)));
        assert_eq!(pending[0].seq, None);
        assert!(
            db.own_changes_since(0, 10).unwrap().is_empty(),
            "a pending row never serves from the sequenced read"
        );
    }

    /// The funnel's `Ok(seq)` upgrade is in-place: the same write that lands
    /// the sequenced row retires the path's pending row, so the store never
    /// holds both forms of one change — and a record for one path never
    /// touches another path's pending row.
    #[test]
    fn the_upgrade_retires_the_paths_pending_row_in_the_same_write() {
        let db = SyncDb::open_in_memory().unwrap();
        db.mint_pending_own_change(&pending_row("cabin/draft.txt", "aa"))
            .unwrap();
        db.mint_pending_own_change(&pending_row("cabin/other.txt", "cc"))
            .unwrap();

        db.retain_own_change(&own_row(9, "cabin/draft.txt"))
            .unwrap();

        let sequenced = db.own_changes_since(0, 10).unwrap();
        assert_eq!(
            sequenced.len(),
            1,
            "exactly one sequenced row — no duplicate"
        );
        assert_eq!(sequenced[0].seq, Some(9));
        let pending = db.own_pending_changes(10).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "the upgraded path's pending row is gone; the other path's stands"
        );
        assert_eq!(pending[0].path, "cabin/other.txt");
    }

    /// Decision 2′ (d): a floor read withdraws only the pending rows sealed
    /// below it — never a sequenced row, an unstamped one, or one at the floor.
    #[test]
    fn a_floor_read_withdraws_only_pending_rows_sealed_below_it() {
        let db = SyncDb::open_in_memory().unwrap();
        let stamped = |path: &str, version: Option<u64>| OwnChangeRow {
            content_key_version: version,
            ..pending_row(path, "aa")
        };
        db.mint_pending_own_change(&stamped("behind.txt", Some(1)))
            .unwrap();
        db.mint_pending_own_change(&stamped("at.txt", Some(2)))
            .unwrap();
        db.mint_pending_own_change(&stamped("owner-only.txt", None))
            .unwrap();
        db.retain_own_change(&OwnChangeRow {
            content_key_version: Some(1),
            ..own_row(4, "landed.txt")
        })
        .unwrap();

        assert_eq!(db.retire_own_pending_below(2).unwrap(), 1);
        let mut pending: Vec<String> = db
            .own_pending_changes(10)
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect();
        pending.sort();
        assert_eq!(pending, ["at.txt", "owner-only.txt"]);
        assert_eq!(db.own_changes_since(0, 10).unwrap().len(), 1);
    }

    /// The two mint doors refuse the wrong shape loudly — a seq-less retain
    /// and a sequenced mint are both caller bugs, not silent writes.
    #[test]
    fn the_pending_and_sequenced_doors_refuse_the_wrong_shape() {
        let db = SyncDb::open_in_memory().unwrap();
        assert!(
            db.retain_own_change(&pending_row("a.txt", "aa")).is_err(),
            "retain requires a nest-assigned seq"
        );
        assert!(
            db.mint_pending_own_change(&own_row(3, "a.txt")).is_err(),
            "mint is the seq-less door"
        );
    }
}
