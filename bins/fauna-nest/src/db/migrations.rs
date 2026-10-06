//! The nest database's schema — the genesis — and the runner that applies it.
//!
//! # Genesis
//!
//! The whole schema is the **genesis**: the `CREATE … IF NOT EXISTS` blocks
//! below, applied in [`GENESIS_BLOCKS`] order by [`apply_genesis`] on every
//! boot, so a table or index added to a block reaches an existing database the
//! same way it reaches a fresh one. It was collapsed twice under the
//! compat-remnant sweep's baseline reset
//! (`docs/goal/architecture/version-compatibility.md` § Dimension 2, the fourth
//! ratified exception): on 2026-09-24 from the 78-step history that had built
//! the schema up, and on 2026-10-04 from the one-shot steps of the 46 versions
//! that followed (79..=124), as the last schema change before the public
//! repository was minted. No nest database predating it exists anywhere, so no
//! migration step, boot reconcile or backfill written for one is kept, and the
//! schema-version numbering continues past the retired numbers with the reader
//! floor at the genesis ([`CURRENT_SCHEMA_VERSION`]; [`check_genesis`] refuses
//! a database this genesis did not write). The shape is pinned by
//! `genesis_shape.txt` (`db/genesis_shape.rs`); every schema change updates the
//! pin in the same commit.
//!
//! **What stays is the upgrade mechanism**, and it is how every change from the
//! genesis on is made:
//!
//! - **An additive change** — a new table or index, or a new column that is
//!   nullable or carries a constant default — is an edit to its block. A new
//!   column reaches an existing database through [`reconcile_added_columns`],
//!   with no hand-written `ALTER`. Bump [`CURRENT_SCHEMA_VERSION`] with a ledger
//!   line; [`MIN_READER_SCHEMA_VERSION`] stays put.
//! - **An index over a column added to an existing table** must be created in
//!   [`run_migrations`] *after* the reconcile, never in the block: in the block
//!   it runs against the old table before the reconcile adds the column, and
//!   every boot of a long-lived `/data` crashes with "no such column" (the
//!   2026-08-03 example.com outage).
//! - **A column whose correct value is per-row** lands nullable with no default
//!   plus a boot pass that derives each row's value — a `DEFAULT` is a constant
//!   written onto every existing row (version-compatibility.md § Dimension 1).
//! - **A non-additive change** is expand→migrate→contract (§ 2.1): a guarded
//!   step in [`run_migrations`] that runs once, and the contract step raises
//!   [`MIN_READER_SCHEMA_VERSION`].
//!
//! A standing boot pass in [`run_migrations`] exists only while a CURRENT writer
//! still produces its input; a pass written for a database some earlier binary
//! left behind is removed with that binary's last supported release.
//!
//! # No user-data loss (I1)
//!
//! The at-rest DB is a production compatibility surface governed by the
//! no-user-data-loss invariant **I1** in
//! `docs/goal/architecture/version-compatibility.md` § 1 (the data-layer
//! corollary of `nest/common.md` § Client-state recoverability): no migration
//! may destroy data a user or admin cannot recreate — not even at a major bump
//! (a major bump *migrates* via expand→migrate→contract, § 2.1, never drops).
//!
//! Every destructive statement a step adds (`DROP TABLE`, `DELETE FROM`, a
//! wipe) carries an inline `I1:` classification in one of four buckets:
//!   - **(a) inert legacy supersession** — drops a table/rows a now-removed code
//!     path created, whose replacement shipped before any supported database
//!     could hold data there. (Verified by: the replacement table's `CREATE` +
//!     the legacy writer being absent from the tree.)
//!   - **(b) ephemeral / derived / lock** — re-derivable on demand (locks
//!     re-acquired, cursors re-scanned, caches recomputed). The explicit I1
//!     carve-out: nothing a user owns is lost.
//!   - **(c) data-preserving rebuild** — expand→migrate→contract (§ 2.1): copies
//!     every row into the new table *before* dropping the old. Not a data-loss
//!     site (the canonical *correct* non-additive change).
//!   - **(d) dead schema** — `DROP TABLE IF EXISTS` of a table never wired to any
//!     reader/writer (no data ever existed). No-op cleanup.
//!
//! A bare `DROP INDEX` is never a data-loss site (indexes are derived). A
//! genuine non-additive user-data change is never a drop: it is the staging of
//! § 2.1.
//!
//! # Sealed labels and hash companions
//!
//! Columns marked `SEALED LABEL` below hold an opaque
//! `fauna_core::path_crypto::SealedLabel` envelope over a user-chosen name or
//! path (`docs/goal/behavior/file-sync.md` § Sealed names & paths, implementing
//! the 2026-07-29 paths-are-content ruling). The nest never holds the key: it
//! stores, copies and serves the blob without reading it, and the two nonce
//! modes named per column are load-bearing — convergent only where the salt
//! determines the plaintext, explicit-random for anything mutable under a fixed
//! salt. Columns marked `HASH COMPANION` are the equality-only addressing keys
//! that let server-side logic stay hash-keyed while the plaintext does not
//! rest; they are 1:1 with their plaintext, nest-computable, and filled in by
//! [`reconcile_path_sealing_companions`] wherever a writer left one unset.
//! [`run_scrub_plaintext`] clears a plaintext sibling wherever its sealed twin
//! rests ([`SCRUB_PLANES`]).
//!
//! # ⚠ No commas in a column's own inline comment
//!
//! SQLite's `ALTER TABLE … DROP COLUMN` locates the column's text span by
//! scanning for the preceding comma **without skipping comments**, so a comma
//! inside any `--` comment in a `CREATE TABLE` misaligns the boundary SQLite
//! computes for the column below it AND for every later column in the same
//! statement — the `DROP COLUMN` that fails with `incomplete input` is
//! usually a *later* column's, not the one whose comment carries the comma.
//! The `reconciler_restores_dropped_additive_columns_for_every_table` guard
//! below fails loudly on that error (it used to skip the column and lose
//! coverage silently), naming the column it could not drop and printing every
//! comma-bearing `--` comment line in that table's stored schema text, so the
//! offender is one glance away rather than one wrong-column search away.
//! Parentheses and non-ASCII are fine; commas are not. Write column comments
//! in comma-free prose (an em dash or a period reads fine), and keep
//! comma-bearing prose in the constant's `///` doc comment, which is not part
//! of the stored schema text.

use anyhow::{Context, Result};
#[cfg(test)]
use fauna_core::sqlite_schema_meta::{column_defs, managed_tables};
use rusqlite::Connection;

// ── Schema version (version-compatibility.md § 2.2) ──────────────────────────
//
// Two binary constants, both recorded in the additive single-row `schema_meta`
// table at the end of `run_migrations`. They let an *older* binary that opens a
// *newer* DB tell "newer-but-additive" (keep operating — I2 backward-compat)
// from "newer-and-breaking" (boot degraded + surface `fauna.nest.outdated`,
// never a destructive migration). See `check_schema_compatibility`.

/// The shape this binary's genesis blocks describe. Bump on **every** schema
/// change, additive or not — the DB's "what shape am I" stamp — with a ledger
/// line below naming what the version added.
///
///  125 = the genesis (2026-10-04, § Genesis in the module doc): the schema as
///  it stood when the 79..=124 run was collapsed into the blocks, with
///  `upload_leases.actor_id` and `folder_content_keys.current_version` declared
///  `NOT NULL`. The numbering CONTINUES past the retired numbers
///  ([`RETIRED_LINEAGE_MAX_SCHEMA_VERSION`]) rather than restarting: a binary
///  of the first history (1..=78) has no genesis check and reads the stamp with
///  the same § 2.2 rule this binary runs ([`classify_schema`]), and only a pair
///  above its own version is `Incompatible` to it. A database stamped with a
///  retired number is refused by [`check_genesis`] before its stamp is ever
///  read.
pub const CURRENT_SCHEMA_VERSION: u32 = 125;

/// The oldest `schema_version` whose binary can still safely **operate** a DB
/// this binary writes. Bump **only** on a non-additive/breaking change (the
/// *contract* step of § 2.1) — an additive change leaves it untouched, which is
/// exactly what keeps an older binary working against a newer additive DB.
/// Raising it is what makes an older binary treat a DB as `Incompatible` and
/// boot the degraded needs-update serve (`SchemaIncompatible`) rather than fail
/// lazily at its first query. The floor sits AT the genesis: no binary of a
/// retired number may operate a genesis database (version-compatibility.md
/// § 2.2, the genesis paragraph).
pub const MIN_READER_SCHEMA_VERSION: u32 = 125;

/// The last schema number retired by a genesis collapse (1..=78 by the first,
/// 79..=124 by the second) — kept only as the bound the genesis numbering must
/// clear. A binary of the first history classifies a stamp with
/// [`classify_schema`] at its own version, so a [`MIN_READER_SCHEMA_VERSION`]
/// above this number is what makes it boot degraded on a genesis database
/// instead of migrating it; a binary of the 79..=124 run refuses the file at
/// its own genesis check ([`NEST_DB_APPLICATION_ID`]).
pub const RETIRED_LINEAGE_MAX_SCHEMA_VERSION: u32 = 124;

// Compile-time pins: the genesis pair clears every retired number, and the
// floor never runs ahead of the shape.
const _: () = assert!(MIN_READER_SCHEMA_VERSION > RETIRED_LINEAGE_MAX_SCHEMA_VERSION);
const _: () = assert!(CURRENT_SCHEMA_VERSION >= MIN_READER_SCHEMA_VERSION);

/// What an unstamped DB (no `schema_meta` table or row) reads as. The only such
/// database [`check_genesis`] admits is an empty one — a nest's first boot — so
/// it reads as the genesis.
const BASELINE_SCHEMA_VERSION: u32 = 125;

/// `PRAGMA application_id` of every nest database the genesis writes — the
/// SQLite header's file-format mark, which [`check_genesis`] requires. Unlike
/// the schema version it is a property of the schema LINE, not of a shape: it
/// changes only when the genesis is collapsed. The first genesis (schema
/// 79..=124) stamped "NEST"; this one stamps "NST2", so a database of that run
/// is refused at open rather than read as merely older.
pub const NEST_DB_APPLICATION_ID: i32 = i32::from_be_bytes(*b"NST2");

pub(super) const MIGRATIONS: &str = "
    CREATE TABLE IF NOT EXISTS tiers (
        name TEXT PRIMARY KEY,
        max_inbox_bytes INTEGER NOT NULL,
        max_storage_bytes INTEGER NOT NULL,
        max_devices INTEGER NOT NULL,
        max_blob_size INTEGER NOT NULL,
        max_feeds INTEGER NOT NULL DEFAULT 5
    );

    CREATE TABLE IF NOT EXISTS users (
        actor_id BLOB PRIMARY KEY,
        tier TEXT NOT NULL REFERENCES tiers(name),
        label TEXT NOT NULL DEFAULT '',
        suspended INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        inbox_bytes_used INTEGER NOT NULL DEFAULT 0,
        storage_bytes_used INTEGER NOT NULL DEFAULT 0,
        handle TEXT DEFAULT '' NOT NULL,
        eviction_status TEXT NOT NULL DEFAULT '',
        eviction_reason TEXT NOT NULL DEFAULT '',
        eviction_category TEXT NOT NULL DEFAULT '',
        eviction_warned_at INTEGER,
        eviction_suspend_at INTEGER,
        eviction_delete_at INTEGER,
        locked_until INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_users_handle
        ON users(handle) WHERE handle != '';

    CREATE TABLE IF NOT EXISTS audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        action TEXT NOT NULL,
        target TEXT,
        detail TEXT,
        prev_hash TEXT NOT NULL DEFAULT '',
        entry_hash TEXT NOT NULL DEFAULT '',
        entry_hash_version INTEGER NOT NULL DEFAULT 0,
        actor_id BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts DESC);
";

/// Single-row schema-version stamp (version-compatibility.md § 2.2) — a table
/// because a `PRAGMA user_version` holds only one 32-bit int. The row is written/refreshed at the end of
/// `run_migrations` by `record_schema_meta`; an absent table/row reads as the
/// `BASELINE_SCHEMA_VERSION`. The shared DDL every native SQLite-backed store
/// composing the scheme uses (`fauna-mls`'s `mls.db` is the other).
pub(super) const MIGRATIONS_SCHEMA_META: &str = fauna_core::sqlite_schema_meta::SCHEMA_META_DDL;

/// The built-in tiers. `INSERT OR IGNORE`, so an admin's edit to a seeded tier
/// survives every boot's re-seed.
///
/// The fourth row is the storage-only **`backup` tier** for held-for-friends
/// backups (`docs/goal/behavior/backup-destinations.md` § State & data shape →
/// *Held-for-friends enrollment*; `docs/goal/behavior/admin.md` § 2 Users). A
/// holding nest's admin admits a data owner at this tier through the existing
/// admission lifecycle; the guest's handle-less `users` row can then own only
/// reserved folders and consume storage against `max_storage_bytes` —
/// `max_inbox_bytes = 0` (no MX / inbox / plaintext-readable content) and
/// `max_feeds = 0` (no feeds) are what make the tier storage-only.
/// `max_storage_bytes` (the offered space) is an admin-editable default;
/// `max_devices` / `max_blob_size` size the backup chunking.
///
/// `free`'s `max_devices` is 3 (ruled 2026-09-26; `docs/goal/behavior/admin.md`
/// § 2 Users → *Device enforcement*): a laptop, a phone, and one more
/// co-located app, which keeps its own `sync_devices` row. Every seed here is
/// a fresh-nest default only — `INSERT OR IGNORE` leaves a provisioned nest's
/// admin-set values alone.
pub(super) const SEED_TIERS: &str = "
    INSERT OR IGNORE INTO tiers
        (name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size, max_feeds)
    VALUES
        ('free',      104857600,   104857600,   3,  10485760,  5),
        ('personal',  1073741824,  10737418240, 5,  104857600, 5),
        ('community', 5368709120,  53687091200, 10, 524288000, 5),
        ('backup',    0,           53687091200, 5,  104857600, 0);
";

pub(super) const MIGRATIONS_WORKER: &str = "
    CREATE TABLE IF NOT EXISTS worker_replication (
        payload_type TEXT NOT NULL,
        payload_key BLOB NOT NULL,
        inbox_row_id INTEGER,
        replicated_at INTEGER NOT NULL,
        PRIMARY KEY (payload_type, payload_key, inbox_row_id)
    );
";

/// The durable idempotency tier (W4 (account-data-plane.md § Workstreams) phase 3 — `account-data-plane.md` § The
/// offline-mutation contract → *Nest-side durable idempotency*): one recorded
/// Reply per `(actor, idempotency_key)`, consulted by the per-actor dispatch
/// sink on a per-connection-cache miss so a replay across a reconnect returns
/// the original outcome instead of re-running the handler. `reply IS NULL` is
/// the too-large marker. Ephemeral by contract — a replay cache with a 7-day
/// retention sweep (`db/rpc_idempotency.rs`), never the only copy of anything:
/// dropping a row costs one re-run of a handler the registry already audits as
/// naturally idempotent, which was the universal pre-tier behavior.
pub(super) const MIGRATIONS_RPC_IDEMPOTENCY: &str = "
    CREATE TABLE IF NOT EXISTS rpc_idempotency (
        actor_id   BLOB NOT NULL,
        idem_key   BLOB NOT NULL,
        kind       TEXT NOT NULL,
        reply      BLOB,
        ok         INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (actor_id, idem_key)
    );
    CREATE INDEX IF NOT EXISTS idx_rpc_idempotency_created
        ON rpc_idempotency(created_at);
";

pub(super) const MIGRATIONS_BLOB: &str = "
    CREATE TABLE IF NOT EXISTS blob_metadata (
        hash            BLOB PRIMARY KEY,
        size_bytes      INTEGER NOT NULL,
        content_type    TEXT NOT NULL,
        created_at      INTEGER NOT NULL,
        last_accessed   INTEGER NOT NULL,
        storage_local   INTEGER NOT NULL DEFAULT 1,
        storage_s3      INTEGER NOT NULL DEFAULT 0,
        storage_nodes   TEXT,
        ref_count       INTEGER NOT NULL DEFAULT 1,
        has_c2pa INTEGER,
        thumbnail_hash BLOB
    );
";

pub(super) const MIGRATIONS_EMAIL: &str = "
    -- Per-recipient outbound mail queue. Implements
    -- docs/goal/behavior/smtp-server.md § Outbound delivery: one row per
    -- (message, recipient) pair so each recipient runs its own retry curve.
    CREATE TABLE IF NOT EXISTS outbound_mail_queue (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        original_msgid    TEXT NOT NULL,
        original_sender   TEXT NOT NULL,
        recipient         TEXT NOT NULL,
        raw_message       BLOB NOT NULL,
        attempt_count     INTEGER NOT NULL DEFAULT 0,
        next_attempt_at   INTEGER NOT NULL,
        delay_warned_at   INTEGER,
        status            TEXT NOT NULL DEFAULT 'pending',
        last_error        TEXT,
        last_enhanced     TEXT,
        inbound_spf       TEXT NOT NULL DEFAULT '',
        inbound_dmarc     TEXT NOT NULL DEFAULT '',
        inbound_dmarc_pol TEXT NOT NULL DEFAULT '',
        is_forwarded      INTEGER NOT NULL DEFAULT 0,
        -- Forward attribution (mail-forwarding.md N2): NULL on a normal
        -- outbound submission; set on a forwarded row (is_forwarded = 1) to
        -- the forwarding actor + the rule id (forward-all, or a filter rule
        -- id) so the SRS rewrite at queue-out (N3) and NDR routing (N4) can
        -- read them off the row.
        forward_actor_id  BLOB,
        forward_rule_id   TEXT,
        created_at        INTEGER NOT NULL,
        -- Submission attribution (mail-app-surface.md § Outbound metering):
        -- the AUTHENTICATED actor that put this row on the queue through
        -- `fauna.email.send`. The per-hour outbound ceiling counts by this
        -- column, because the alternative -- `original_sender`, the caller's
        -- own `From:` header -- is a value the caller chooses: an off-domain
        -- `From:` deliberately bypasses the handle gate, so a fresh string
        -- each message was a fresh counter each message.
        -- NULL on rows from every other producer (raw-SMTP submission,
        -- forwards, bounces, TLS reports, list fan-out), each of which carries
        -- its own metering -- which is why the count is a floor and never an
        -- authorization answer.
        submit_actor_id   BLOB,
        -- The forward's copy mode (mail-forwarding.md § Per-rule forward
        -- to): 'copy' when the original also rests in the mailbox (this
        -- row is a SECOND copy); 'redirect' when it does not (this row is
        -- the ONLY copy of mail the nest already answered 250 for). NULL on
        -- every non-forward row and on a forward enqueued before the column
        -- existed (read as unknown -- never as 'copy'). A succession burns
        -- only a 'copy' row (`successions.rs::rule_the_forwarding_family`).
        forward_copy_mode TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_outbound_mail_queue_due
        ON outbound_mail_queue(status, next_attempt_at);
    CREATE INDEX IF NOT EXISTS idx_outbound_mail_queue_msgid
        ON outbound_mail_queue(original_msgid, recipient);
    CREATE INDEX IF NOT EXISTS idx_outbound_mail_queue_sender
        ON outbound_mail_queue(original_sender, created_at);
    -- Backs `count_outbound_by_actor_window`, the per-hour outbound ceiling.
    CREATE INDEX IF NOT EXISTS idx_outbound_mail_queue_submit_actor
        ON outbound_mail_queue(submit_actor_id, created_at);

    -- NDR rate-limit history per docs/goal/behavior/smtp-server.md
    -- § NDR rate-limit per recipient. One row per emitted bounce; the
    -- (original_sender, original_msgid) tuple is checked against a
    -- 7-day window before any new bounce is sent, suppressing the
    -- second-and-following bounces in that window as
    -- smtp_outbound_bounces_total{verdict=\"suppressed_rate\"}.
    CREATE TABLE IF NOT EXISTS bounce_history (
        original_sender   TEXT NOT NULL,
        original_msgid    TEXT NOT NULL,
        sent_at           INTEGER NOT NULL,
        PRIMARY KEY (original_sender, original_msgid, sent_at)
    );
    CREATE INDEX IF NOT EXISTS idx_bounce_history_window
        ON bounce_history(original_sender, original_msgid, sent_at);

    -- Daily TLSRPT outbound aggregate reports per
    -- docs/goal/behavior/smtp-server.md § TLSRPT outbound reporter.
    -- One row per (recipient_domain, report_date); the gzipped JSON
    -- payload is retained for 7 days for admin inspection.
    CREATE TABLE IF NOT EXISTS tlsrpt_outbound_reports (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        report_id         TEXT NOT NULL,
        recipient_domain  TEXT NOT NULL,
        report_date       TEXT NOT NULL,
        transport         TEXT NOT NULL,
        destination_uri   TEXT NOT NULL,
        payload_json      BLOB NOT NULL,
        submitted_at      INTEGER NOT NULL,
        UNIQUE (recipient_domain, report_date, transport, destination_uri)
    );
    CREATE INDEX IF NOT EXISTS idx_tlsrpt_outbound_retention
        ON tlsrpt_outbound_reports(submitted_at);

    -- Periodic + on-demand DNSBL self-check history per
    -- docs/goal/behavior/mail-deliverability.md § Blocklist self-check.
    -- One row per check (the 24h timer or an admin force-refresh); the
    -- per-DNSBL outcome is a JSON object keyed by server. Retained 90 days
    -- (mail.outbound.self_blocklist_retention_days).
    CREATE TABLE IF NOT EXISTS mail_outbound_self_blocklist_check (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        checked_at   INTEGER NOT NULL,
        results_json TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_mail_outbound_self_blocklist_check_time
        ON mail_outbound_self_blocklist_check(checked_at);

    -- Admin-on-demand deliverability-diagnostic run audit per
    -- docs/goal/behavior/mail-deliverability.md § Admin-visible audit.
    -- One row per `run_deliverability_diagnostics` call; `results_json` is
    -- the JSON checklist, `ran_by_actor_id` the admin who ran it. Admin-only
    -- access; retained 90 days (mail.outbound.diagnostic_retention_days).
    CREATE TABLE IF NOT EXISTS mail_outbound_diagnostic_runs (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        ran_at          INTEGER NOT NULL,
        results_json    TEXT NOT NULL,
        ran_by_actor_id BLOB NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_mail_outbound_diagnostic_runs_time
        ON mail_outbound_diagnostic_runs(ran_at);

    -- Fresh-IP outbound warm-up state per docs/goal/behavior/
    -- mail-deliverability.md § Fresh-IP warm-up → § State model. ONE
    -- deployment-wide row (CHECK(id=1), no actor_id — the warm-up is the
    -- deployment's shared outbound-IP reputation), upserted at every
    -- authenticated submission and on the admin reset.
    --   first_outbound_at  — epoch-secs of the first ever outbound mail; NULL
    --                        until then. current_day derives from it + NOW.
    --   current_day        — cached day-since-first-outbound (1-based); always
    --                        recomputed on read so it never goes stale.
    --   mails_sent_today   — running counter, lazily reset to 0 when
    --                        counter_epoch_day rolls over (00:00 UTC daily).
    --   mails_sent_total   — lifetime counter (admin interest; preserved
    --                        across a manual reset).
    --   counter_epoch_day  — IMPL-ONLY: the UTC epoch-day (NOW/86400) that
    --                        mails_sent_today/current_day were last computed
    --                        for; the lazy-daily-reset marker (the doc's
    --                        5-column § State model + this reset marker).
    --   deferred_total     — IMPL-ONLY: lifetime `deferred_warmup` count (the
    --                        mail_outbound_warmup_deferred_total metric).
    --   last_reset_at      — epoch-secs of the last admin manual reset
    --                        (e.g. after a VPS IP change); NULL until then.
    CREATE TABLE IF NOT EXISTS mail_outbound_warmup_state (
        id                INTEGER PRIMARY KEY CHECK (id = 1),
        first_outbound_at INTEGER,
        current_day       INTEGER NOT NULL DEFAULT 1,
        mails_sent_today  INTEGER NOT NULL DEFAULT 0,
        mails_sent_total  INTEGER NOT NULL DEFAULT 0,
        counter_epoch_day INTEGER NOT NULL DEFAULT 0,
        deferred_total    INTEGER NOT NULL DEFAULT 0,
        last_reset_at     INTEGER
    );

    -- The mail health readout's two heartbeat stamps (mail-deliverability.md
    -- § The mail health readout → Two heartbeat stamps). ONE deployment-wide
    -- row (id = 1) — the mail_outbound_warmup_state idiom. No actor and no
    -- domain and no message identity: facts for the admin readout only and
    -- never a state input.
    --   last_outbound_delivered_at — epoch-secs of the latest
    --                                mark_outbound_delivered; NULL = never.
    --   last_inbound_accepted_at   — epoch-secs of the latest
    --                                ingest_inbound_mail; NULL = never.
    CREATE TABLE IF NOT EXISTS mail_heartbeat_state (
        id                         INTEGER PRIMARY KEY CHECK (id = 1),
        last_outbound_delivered_at INTEGER,
        last_inbound_accepted_at   INTEGER
    );
";

pub(super) const MIGRATIONS_SYNC: &str = "
    CREATE TABLE IF NOT EXISTS sync_devices (
        actor_id    BLOB NOT NULL,
        device_id   BLOB NOT NULL,
        label       TEXT NOT NULL DEFAULT '',
        registered_at INTEGER NOT NULL,
        last_seen   INTEGER NOT NULL,
        -- The guardian-enrolled-device marker (family-safety.md § Full
        -- visibility). Set/cleared only by the ward's guardian via
        -- `fauna.family.device.mark`; makes an otherwise-indistinguishable
        -- enrolled device refusable to the ward's own `devices.delete` and
        -- revocable at graduation. Inert on an unsupervised account — the
        -- refusal predicate is `marked AND currently supervised`, so a stale
        -- mark left by a crash never strands an undeletable row.
        guardian_marked INTEGER NOT NULL DEFAULT 0,
        -- The device's RenewBearer renewal grant (sync-agent.md § Credential
        -- model): auth_device_key = the renewal device public key (32 bytes),
        -- auth_grant = the verified embed-as-bytes DeviceAuthorization wire
        -- blob, re-verified at every fauna.auth.device_handshake mint. NULL
        -- until fauna.sync.device_grant.register stores one; the row's
        -- deletion (fauna.sync.devices.delete) is the revocation.
        auth_device_key BLOB,
        auth_grant      BLOB,
        -- SEALED LABEL over `label` -- random-nonce mode because a user can
        -- rename a device under a fixed salt. See
        -- `reconcile_path_sealing_companions`.
        label_sealed BLOB,
        capabilities TEXT NOT NULL DEFAULT 'read,write',
        -- Per-device p2p participation (p2p.md § Per-device participation).
        -- p2p_participation = the device's OWN last report ('on' | 'off';
        -- NULL = never reported) -- informative only; the device-local row
        -- is the authority. p2p_off_requested = a pending brake another of
        -- the account's devices raised; the device folds it at its next pass
        -- and clears it with an 'off' report. Nothing here can turn a
        -- listener ON. Both additive (reconcile_added_columns).
        p2p_participation TEXT,
        p2p_off_requested INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (actor_id, device_id)
    );

    -- Revocation memory for renewal device grants (sync-agent.md
    -- § Credential model). A device deletion tombstones the row's
    -- auth_device_key here so replaying the same root-signed grant can never
    -- re-attach it and the tombstoned key can never mint again. Keyed on the
    -- renewal device PUBLIC key -- every legitimate provision mints a fresh
    -- keypair (build_renewal_grant) so re-enrollment is never blocked. No
    -- column comments inside the list: the additive-column reconciler's
    -- DROP COLUMN probe misparses comma-bearing comments.
    CREATE TABLE IF NOT EXISTS revoked_device_grants (
        actor_id        BLOB NOT NULL,
        auth_device_key BLOB NOT NULL,
        revoked_at      INTEGER NOT NULL,
        PRIMARY KEY (actor_id, auth_device_key)
    );

    -- The relay's admission question (p2p.md § The relay) arrives with a
    -- device key and no actor, and anyone on the internet can make the relay
    -- ask it: answer from an index, never a scan under the one connection lock.
    CREATE INDEX IF NOT EXISTS idx_sync_devices_auth_device_key
        ON sync_devices(auth_device_key) WHERE auth_device_key IS NOT NULL;

    -- The DeviceAuthorization each delegated change-record signer verified
    -- under at ingest (writer-signed-change-records.md § Writer-signed change
    -- records -- the cert travels by reference within a home nest). One row
    -- per (actor -- device key -- the actor the cert names) and the latest
    -- verified cert wins within one: a re-ceremony certifies the same key
    -- with more capabilities. cert_actor_id is written at ingest from the
    -- cert that verified and never moves: actor_id follows the rows a
    -- succession moves while the cert still names the predecessor -- so a
    -- successor certifying the same device key stores its own cert beside
    -- the moved one instead of replacing it (ruling (8)(i)). The list
    -- replies project every cert at a row's (actor -- signer key) as the
    -- signer_certs side table. Kept past a device deletion on purpose: a row
    -- authored while authorized stays verifiable and the tombstone refuses
    -- every NEW record at ingest. Also the home of a federated writer's
    -- inline cert (that writer has no sync_devices row here). No column
    -- comments inside the list: the additive-column reconciler's DROP COLUMN
    -- probe misparses comma-bearing comments.
    CREATE TABLE IF NOT EXISTS sync_signer_certs (
        actor_id      BLOB NOT NULL,
        device_key    BLOB NOT NULL,
        cert_actor_id BLOB NOT NULL,
        cert          BLOB NOT NULL,
        updated_at    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, device_key, cert_actor_id)
    );

    -- The class-2 feed's retention gate (account-data-taxonomy.md
    -- § Fleet-scope reclamation, clause 1). Per (scope folder, walker), the
    -- highest held_through_seq that walker has claimed on the feed -- the
    -- serve-order watermark it banked. fauna.account.state.retire compacts a
    -- live row only once every marked walker whose key holds a live,
    -- non-tombstoned grant on the actor has walked past it. A walker is a
    -- device writer id (the store principal's key), keyed like the grant.
    CREATE TABLE IF NOT EXISTS state_walk_marks (
        folder_id       INTEGER NOT NULL,
        walker          BLOB NOT NULL,
        seq             INTEGER NOT NULL,
        PRIMARY KEY (folder_id, walker)
    );

    CREATE TABLE IF NOT EXISTS sync_changes (
        seq             INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id        BLOB NOT NULL,
        path_hash       BLOB NOT NULL,
        manifest_hash   BLOB,
        size_bytes      INTEGER NOT NULL DEFAULT 0,
        change_type     TEXT NOT NULL,
        created_at      INTEGER NOT NULL,
        -- M2 content-key generation these chunks were sealed under (shared file
        -- sets); NULL for owner-only sets and deletes.
        -- Stored opaque + echoed back on list so the reader selects
        -- key_for(version).
        content_key_version INTEGER,
        -- Hex thumbnail-blob hash the uploader recorded for this file (the
        -- UploadSidecar.thumbnail_hash `?thumb=1` pointer). NULL for deletes --
        -- non-media files -- and changes recorded before a producer supplies
        -- one. Stored opaque + surfaced via fauna.media.list (media.md §
        -- State & data shape).
        thumbnail_hash TEXT,
        -- Epoch-millis stamp set by fauna.sync.changes.supersede when a strictly
        -- newer live manifest row for the same (folder_id / path_hash) has
        -- been verified retrievable+decryptable by the owner (M2 pre-bind
        -- re-seal reclaim -- mls-group-key-material.md § M2 bullet B). NULL =
        -- live. A superseded row's manifest stops pinning its chunks against
        -- GC and is dropped from the changes.list feed; the row itself is
        -- NEVER deleted (append-only history -- backup_custody-tombstone
        -- precedent).
        superseded_at INTEGER,
        -- SEALED LABEL over `path` -- convergent under the row's own
        -- `path_hash`. See `reconcile_path_sealing_companions`.
        path_sealed BLOB,
        -- Causal watermark (ruled 2026-08-02 -- file-sync.md § Conflicts): the
        -- set seq
        -- through which the RECORDING writer had incorporated every row when
        -- the content was produced — client-stamped -- stored opaque --
        -- echoed on list + the real-time forward. NULL = unknown (the writer
        -- stamped none); readers then use the local==base heuristic.
        derived_through INTEGER,
        -- 1 when the row is a pure RESOLUTION (auto-resolve merge result /
        -- resolution-winner propagation) -- NULL/0 for a fresh user edit —
        -- the receiver-side stale-resolution skip keys on it (same ruling).
        is_resolution INTEGER,
        -- 1 when the row is the resolved report's LOSER-RETENTION vehicle
        -- (loser-row ruling 2026-08-05 -- conflicts.md § Concurrent resolution):
        -- set only by report_conflict's transaction, never client-recorded.
        -- Receivers account-and-skip it; the row keeps GC-pinning and
        -- candidate-listing the losing version.
        is_retention INTEGER,
        -- W2.3 -- the generalized account-data feed (account-sync-plane.md
        -- § Feeds and cursors). The explicit item-class discriminator —
        -- 'chunk-manifest' | 'direct-blob' | 'record-cid' | 'state-entry'
        -- (fauna_protocol::account_state::ItemClass). NULL on every ordinary
        -- file row: infer-from-shape routing IS the NULL semantics -- which is
        -- what lets the generalized row EXTEND this one instead of replacing it.
        item_class TEXT,
        -- The 32-byte authoring WRITER id (a device principal's public key) for
        -- a row the nest relays from a device writer; NULL when the nest itself
        -- is the writer. Multi-master per R6 (account-data-plane.md § The ratified decisions): a
        -- scope carries one high-water per writer -- never one shared counter.
        origin_writer BLOB,
        -- That writer's OWN log sequence for this row — the coordinate a reader
        -- accounts into `origin_writer`'s frontier slot. NULL with a NULL
        -- origin_writer: the nest-writer slot accounts `seq` -- which is what
        -- makes today's scalar `since` cursor the degenerate frontier
        -- {nest: since}.
        origin_seq INTEGER,
        -- The sealed class-2 entry served INLINE on the feed row (the frozen
        -- T14 envelope: [form version][random nonce][ChaCha20-Poly1305] --
        -- AAD-bound to its own {form_version / writer_id / writer_seq / scope
        -- / item_key}). Stored and echoed byte-for-byte — the nest holds no
        -- key that opens one -- and the AAD is what makes that safe: no relay
        -- can splice one entry's ciphertext under another's coordinates. NULL
        -- on every non-'state-entry' row.
        entry_sealed BLOB,
        -- WRITER SIGNATURE (mls-group-key-material.md § M2 -> Multi-writer ->
        -- Writer-signed change records). The writer's 64-byte Ed25519
        -- signature over the row's SignedChange statement and the 32-byte key
        -- it verifies under (the device principal key -- or the actor id
        -- itself for a direct signature). Verified at ingest and echoed
        -- verbatim on every projection. NULL on the exempt row classes.
        signature BLOB,
        signer_key BLOB,
        -- VERSION-RETENTION prune pipeline marks (file-versions.md
        -- § Retention ruling 3 — the § 7 layers state-for-state on the version
        -- plane). All three NULL on every row outside the pipeline — the
        -- healthy steady state.
        --
        -- prune_pending: 1 = targeted by an un-executed VersionBulkPrune
        -- pending action (the 7-day cancellable window -- Layer 2). The row
        -- stays listable and restorable; cancel clears the mark. Also what
        -- makes the evaluator idempotent: marked rows leave its candidate
        -- population.
        prune_pending INTEGER,
        -- pruned_at: epoch-millis stamp when the executor SOFT-pruned the row
        -- (Layer 3). The row leaves the default versions.list projection but
        -- stays GC-pinned (superseded_at is still NULL) and recoverable via
        -- fauna.files.versions.undelete + the include_pruned recovery browse.
        pruned_at INTEGER,
        -- purge_after: epoch-seconds deadline of the 30-day recovery window.
        -- Only past it does the GC-cycle purge step stamp superseded_at, after
        -- which the existing pin predicate releases the chunks one GC grace
        -- later. Set/cleared together with pruned_at.
        purge_after INTEGER,
        -- charged: 1 = this row holds its bytes' quota charge. The charge
        -- follows the (path, manifest) pair across the path's LISTABLE rows
        -- (manifest-bearing, not superseded, not soft-pruned), held by exactly
        -- one of them (writer-signed-change-records.md ruling (11)(g)): a
        -- record over the manifest the path already holds takes the charge and
        -- clears the flag on the row that held it, and a release hands the
        -- flag to a surviving same-manifest row before it credits. Read only
        -- on a listable row -- undelete decides a rejoining row's flag afresh
        -- -- so the default 1 is right for every row that predates the column.
        charged INTEGER NOT NULL DEFAULT 1,
        folder_id INTEGER,
        device_id BLOB,
        path TEXT
        -- NB (multi-writer Phase 1): `actor_id` above is the RECORDING connection
        -- actor, nest-stamped at insert — with writer members it is no longer
        -- always the set owner. It doubles as the wire `author_actor_id`
        -- attribution (file-sync.md § Multi-writer shared sets); every shared-set
        -- read path is folder_id-keyed, so a writer-recorded row breaks no
        -- consumer.
    );
    CREATE INDEX IF NOT EXISTS idx_sync_changes_actor
        ON sync_changes(actor_id, seq);
    -- The file-versions projection reads sync_changes by path_hash
    -- (file-sync.md § File Versions; list_file_versions_in_sets /
    -- get_file_version_by_seq).
    CREATE INDEX IF NOT EXISTS idx_sync_changes_path
        ON sync_changes(path_hash, seq);
    -- The class-2 feed's per-(scope, item_key, writer) head lookup, done on
    -- every put: the CAS probe, the per-writer monotonicity refusal, and the
    -- collapse of that writer's predecessors. The (path_hash, seq) index
    -- cannot answer it without scanning the other writers' rows for the item.
    CREATE INDEX IF NOT EXISTS idx_sync_changes_state_entry
        ON sync_changes(folder_id, path_hash, origin_writer, seq);
    -- (scope, writer, seq) is a journal row's identity
    -- (account-data-plane.md § Store logical schema): the class-2 write path
    -- refuses a coordinate the scope already holds on any item
    -- (`account_state::StateEntryError::SeqReused`), and this UNIQUE index is
    -- that refusal's structural twin, so a reuse is unrepresentable rather
    -- than merely refused at one write path. It also answers the refusal's own
    -- per-writer probe. Only a class-2 put sets `origin_writer`, and SQLite
    -- holds NULLs distinct in a UNIQUE index, so every ordinary file row
    -- (`origin_writer IS NULL`) is untouched. Should another item class ever
    -- set the column, either the probe's `item_class` filter goes or this
    -- index gains the column, or the two stop agreeing about what is refusable.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_changes_writer_coordinate_unique
        ON sync_changes(folder_id, origin_writer, origin_seq);
";

/// Backup-custody projection (`docs/goal/architecture/message-segment-store.md`
/// § Cross-location backup protocol → *GC-safety — custodian-authoritative
/// custody*). On a **destination** nest, a cross-location backup is a file-sync
/// of the owner's reserved folder, which the destination provisions as a
/// custody copy (`folders.custody_copy`): the coordinator records
/// each uploaded segment + the `manifest.<kind>` mirror as a path via the
/// standard `fauna.sync.changes.record`, and the record handler routes
/// custody copies here — the **latest non-deleted manifest per
/// `(folder, path)`** — instead of into the append-only `sync_changes`
/// device-sync feed. GC walks this set so a destination never deletes a live
/// backup blob; reclamation fires on supersede (new manifest, same path),
/// compacted-out (`delete` → `manifest_hash = NULL` tombstone) or destination
/// removal (the whole set's rows dropped). Routing backups *here* (not into
/// `sync_changes`) makes the device-pull exclusion automatic (nothing to
/// filter) and gives latest-per-path reclamation by construction (UPSERT, no
/// GROUP-BY over append-only history). `uploader_actor` is the authenticated
/// bearer, never taken from the wire.
///
/// `size_bytes` is the uploader-asserted logical size of the current manifest —
/// the per-actor storage-quota accounting field (the custody twin of
/// `sync_changes.size_bytes`). It lets the quota path compute a supersede delta
/// without re-walking the (here opaque, `BackupKey`-sealed) manifest blob: the
/// prior row's `size_bytes` is the bytes to credit back when a path is
/// superseded or tombstoned.
pub(super) const MIGRATIONS_BACKUP_CUSTODY: &str = "
    CREATE TABLE IF NOT EXISTS backup_custody (
        uploader_actor BLOB    NOT NULL,
        folder_id    INTEGER NOT NULL,
        path_hash      BLOB    NOT NULL,
        manifest_hash  BLOB,
        size_bytes     INTEGER NOT NULL DEFAULT 0,
        updated_at     INTEGER NOT NULL,
        -- The recorder's plaintext path — populated on every record so a
        -- backup-type set's files can surface in `fauna.media.list`.
        path           TEXT,
        -- The recorder's hex thumbnail-blob hash (the UploadSidecar.thumbnail_hash
        -- `?thumb=1` pointer), so a backup-type set's files surface their
        -- thumbnail in `fauna.media.list` just like ordinary files. NULL until a
        -- producer supplies one.
        thumbnail_hash TEXT,
        -- SEALED LABEL over `path` -- convergent under the row's own
        -- `path_hash`. See `reconcile_path_sealing_companions`.
        path_sealed BLOB,
        PRIMARY KEY (folder_id, path_hash)
    );
";

/// Retained **superseded** custody generations — the destination-side custody
/// grace window (`docs/goal/architecture/message-segment-store.md`
/// § Cross-location backup protocol; T is
/// `crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS`).
///
/// A custody writer's supersede power is delete power: `backup_custody` is
/// latest-per-path (PK `(folder_id, path_hash)`), so a supersede UPSERTs the
/// prior `manifest_hash` **away** and a tombstone NULLs it — after which the GC
/// reference walk can never see that generation again and its exclusive chunks
/// reclaim on the next cycle. A rogue (or compromised) **source nest**, which
/// holds a user-minted nest-writer grant, could therefore wipe a user's whole
/// backup by re-recording every path with junk. This table is the mitigation:
/// the prior generation is *moved here* rather than forgotten, stays in the GC
/// reference set (so its chunks survive), stays charged to the owner's quota,
/// and is client-restorable — until it ages past T and is reclaimed for real.
///
/// **Append-only per (path, content).** The PK is
/// `(folder_id, path_hash, manifest_hash)`, so a path that flaps between two
/// contents keeps exactly two rows rather than growing without bound; a repeat
/// supersede of a content already retained refreshes `superseded_at` (the
/// over-retain direction, never under-retain) and does **not** re-charge quota.
///
/// **Scope: reserved destination sets only.** Retention is gated on
/// `is_reserved_custody_copy` — the foreign-written custody a rogue source
/// could wipe. An *ordinary* folder (the Photo Library preset included)
/// records to the `sync_changes` head feed since the
/// phase 3 head unification (2026-08-17) and protects superseded versions with
/// real snapshot pins plus client-verified supersede marking
/// (`backup-restore.md` § *Backup folders and snapshots*); retaining
/// generations there too would double-protect and double-charge the same bytes.
/// A reserved destination set is never snapshotted, which is exactly why it
/// needs this instead.
pub(super) const MIGRATIONS_BACKUP_CUSTODY_GENERATIONS: &str = "
    CREATE TABLE IF NOT EXISTS backup_custody_generations (
        uploader_actor BLOB    NOT NULL,
        folder_id    INTEGER NOT NULL,
        path_hash      BLOB    NOT NULL,
        manifest_hash  BLOB    NOT NULL,
        -- The bytes this generation holds = the amount that was CHARGED for it
        -- (destination-derived from the held manifest + chunks since
        -- 2026-07-29, review (xxxi-c) -- never the writer's declaration).
        -- Stays charged to `users.storage_bytes_used` for the whole grace
        -- window: quota-charging retained generations IS the supersede rate
        -- cap (ratified 2026-07-23, 'reclaim rate-cap = not-a-parameter'), so
        -- a supersede storm walks the owner's own quota boundary instead of
        -- needing a bespoke limiter.
        size_bytes     INTEGER NOT NULL,
        -- Epoch seconds at which this generation stopped being the live one —
        -- the start of its T clock. Refreshed if the same content is superseded
        -- again (over-retain, never under-retain).
        superseded_at  INTEGER NOT NULL,
        -- The recorder's plaintext path, carried over from the custody record so
        -- a generation listing is renderable without a reverse path_hash lookup
        -- (`path_hash` is one-way). It rests only where the custody row's own
        -- plaintext does -- the exempt classes `rest_path` admits; every other
        -- record arrives with it NULL, and the live custody row keeps the seal.
        path           TEXT,
        PRIMARY KEY (folder_id, path_hash, manifest_hash)
    );
    CREATE INDEX IF NOT EXISTS idx_backup_custody_gen_expiry
        ON backup_custody_generations(superseded_at);
    CREATE INDEX IF NOT EXISTS idx_backup_custody_gen_actor
        ON backup_custody_generations(uploader_actor);
";

pub(super) const MIGRATIONS_KEY_PACKAGES: &str = "
    CREATE TABLE IF NOT EXISTS key_packages (
        id TEXT PRIMARY KEY,
        actor_id BLOB NOT NULL,
        key_package_data BLOB NOT NULL,
        published_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        last_resort INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_kp_actor ON key_packages(actor_id);
";

pub(super) const MIGRATIONS_CONTACTS: &str = "
    CREATE TABLE IF NOT EXISTS contacts (
        actor_id    BLOB NOT NULL,
        peer_id     BLOB NOT NULL,
        status      TEXT NOT NULL,
        accepted_at INTEGER,
        created_at  INTEGER NOT NULL,
        PRIMARY KEY (actor_id, peer_id)
    );

    CREATE TABLE IF NOT EXISTS knocks (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id    BLOB NOT NULL,
        sender_id   BLOB NOT NULL,
        sender_node BLOB NOT NULL,
        summary     TEXT NOT NULL,
        payload     BLOB NOT NULL,
        created_at  INTEGER NOT NULL,
        delivered   INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_knocks_recipient
        ON knocks(actor_id, delivered);
";

pub(super) const MIGRATIONS_INBOX_MODE: &str = "
    CREATE TABLE IF NOT EXISTS inbox_modes (
        actor_id BLOB PRIMARY KEY,
        mode TEXT NOT NULL DEFAULT 'allow_knock'
    );
";

/// The **room plane** — the floor roster and the room record it belongs to
/// (`../../docs/goal/behavior/conversation-rooms.md` § The room, § The floor
/// roster).
///
/// **Minted fresh rather than reshaped from `groups` + `group_members`**, the
/// call § The group plane's fate step 2 leaves to the build, taken on that
/// section's own tie-breaker: no shipped app ever wrote a row to either old
/// table, so a fresh mint costs nothing a user could lose. Two things decided
/// it beyond the tie-breaker. The floor roster's principal is not the old
/// table's actor — it is a *principal* of three kinds (user, nest, bridge),
/// and a community room's home nest is a member row that `group_members`'
/// `actor_id`-shaped world has no vocabulary for. And the old table deletes a
/// departed member's row where this one absorbs the removal as history, which
/// the succession axis requires (§ The home nest → *The succession axis*).
/// Reshaping in place would have meant a column rename, a CHECK rewrite and a
/// row-semantics change on a live authorization gate, to reach a table shaped
/// like this one anyway.
///
/// The two old tables were retired at step 3 and are in no genesis.
pub(super) const MIGRATIONS_ROOMS: &str = "
    CREATE TABLE IF NOT EXISTS rooms (
        room_id BLOB PRIMARY KEY,
        class TEXT NOT NULL CHECK(class IN ('end_to_end', 'community', 'transport_only')),
        home_node_url TEXT NOT NULL DEFAULT '',
        owner_id BLOB,
        policy_version INTEGER,
        policy_blob BLOB,
        -- The BIRTH RECORD's salt, set only by the create ceremony
        -- (`fauna.conversations.room.create`). Its presence is what says
        -- this room's membership authority is the FLOOR: the nest's own
        -- ceremonies write the roster, and the member-reported mirror door
        -- (`room.roster_report`) is refused. A room minted by a report
        -- leaves it NULL and keeps the mirror semantics it was born with.
        --
        -- Stored rather than discarded after the id check so the birth
        -- record stays re-verifiable: room_id == derive_room_id(owner_id,
        -- birth_salt) is checkable at any later time by anyone reading the
        -- row, which is what makes it a RECORD and not just an admission
        -- decision taken once.
        birth_salt BLOB,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        -- The room's SIGNED LABELER SET
        -- (`fauna_mls::room_policy::SignedRoomLabelers` -- set through
        -- `fauna.conversations.room.set_labelers`): which transparent labelers
        -- this nest applies to a community room's messages
        -- (`../../docs/goal/behavior/conversation-rooms.md` § The three
        -- classes -> *What the home nest does with its read* -> purpose 2).
        -- Stored exactly like the policy -- the bytes its author signed and
        -- the version the ratchet compares against -- and a record of its own
        -- rather than a policy field. So a policy change can never reset it.
        -- NULL version = the room never named one (read as 0). The room's
        -- choice and not a derived view: a revoke leaves it.
        -- Comma-free on purpose (the module doc's DROP COLUMN trap).
        labelers_version INTEGER,
        labelers_blob BLOB,
        -- The room log's position of the commit whose member report the
        -- floor holds -- what ORDERS an end-to-end room's reports
        -- (`../../docs/goal/behavior/conversation-rooms.md` § The floor
        -- roster). A report at or below it is not applied -- so one that
        -- arrives after a later commit's report rolls nothing back. NULL until
        -- the first report naming a position and on every ceremony-born
        -- room. Keep this comment free of commas: it is the table's last
        -- column (see the module doc).
        roster_commit_seq INTEGER
    );

    -- Every signed policy version a room has held -- the superseded
    -- ones beside the current one -- served by number so a member can judge a
    -- floor delete record against the policy OF THE VERSION IT NAMES
    -- (`../../docs/goal/behavior/conversation-rooms.md` § Roles and
    -- authorization -> *Delete any message -- the mechanism* -> *Community
    -- rooms*). `rooms.policy_blob` stays the floor's one current answer; this
    -- is its history and every policy writer appends here in the transaction
    -- that moves it. User-irrecoverable: a signed version cannot be re-minted
    -- by anyone but its then-owner. Never dropped.
    CREATE TABLE IF NOT EXISTS room_policy_versions (
        room_id BLOB NOT NULL,
        version INTEGER NOT NULL,
        policy_blob BLOB NOT NULL,
        stored_at INTEGER NOT NULL,
        PRIMARY KEY (room_id, version)
    );

    CREATE TABLE IF NOT EXISTS room_members (
        room_id BLOB NOT NULL,
        principal_id BLOB NOT NULL,
        principal_kind TEXT NOT NULL CHECK(principal_kind IN ('user', 'nest', 'bridge')),
        role TEXT CHECK(role IS NULL OR role IN ('owner', 'admin', 'member')),
        invited_by BLOB,
        home_node_url TEXT NOT NULL DEFAULT '',
        joined_at INTEGER NOT NULL,
        reported_at INTEGER NOT NULL,
        removed_at INTEGER,
        -- The recipient-set scheme's two per-member facts
        -- (`account-data-taxonomy.md` § The recipient-set scheme;
        -- `conversation-rooms.md` § The three classes → *Community*).
        --
        -- `entry_id` is this seating's 32-byte roster-entry id, derived from
        -- (room, principal, join stamp) at the moment of admission, and it is
        -- half the AAD of every wrap sealed to this member. Deriving it from
        -- the SEATING STAMP rather than the principal alone is what makes the
        -- scheme's rule true here -- re-admission is a fresh entry id, so
        -- add-wins resurrection is unrepresentable: a removed member who is
        -- later re-invited gets a new slot, so no wrap minted for its old
        -- seat can be replayed at it.
        --
        -- `reception_pubkey` is the member's group-reception X-Wing public
        -- half — the wrap target, supplied by the member itself at the act
        -- that seats it (the create ceremony, or `room.accept_invite`). The
        -- home nest's own row carries its `room_read_pubkey`
        -- (`room_read_key.rs`), which is what makes it a reader recipient
        -- rather than a parallel kind of member.
        --
        -- Both nullable: a member seated without them simply has no wrap
        -- target until a member top-up heals it — the scheme's own
        -- self-healing path rather than a special case.
        entry_id BLOB,
        reception_pubkey BLOB,
        PRIMARY KEY (room_id, principal_id),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    CREATE INDEX IF NOT EXISTS idx_room_members_principal
        ON room_members(principal_id) WHERE removed_at IS NULL;

    -- The room's generation DAG — the recipient-set scheme's keying plane
    -- for a community room (`conversation-rooms.md` § The three classes →
    -- *Community*, reason 1: 'The scheme wraps a random room generation key
    -- to each member's reception key on every membership change').
    --
    -- The nest stores, admits and serves; it NEVER mints (§ Don't do these).
    -- `mint_blob` is the owner-or-admin-signed
    -- `GroupGenerationMintRecord` whole, kept rather than reduced to its
    -- fields for the reason `room_invites.signed_invite` is: it is what a
    -- member verifies, and what a cross-nest relay leg will carry unchanged.
    -- `key_commitment` is lifted out of it because every wrap open checks the
    -- recovered key against it, and `parent_id` because tip resolution reads
    -- it on every publish.
    CREATE TABLE IF NOT EXISTS room_generations (
        room_id BLOB NOT NULL,
        generation_id BLOB NOT NULL,
        parent_id BLOB,
        key_commitment BLOB NOT NULL,
        minted_by BLOB NOT NULL,
        mint_blob BLOB NOT NULL,
        minted_at_ms INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (room_id, generation_id),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    CREATE INDEX IF NOT EXISTS idx_room_generations_room
        ON room_generations(room_id, minted_at_ms);

    -- One X-Wing wrap per (generation, roster entry). Served to the holder of
    -- that entry ALONE (`fauna.conversations.room.generations`), so this
    -- table is never an enumeration surface for a room's key material; the
    -- home nest opens its own row here and nothing else.
    --
    -- A wrap naming an entry that has since been removed is inert rather than
    -- deleted — the scheme's own rule ('a wrap naming a Removed member is
    -- inert, never served against, dropped at the next mint') — because the
    -- removed member keeps what it could already open (the universal rule-6
    -- bound) and severance is wrap TARGETING on future mints, not deletion of
    -- past ones.
    CREATE TABLE IF NOT EXISTS room_generation_wraps (
        room_id BLOB NOT NULL,
        generation_id BLOB NOT NULL,
        entry_id BLOB NOT NULL,
        wrap BLOB NOT NULL,
        PRIMARY KEY (room_id, generation_id, entry_id),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    -- Pending and accepted room invitations. An invite does NOT seat a
    -- member: `conversation-rooms.md` § Join rules and invites makes
    -- acceptance the seating act ('a community room's home nest writes the
    -- roster row on acceptance'), so a room's floor never names someone who
    -- has not agreed to be there. That is the one place the room plane
    -- deliberately diverges from the dormant group plane, whose
    -- `group.invite` adds the member outright.
    --
    -- `signed_invite` is the inviter's signed act, kept whole rather than
    -- reduced to its fields: it is what the invitee verifies, and what a
    -- cross-nest leg will carry unchanged when invite delivery crosses a
    -- nest boundary.
    CREATE TABLE IF NOT EXISTS room_invites (
        room_id BLOB NOT NULL,
        invitee_id BLOB NOT NULL,
        inviter_id BLOB NOT NULL,
        -- 'admin' or 'member' -- never 'owner': a room has exactly one and
        -- it moves by transfer, not by invitation.
        role TEXT NOT NULL CHECK(role IN ('admin', 'member')),
        invitee_node_url TEXT NOT NULL DEFAULT '',
        signed_invite BLOB NOT NULL,
        invited_at INTEGER NOT NULL,
        accepted_at INTEGER,
        -- The `content_links.id` of this invitation's current un-acked
        -- inbox envelope, or NULL when none stands (never delivered, or
        -- already consumed by a re-invite or an accept). One row, one live
        -- envelope at a time -- see `record_room_invite_and_deliver` and
        -- `accept_room_invite` in `db/rooms.rs`.
        inbox_link_id INTEGER,
        -- The VERIFIED nest identity a cross-nest invitation was delivered to
        -- (resolved from the federation dial, never inviter-declared), or NULL
        -- for an invitee homed here. The gate the relayed accept runs behind:
        -- only that nest may seat this invitee -- see
        -- `record_room_invite_for_foreign_delivery` and
        -- `room_invite_home_binding` in `db/rooms.rs` (schema 81).
        invitee_nest_id BLOB,
        PRIMARY KEY (room_id, invitee_id),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    CREATE INDEX IF NOT EXISTS idx_room_invites_invitee
        ON room_invites(invitee_id) WHERE accepted_at IS NULL;

    -- The nest's ROOM-READ keypair — one X-Wing reception keypair per
    -- deployment, the wrap target that makes this nest a readable member of
    -- the community rooms it homes
    -- (`../../docs/goal/architecture/key-material-hierarchy.md`
    -- § Audience: deployment infrastructure -> *Room-read keypair*).
    --
    -- `ikm_wrapped` is the raw 32-byte X-Wing input key material sealed under
    -- nest_kek::ROOM_READ_CONTEXT, registered in nest_kek::SATELLITES so a
    -- deployment-seed rotation re-keys it rather than stranding it. The
    -- keypair itself is deterministic from the ikm
    -- (`fauna_core::group_generation::GroupReceptionKeyRecord::keypair`'s two
    -- frozen contexts), so nothing key-sized rests beyond the seed -- the
    -- member-side kind's own discipline, reused rather than re-invented.
    --
    -- ⚠ WHY SEALED-RANDOM AND NOT SEED-DERIVED. The goal doc says the key is
    -- minted from the deployment seed at first boot, which reads either way;
    -- the satellite shape is the one that survives a seed rotation. A keypair
    -- derived directly from the deployment seed would CHANGE identity when
    -- the seed rotates, silently making every community room's existing wrap
    -- to this nest unopenable, with no ceremony that could fix it -- the
    -- rows are wraps other people minted to a public key that no longer
    -- exists. Sealed-random has exactly the security property the goal doc
    -- claims -- DB-only exfiltration of the wraps opens nothing without the
    -- deployment seed -- and keeps the pubkey stable across rotation, because
    -- `reencrypt_satellites` rewraps the ciphertext without touching the
    -- keypair. The goal doc records this resolution.
    --
    -- One row, `id = 0`: a deployment has one room-read position. It does
    -- not rotate here -- the nest reads, it never mints or rotates; a room
    -- revokes this nest by rotating its own generation and leaving this key
    -- out of the recipient set, which is the members' act, not the nest's.
    CREATE TABLE IF NOT EXISTS nest_room_read_key (
        id           INTEGER PRIMARY KEY CHECK(id = 0),
        ikm_wrapped  BLOB NOT NULL,
        public_key   BLOB NOT NULL,
        created_at   INTEGER NOT NULL
    );

    -- The home nest's derived view of a community room's log, as the index
    -- that CAN BE READ BACK (`../../docs/goal/behavior/conversation-rooms.md`
    -- § The three classes -> *What the home nest does with its read*).
    --
    -- The searchable text itself lives in the shared FTS corpus under this
    -- room's own class (`CacheDb::room_view_schema`); what this table adds is
    -- the only thing that corpus cannot answer: WHICH MESSAGE a hit is. An
    -- FTS row is keyed by `content_id_for_document(schema, seq)`, a one-way
    -- hash, so without a stored map a hit resolves to nothing a member could
    -- fetch -- which is the state the search index sat in from the day it
    -- was built until this door opened.
    --
    -- It is a DERIVED VIEW like the FTS rows beside it, under the same
    -- per-room class, and `purge_room_derived_views` deletes both in the one
    -- act the materialization grant's revoke is
    -- (`../../docs/goal/principles.md` § The user always controls their
    -- data). Nothing here is authoritative: every row is rebuildable from the
    -- sealed log by a nest that holds a wrap, and a nest that does not hold
    -- one must have none.
    --
    -- No plaintext. The seq is a position in the room's own log and the doc
    -- key is a hash -- the message's bytes stay sealed in the segment store,
    -- and the text stays in the FTS corpus the door never serves.
    -- `generation_id` is WHICH generation the message was sealed under when
    -- the reception pass indexed it -- the tip the nest held then.
    -- It is what bounds the search door to positions the caller could open: a
    -- hit is served only to a member holding a wrap for this generation
    -- (`../../docs/goal/behavior/community-rooms.md` § The three classes ->
    -- *The door answers where, never what*).
    CREATE TABLE IF NOT EXISTS room_message_views (
        room_id  BLOB NOT NULL,
        seq      INTEGER NOT NULL,
        doc_key  BLOB NOT NULL,
        generation_id BLOB NOT NULL,
        PRIMARY KEY (room_id, seq),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    -- The lookup the search door runs: hits come back as doc keys.
    CREATE INDEX IF NOT EXISTS idx_room_message_views_doc
        ON room_message_views(room_id, doc_key);

    -- The POST twin of `room_message_views`: which
    -- room-restricted posts addressed to this room the nest indexed at
    -- reception, keyed by post id (`../../docs/goal/ui/feed.md` § Encryption
    -- at rest -> *Room-restricted -- the ruling*, ruling 7;
    -- `conversation-rooms.md` § The three classes -> *What the home nest does
    -- with its read*, purpose 3). A room post is its author's post and never
    -- enters the room's log, so it has no seq to key on -- the post id is
    -- what a member reads it back by.
    --
    -- Same class as its twin in every respect that matters: a DERIVED VIEW,
    -- rebuildable from the stored post and the room's wrap, deleted by
    -- `purge_room_derived_views` in the same act as the FTS rows and the seq
    -- map, carrying no plaintext (a post id and a hash).
    -- `generation_id` is its twin's, for the same reason and with
    -- the same fail-closed NULL: a post seals under the room's generation
    -- exactly as a message does, so a post id is served only to a member
    -- holding that generation's wrap.
    CREATE TABLE IF NOT EXISTS room_post_views (
        room_id  BLOB NOT NULL,
        post_id  BLOB NOT NULL,
        doc_key  BLOB NOT NULL,
        generation_id BLOB,
        PRIMARY KEY (room_id, post_id),
        FOREIGN KEY (room_id) REFERENCES rooms(room_id)
    );

    -- The search door's lookup (hits come back as doc keys), and the post
    -- delete's (a deleted post takes its view with it).
    CREATE INDEX IF NOT EXISTS idx_room_post_views_doc
        ON room_post_views(room_id, doc_key);
    CREATE INDEX IF NOT EXISTS idx_room_post_views_post
        ON room_post_views(post_id);

";

/// The **bridged-conversation family** (`docs/goal/architecture/apps/bridges.md`
/// § Bridge-kind catalogue → Phase G; payload semantics owned by
/// `docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
/// adapter*): a blind sealed mailbox in both directions.
///
/// - `bridge_conversation_rooms` — one row per bridged room, keyed by the
///   `rooms` row's id (born `transport_only`, its roster seated with the user
///   and the bridge principal — `conversation-rooms.md` § Bridged rooms). The
///   far room id is the bridge's own spelling, unique per `(actor, bridge)`;
///   `capabilities` is the manifest's declared vector and `bridge_x25519` the
///   principal's attested key, both snapshotted at birth and refreshed by
///   every `room.upsert`.
/// - `bridge_conversation_messages` — every row in both directions, sealed to
///   the user's own recipient key. `id` is assigned in arrival order, so it is
///   the inbox cursor and the `received_at` order at once; `created_at` is the
///   far side's claim, carried and never ordered on.
/// - `bridge_conversation_outbox` — the items sealed to the bridge, one per
///   Sent row (same id), deleted by the bridge's ack — or by the nest when the
///   serving principal ends (`bridged_conversations::end_bridged_outbox_in_tx`),
///   which stamps the Sent row's `undelivered_at` (additive, 2026-10-03).
/// - `first_party_bridge_keys` — an in-process leg's X25519 keypair, the key
///   its outbound items are sealed to; minted on first use, the secret wrapped
///   under the deployment seed (`crate::nest_kek`'s
///   `FIRST_PARTY_BRIDGE_CONTEXT`; `bridged_conversations::first_party_bridge_key`).
///   Today the one row is the Nostr DM leg's (schema 118).
///
/// User-irrecoverable: the rows and messages are sealed content the user
/// received or sent. The outbox is derived — ciphertext of a Sent row, sealed
/// to a bridge key — and is the one table a principal's end deletes. The leg
/// key is not user data either: a lost one is re-minted, and the leg's queued
/// items sealed to the old one are re-sent by the user.
pub(super) const MIGRATIONS_BRIDGED_CONVERSATIONS: &str = "
    CREATE TABLE IF NOT EXISTS bridge_conversation_rooms (
        room_id             BLOB PRIMARY KEY,
        actor_id            BLOB NOT NULL,
        bridge_principal_id BLOB NOT NULL,
        bridge_id           TEXT NOT NULL,
        far_room_id         TEXT NOT NULL,
        label               TEXT,
        participants        TEXT NOT NULL DEFAULT '[]',
        self_address        TEXT,
        capabilities        TEXT NOT NULL,
        bridge_x25519       BLOB NOT NULL,
        created_at          INTEGER NOT NULL,
        last_at             INTEGER NOT NULL,
        UNIQUE (actor_id, bridge_id, far_room_id)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_conversation_rooms_principal
        ON bridge_conversation_rooms(actor_id, bridge_principal_id);

    CREATE TABLE IF NOT EXISTS bridge_conversation_messages (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        room_id        BLOB NOT NULL,
        actor_id       BLOB NOT NULL,
        direction      TEXT NOT NULL CHECK(direction IN ('in', 'out')),
        far_message_id TEXT,
        sender         TEXT NOT NULL,
        sealed_content BLOB NOT NULL,
        created_at     INTEGER NOT NULL,
        received_at    INTEGER NOT NULL,
        receipt        TEXT CHECK(receipt IS NULL OR receipt IN ('delivered', 'read')),
        undelivered_at INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_conversation_messages_far
        ON bridge_conversation_messages(room_id, far_message_id)
        WHERE far_message_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_bridge_conversation_messages_actor
        ON bridge_conversation_messages(actor_id, id);
    CREATE INDEX IF NOT EXISTS idx_bridge_conversation_messages_room
        ON bridge_conversation_messages(room_id, id);

    CREATE TABLE IF NOT EXISTS bridge_conversation_outbox (
        message_id          INTEGER PRIMARY KEY,
        room_id             BLOB NOT NULL,
        actor_id            BLOB NOT NULL,
        bridge_principal_id BLOB NOT NULL,
        ciphertext          BLOB NOT NULL,
        queued_at           INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_conversation_outbox_principal
        ON bridge_conversation_outbox(actor_id, bridge_principal_id, message_id);

    CREATE TABLE IF NOT EXISTS first_party_bridge_keys (
        bridge_id      TEXT PRIMARY KEY,
        secret_wrapped BLOB NOT NULL,
        x25519_public  BLOB NOT NULL,
        created_at     INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_FOLDERS: &str = "
    CREATE TABLE IF NOT EXISTS folders (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        -- The set's plaintext name -- only where it rests by design: a reserved
        -- `__` routing constant or a `public` folder's URL segment. NULL for
        -- every other set whose sealed twin `name_sealed` rests (schema 114 --
        -- path-sealing.md § the set-name plane). The table CHECK below keeps a
        -- NULL name addressable.
        name        TEXT,
        actor_id    BLOB NOT NULL,
        created_at  INTEGER NOT NULL,
        -- Cross-user shared folders (shared-folders Slice 1). NULL = owner-
        -- only: chunks stay on BackupKey. Non-NULL = the 32-byte MLS group id
        -- this set is bound to; its chunks seal under chunk_crypto keyed by the
        -- group fauna.chunk.v1 exporter root (key-material-hierarchy.md
        -- § Audience: an MLS group at a specific epoch).
        mls_group_id BLOB,
        -- Per-set WebDAV serve flag (webdav-server.md § Independent enablement).
        -- 0 = not served (the default); 1 = the user flagged this set to be served
        -- over WebDAV, so the MDA exposes it read/write to generic DAV clients.
        -- The actual per-set exposure gate; the deployment-wide webdav_enabled is
        -- only the protocol switch. Reserved `__` sets are never served.
        webdav_enabled INTEGER NOT NULL DEFAULT 0,
        -- Per-set conflict policy (file-sync.md § Conflicts, ratified
        -- 2026-07-10): 'auto' (text three-way merge when clean, else
        -- latest-writer-wins — the default) | 'latest_wins_always'. The
        -- ConflictPolicy wire strings (fauna_core::format).
        conflict_policy TEXT NOT NULL DEFAULT 'auto',
        -- Web-paywall tier (monetization.md § Pillar 2, the folder half): NULL =
        -- not paywalled (the default — a website folder serves publicly); non-NULL
        -- = the owner's subscription-tier name this website folder is paywalled
        -- to. The entitlement seam Pillar 3's engine consults before minting a
        -- visitor capability URL.
        web_paywall_tier TEXT,
        -- THE NEST PLACE's snapshot policy (folders re-model phase 2
        -- § Places -- the nest place). Every folder has a nest place and it is
        -- always live; what varies is whether that place KEEPS SNAPSHOTS and how
        -- long a quiet period it waits for. `retention_policy` is the third
        -- member of the same policy and keeps its own column -- naming it part of
        -- the nest place is a concept move.
        --
        -- BOTH NULLABLE ON PURPOSE -- NULL means nothing authoritative said and
        -- resolves to the nest-wide behavior. A NOT NULL DEFAULT 1 would have
        -- been a lie on the reserved `__` destination sets, which are refused a
        -- snapshot structurally (a snapshot pin defeats the custodian's
        -- latest-per-path reclamation -- message-segment-store.md § GC-safety)
        -- and would then rest claiming a policy they never obey. The structural
        -- refusal is not a user choice and stays a hard-coded rule; these two
        -- columns are the user's own knob and rest unset until the user turns it.
        --
        -- nest_snapshots: 1 = keep snapshots -- 0 = keep none -- NULL = unset.
        nest_snapshots INTEGER,
        -- nest_snapshot_quiet_secs: seconds of quiet before a cut -- NULL = use
        -- the nest-wide scheduler quiet period.
        nest_snapshot_quiet_secs INTEGER,
        -- VERSION-RETENTION bounds (file-versions.md § Retention ruling
        -- 1): the per-set SIBLING policy of `retention_policy`, bounding
        -- version history on the sync plane -- never re-mapped onto the armed
        -- snapshot column (§ 8 forbids the silent re-map). Canonical JSON
        -- {max_versions_per_path: N, max_age_days: M}
        -- (fauna_protocol::folders::VersionRetention); a 0 bound is unset;
        -- NULL = keep everything (the honest resting value, where every
        -- folder rests until its owner chooses); an unparseable value refuses
        -- and logs and never guesses (backup::version_prune).
        version_retention TEXT,
        -- AUDIENCE (folders re-model phase 4 -- folders.md § Target
        -- re-model; principles.md § The user always controls their data owns
        -- the invariant exception). Stores ONLY the owner's explicit
        -- DECLASSIFICATION: 'public' = world-readable by design, chunks and
        -- names/paths rest UNSEALED (integrity-protected only); NULL = not
        -- declassified. The wire tri-state is DERIVED (public if this column,
        -- else shared if mls_group_id IS NOT NULL, else private) -- bound-ness
        -- already rests authoritatively in mls_group_id, and a second stored
        -- spelling of 'shared' could only ever disagree with it
        -- (folder_handlers::audience_of is the one derivation). Nullable with
        -- no default: NULL is every undeclassified folder's resting value.
        audience TEXT,
        -- WEBSITE TOGGLE (folders re-model phase 4): 1 = this folder's
        -- recorded changes fan out to web_files IN ADDITION TO the head row
        -- (never instead -- web_files is not a GC reachability source;
        -- backup/gc.rs enumerates the oracle) and the folder serves as the
        -- user's website. It is the fan-out key. DEFAULT 0 =
        -- website off until the owner turns it on.
        website_enabled INTEGER NOT NULL DEFAULT 0,
        -- PUBLIC FLOOR (phase 4 slice 4f-i -- folders.md § Publicly-synced
        -- follow owns the rule). The head seq at the folder's most recent
        -- ->public transition: the public read plane
        -- (fauna.folders.public.fetch + its federation twin) serves ONLY rows
        -- STRICTLY ABOVE this. What the owner declassified is the folder FROM
        -- THE FLIP FORWARD -- the private era's metadata (edit timing, sizes,
        -- counts, sealed labels) was never declassified and never crosses the
        -- public plane. Making that boundary a stamped column rather than a
        -- per-row filter is what keeps it structural: one comparison, no
        -- per-row classification to get wrong.
        --
        -- Stamped in the ONE audience writer (update_folder_for_user), in the
        -- same UPDATE as the audience itself and guarded on the PRE-update
        -- value, so a re-assert of 'public' never re-stamps (which would hide
        -- content recorded during the public window). NOT cleared on a
        -- flip-back: the gate is audience-keyed, so a stale floor serves
        -- nothing, and the next ->public re-stamps it higher.
        --
        -- DEFAULT 0 is a CONSTANT that is correct per-row: a born-public
        -- folder's floor IS 0 (nothing was ever recorded privately) and for a
        -- never-declassified row the column is simply unread until its first
        -- ->public stamps it.
        public_floor_seq INTEGER NOT NULL DEFAULT 0,
        -- CONTENT RESIDENCY (folders re-model phase 5 -- file-sync.md
        -- § Content residency owns the model). NULL = full (the default: the
        -- nest holds metadata AND chunk bytes); 'metadata_only' = the owner's
        -- explicit and consent-gated choice that this folder's CHUNK BYTES
        -- never rest on
        -- the nest (metadata -- change feed, manifests, snapshots -- syncs
        -- exactly as always). Reserved `__` rails and any folder with a
        -- serving toggle on (website/webdav/paywall) are refused the value
        -- (handler-gated, both directions). The fail-closed reading is FULL:
        -- only an explicit, parsed 'metadata_only' may stop bytes resting.
        nest_content_residency TEXT,
        -- EXCLUSIVE EDITING (file-sync.md § Exclusive editing owns the
        -- model). 0 = off (the default); 1 = the owner asked that ONE DEVICE AT A
        -- TIME may write this folder, so a seat takes
        -- `fauna.folders.lease.acquire` before an upload pass and releases
        -- when it drains. Deliberately NOT folded into `conflict_policy`:
        -- that decides what happens AFTER a divergence, this tries to stop
        -- one BEFORE, and a lease-governed folder still needs a policy for
        -- what a lease cannot cover (an offline edit, an expired lease, a
        -- member who never took one). The live lease itself lives in
        -- `upload_leases`, never here -- this column is the owner's
        -- standing CHOICE, that table is the transient HOLDER.
        -- The fail-open reading is OFF: a value that does not parse must
        -- never freeze a user's own folder against their own writes.
        exclusive_editing INTEGER NOT NULL DEFAULT 0,
        -- The owner's signed statement that this folder is `public`
        -- (encryption-at-rest.md § Readable classes). Opaque to the nest: it
        -- stores and serves the blob and verifies nothing, because the seats
        -- verify it AGAINST the nest. Deliberately NOT cleared on a flip-back
        -- -- the next mint must count above it. NULL = never attested.
        audience_attestation BLOB,
        -- The client-minted 32-byte SET NONCE (mls-group-key-material.md § M2
        -- -> Writer-signed change records ruling (2) and its custody sub-bullet
        -- (f)). Stored opaque at create -- overwritten by the owner's update --
        -- echoed on both list arms. The nest vouches for nothing: ingest only
        -- checks a signed statement against THIS copy (the client's custody is
        -- the authority and the owner's reconcile pushes it when the echo
        -- differs). NULL = never sent.
        set_nonce BLOB,
        -- Per-actor, NOT global: reserved sets (__config, __mail, __index, …)
        -- are per-actor collections (file-sync.md § Reserved folders), and
        -- two users on one nest must each own their own. A global UNIQUE(name)
        -- let the first writer claim the single `__config`/`__mail`/… row
        -- nest-wide and made every other actor's write fail. The scope key is
        -- the (name, actor_id) pair — for the channel-scoped __conv sets the
        -- actor_id column carries the channel_id, so they stay unique too.
        --
        -- HASH COMPANION of `name` via
        -- `fauna_core::path_crypto::set_name_hash` -- what a set is addressed
        -- by once `name` rests sealed. Backed by the UNIQUE index
        -- `idx_folders_name_hash` below.
        -- Reserved `__` rows hash too and stay addressed by their literal
        -- routing-constant name.
        name_hash BLOB,
        -- SEALED LABEL over `name` -- convergent under `name_hash`. Reserved
        -- `__` names are routing constants and never seal.
        name_sealed           BLOB,
        -- SEALED LABELS over the include/exclude lists -- the owner's ABSOLUTE
        -- local filesystem layout and the sharpest item in the tightening set.
        -- Random-nonce mode: both are mutable under a fixed salt.
        include_paths_sealed  BLOB,
        exclude_paths_sealed  BLOB,
        -- SEALED LABEL over `retention_policy` (S6-e). Salt is `name_hash`
        -- rather than this row's `id` because retention is settable at CREATE
        -- and the id is minted at INSERT. Random-nonce mode: the policy is
        -- mutable under that fixed salt. Sealed to the LABEL AUDIENCE (owner
        -- plus roster member) because `member_summary` ships the plaintext to a
        -- member today -- see file-sync.md § Sealed names & paths.
        retention_policy_sealed BLOB,
        node_cache INTEGER NOT NULL DEFAULT 0,
        retention_policy TEXT,
        cached_snapshot_count INTEGER NOT NULL DEFAULT 0,
        cached_total_bytes INTEGER NOT NULL DEFAULT 0,
        cached_last_snapshot_at INTEGER,
        include_paths TEXT,
        exclude_paths TEXT,
        high_cadence INTEGER NOT NULL DEFAULT 0,
        -- CUSTODY COPY (reserved-folders.md § Destination capability owns the
        -- rule). 1 = this reserved row is the blind sealed mirror of ANOTHER
        -- location's rail or folder -- provisioned by this nest on the first
        -- custody write. 0 = a rail or an ordinary folder. Written 1 ONLY by the
        -- two nest-side provisioners and on no wire kind. Read through ONE seam
        -- (snapshots::is_reserved_custody_copy). The CHECK makes a custody copy
        -- on a non-reserved name unrepresentable.
        custody_copy INTEGER NOT NULL DEFAULT 0
            CHECK (custody_copy = 0 OR substr(COALESCE(name, ''), 1, 2) = '__'),
        -- SQLite NULLs are distinct here: any number of NULL-named sets sit
        -- under one owner -- the hash index below is their uniqueness.
        UNIQUE(name, actor_id),
        CHECK (name IS NOT NULL OR (name_hash IS NOT NULL AND name_sealed IS NOT NULL))
    );
    -- The hash twin of the plaintext UNIQUE(name, actor_id): a duplicate is
    -- impossible by construction, since the digests are 1:1 with the names.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_folders_name_hash
        ON folders(name_hash, actor_id);

    CREATE TABLE IF NOT EXISTS folder_members (
        folder_id INTEGER NOT NULL REFERENCES folders(id),
        device_id   BLOB NOT NULL,
        -- The device place's behavior flags (folders re-model § Places) -- one
        -- column per `fauna_protocol::folders::PlaceFlags` field, same
        -- spelling, so the struct and the row cannot drift apart under a grep.
        -- The three flags ARE the place: every writer states all three
        -- (`upsert_folder_place`), so there is no default to rest on a seat
        -- nobody configured. (The legacy `role` column they replaced dropped
        -- at schema 95.)
        originates      INTEGER NOT NULL,
        accepts         INTEGER NOT NULL,
        applies_deletes INTEGER NOT NULL,
        PRIMARY KEY (folder_id, device_id)
    );
";

// Shared folder content-key envelopes (shared-folders Slice 3, M2). One
// opaque sealed envelope per group, keyed by the 32-byte derived ChannelId,
// re-published (upsert) on every membership change. The nest holds it as opaque
// ciphertext — it never has the group secret; the bytes seal/open in `fauna-mls`
// (mls-group-key-material.md § M2 content-key mechanism). `sealed` is the AEAD
// output; `epoch` is the MLS epoch the owner sealed under (staleness metadata).
pub(super) const MIGRATIONS_FOLDER_CONTENT_KEYS: &str = "
    CREATE TABLE IF NOT EXISTS folder_content_keys (
        channel_id  BLOB PRIMARY KEY,
        epoch       INTEGER NOT NULL,
        sealed      BLOB NOT NULL,
        updated_at  INTEGER NOT NULL,
        -- The owner-stamped generation floor for non-owner records (KMH § M2
        -- version floor). Monotonic: content_key.put only ever raises it (the
        -- greater of stored and new). Every put stamps it.
        current_version INTEGER NOT NULL
    );
";

// Per-(channel, actor) member access for multi-writer shared folders (Phase 1;
// folders.md § Sharing owns the access model, file-sync.md § Multi-writer
// shared sets the metering mechanics). Absent row = `reader`. `bytes_used` is an abuse
// counter (floored at 0 on reclaim), not exact attribution; `byte_cap` NULL =
// uncapped (blank-cap-means-uncapped is ratified; the warning lives client-side).
// Written only via the owner+claimant-gated `fauna.folders.members.set_access`
// and the share-time access param (same claim transaction as the share itself).
pub(super) const MIGRATIONS_FOLDER_MEMBER_ROLES: &str = "
    CREATE TABLE IF NOT EXISTS folder_member_access (
        channel_id  BLOB NOT NULL,
        actor_id    BLOB NOT NULL,
        access      TEXT NOT NULL CHECK (access IN ('reader', 'writer')),
        byte_cap    INTEGER,
        bytes_used  INTEGER NOT NULL DEFAULT 0,
        updated_at  INTEGER NOT NULL,
        PRIMARY KEY (channel_id, actor_id)
    );
";

// First-binder-wins claim on the `group_id -> ChannelId` namespace. The `group_id -> ChannelId` namespace was unowned: `share_core` let
// any owner bind their set to an arbitrary client-supplied `mls_group_id` and self-
// register on the derived roster, with no first-binder claim — so a removed member
// could re-`share` to re-add themselves (defeating the F1 `members.evict`), and a
// group-id-knower could clobber a victim group's content-key envelope slot. This
// table records the **single authorized folder owner** of a channel: the first
// actor to bind a folder to it. `share` / `content_key.put` / `members.evict` all
// reject a caller other than `claimed_by` (`db::channels::claim_folder_channel`).
// A pre-populated channel (a conversation, whose members are already on the
// `actor_channels` roster) is claimable only by an existing roster member — so an
// outsider who merely knows the raw group id cannot inject onto a conversation's
// roster (the "treat a conv-born channel as claimed by the conversation" caveat,
// resolved purely by reading the shared roster — no conversations-side change).
pub(super) const MIGRATIONS_FOLDER_CHANNEL_CLAIMS: &str = "
    CREATE TABLE IF NOT EXISTS folder_channel_claims (
        channel_id  BLOB PRIMARY KEY,
        claimed_by  BLOB NOT NULL,
        claimed_at  INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_FEEDS: &str = "
    CREATE TABLE IF NOT EXISTS feeds (
        feed_id     TEXT PRIMARY KEY,
        owner       BLOB NOT NULL,
        name        TEXT NOT NULL,
        rules       BLOB NOT NULL,
        combination TEXT NOT NULL DEFAULT 'all',
        created_at  INTEGER NOT NULL,
        scope TEXT NOT NULL DEFAULT 'local',
        contributor_seeds TEXT NOT NULL DEFAULT '[]',
        composition BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_feeds_owner ON feeds(owner);
";

// The user's GLOBAL factor set (frame § Composition — scope is by
// container): per-user `(factor, weight)` entries folded into every one of
// their feeds' composed `order=score` orderings. Transparent factor
// preferences only — sealed tier-1 factor data never lands here (the nest
// can't read it); the whole set is replaced on `fauna.feed.factors.set`.
pub(super) const MIGRATIONS_FEED_GLOBAL_FACTORS: &str = "
    CREATE TABLE IF NOT EXISTS feed_global_factors (
        owner            BLOB    NOT NULL,
        factor           TEXT    NOT NULL,
        weight_permille  INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL,
        PRIMARY KEY (owner, factor)
    );
";

pub(super) const MIGRATIONS_FEED_CONTRIBUTORS: &str = "
    CREATE TABLE IF NOT EXISTS feed_contributors (
        feed_id        TEXT NOT NULL,
        nest_url       TEXT NOT NULL,
        author_id      BLOB NOT NULL DEFAULT x'',
        hit_count      INTEGER NOT NULL DEFAULT 0,
        last_seen      INTEGER NOT NULL DEFAULT 0,
        poll_priority  TEXT NOT NULL DEFAULT 'hot',
        discovered_via TEXT NOT NULL DEFAULT 'seed',
        created_at     INTEGER NOT NULL,
        PRIMARY KEY (feed_id, nest_url, author_id)
    );
    CREATE INDEX IF NOT EXISTS idx_feed_contributors_feed
        ON feed_contributors(feed_id, poll_priority);
";

pub(super) const MIGRATIONS_SNAPSHOTS: &str = "
    CREATE TABLE IF NOT EXISTS snapshots (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        folder_id INTEGER NOT NULL REFERENCES folders(id),
        created_at  INTEGER NOT NULL,
        file_count  INTEGER NOT NULL,
        total_bytes INTEGER NOT NULL,
        -- HASH COMPANION of the create's tag list -- a JSON array of hex
        -- `fauna_core::path_crypto::snapshot_tag_hash` digests written at
        -- create. What the retention pruner matches a policy's `keep_tags`
        -- against. The tags themselves never rest in plaintext.
        tag_hashes  TEXT,
        -- SEALED LABEL over the whole tag list as the display copy --
        -- random-nonce mode because the list grows under a fixed salt. The
        -- equality-matching half is `tag_hashes` above.
        tags_sealed BLOB,
        parent_id INTEGER REFERENCES snapshots(id),
        device_id BLOB,
        max_change_seq INTEGER,
        deletion_pending INTEGER NOT NULL DEFAULT 0,
        soft_deleted INTEGER NOT NULL DEFAULT 0,
        purge_after INTEGER,
        message_kind TEXT,
        message_manifest BLOB,
        placement_manifest BLOB,
        UNIQUE(folder_id, created_at)
    );

    CREATE TABLE IF NOT EXISTS snapshot_files (
        snapshot_id   INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
        manifest_hash BLOB   NOT NULL,
        size_bytes    INTEGER NOT NULL,
        mtime         INTEGER NOT NULL,
        -- HASH COMPANION of the file's path via `fauna_core::sync::path_hash`
        -- and the row's PK — no plaintext path column rests here. Nullable in
        -- the declaration but every writer supplies it.
        path_hash     BLOB,
        -- SEALED LABEL over the path -- convergent under `path_hash`. Copied
        -- verbatim off the membership projection at snapshot creation, which is
        -- a keyless server-side row copy.
        path_sealed   BLOB,
        mode INTEGER NOT NULL DEFAULT 0,
        file_type TEXT NOT NULL DEFAULT 'regular',
        symlink_target TEXT,
        PRIMARY KEY (snapshot_id, path_hash)
    );
";

pub(super) const MIGRATIONS_SNAPSHOTS_V3: &str = "
    CREATE INDEX IF NOT EXISTS idx_snapshots_folder_id ON snapshots(folder_id);
    CREATE INDEX IF NOT EXISTS idx_snapshots_created_at ON snapshots(created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_snapshot_files_snapshot_id ON snapshot_files(snapshot_id);
    CREATE INDEX IF NOT EXISTS idx_snapshot_files_path_hash
        ON snapshot_files(snapshot_id, path_hash);
    CREATE INDEX IF NOT EXISTS idx_snapshots_message_kind_scope
        ON snapshots(message_kind, folder_id, created_at);
";

/// Restore history — one row per kind-aware snapshot restore performed by
/// nest. Surfaced in the Backups page (docs/goal/ui/backups.md §
/// Restore history).
pub(super) const MIGRATIONS_RESTORE_HISTORY: &str = "
    CREATE TABLE IF NOT EXISTS restore_history (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        completed_at    INTEGER NOT NULL,
        actor_id        BLOB    NOT NULL,
        snapshot_id     INTEGER NOT NULL,
        kinds_restored  TEXT    NOT NULL,
        source_member_id BLOB,
        FOREIGN KEY (snapshot_id) REFERENCES snapshots(id)
    );
    CREATE INDEX IF NOT EXISTS idx_restore_history_actor
        ON restore_history(actor_id, completed_at);
";

/// MUA-ahead-at-reconnect divergence log (spec D6 (γ) of
/// imap-caldav-restore-design.md). Populated at IMAP SELECT / CalDAV
/// sync-collection REPORT time in Plan 2 when a MUA's last_modseq
/// exceeds the server's restored highestmodseq. Retained for the
/// snapshot's recovery window (32 days per backup-restore.md § 5);
/// retention enforced by the snapshot GC.
pub(super) const MIGRATIONS_BRIDGE_RESTORE_DIVERGENCE: &str = "
    CREATE TABLE IF NOT EXISTS bridge_restore_divergence (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        snapshot_id     INTEGER NOT NULL,
        observed_at     INTEGER NOT NULL,
        actor_id        BLOB    NOT NULL,
        protocol        TEXT    NOT NULL,
        collection      TEXT    NOT NULL,
        mua_id          TEXT,
        client_modseq   INTEGER NOT NULL,
        server_modseq   INTEGER NOT NULL,
        lost_event_count INTEGER NOT NULL,
        FOREIGN KEY (snapshot_id) REFERENCES snapshots(id)
    );
    CREATE INDEX IF NOT EXISTS idx_restore_divergence_snapshot_actor
        ON bridge_restore_divergence(snapshot_id, actor_id);
";

pub(super) const MIGRATIONS_EMAIL_FILTERS: &str = "
    CREATE TABLE IF NOT EXISTS email_filters (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        owner       BLOB NOT NULL,
        name        TEXT NOT NULL DEFAULT '',
        rules       BLOB NOT NULL,
        combination TEXT NOT NULL DEFAULT 'all',
        action      TEXT NOT NULL DEFAULT 'discard',
        priority    INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL,
        continue_on_match INTEGER NOT NULL DEFAULT 0,
        -- The per-rule Forward action's copy mode (mail-forwarding.md § Per-rule
        -- forward-to): 1 = `redirect` (forward with no local delivery) and 0 =
        -- `copy` (the default). Meaningful only when `action` is
        -- `forward:<address>` — a column rather than a new action-string prefix
        -- so an older nest binary reading a newer database sees a `copy` rule
        -- instead of misparsing the string (version-compatibility.md I4: an
        -- older binary ignores columns it does not know).
        forward_redirect INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_email_filters_owner ON email_filters(owner, priority);
";

pub(super) const MIGRATIONS_REGISTRATION: &str = "
    CREATE TABLE IF NOT EXISTS invite_codes (
        code        TEXT PRIMARY KEY,
        tier        TEXT NOT NULL REFERENCES tiers(name),
        uses_left   INTEGER NOT NULL DEFAULT 1,
        created_at  INTEGER NOT NULL,
        guardian_actor BLOB,
        -- family-safety.md § The account age band: the band the code
        -- admits under (wire token, e.g. 'u13'); NULL = no band chosen.
        -- Only mintable beside guardian_actor (handler-validated).
        age_band    TEXT
    );

    CREATE TABLE IF NOT EXISTS handle_cooldowns (
        handle      TEXT PRIMARY KEY,
        old_actor_id BLOB NOT NULL,
        released_at INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_BRIDGE_FEED_SUBSCRIPTIONS: &str = "
    CREATE TABLE IF NOT EXISTS bridge_feed_subscriptions (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id    BLOB NOT NULL,
        bridge      TEXT NOT NULL,
        feed_uri    TEXT NOT NULL,
        name        TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        UNIQUE(actor_id, bridge, feed_uri)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_feed_subs_actor
        ON bridge_feed_subscriptions(actor_id, created_at DESC);
";

pub(super) const MIGRATIONS_EVICTION_TOKEN: &str = "
    CREATE TABLE IF NOT EXISTS eviction_tokens (
        token TEXT PRIMARY KEY,
        actor_id BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_OPERATION_LOCKS: &str = "
    CREATE TABLE IF NOT EXISTS operation_locks (
        lock_type TEXT NOT NULL,
        folder_id INTEGER NOT NULL,
        holder TEXT NOT NULL,
        acquired_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        UNIQUE(lock_type, folder_id)
    );

    CREATE TABLE IF NOT EXISTS upload_leases (
        folder_id INTEGER NOT NULL UNIQUE,
        device_id BLOB NOT NULL,
        acquired_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        heartbeat_at INTEGER NOT NULL,
        -- The ACCOUNT that took the lease.
        actor_id BLOB NOT NULL
    );
";

pub(super) const MIGRATIONS_CONTENT_LABELS: &str = "
    CREATE TABLE IF NOT EXISTS content_labels (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        content_type     TEXT NOT NULL,
        content_id       TEXT NOT NULL,
        category         TEXT NOT NULL,
        confidence       REAL NOT NULL,
        mechanism_type   INTEGER NOT NULL,
        classifier_id    BLOB NOT NULL,
        classifier_version INTEGER NOT NULL,
        attestation_type INTEGER NOT NULL,
        attestation_data BLOB,
        obligation_id    BLOB,
        created_at       INTEGER NOT NULL,
        scanner_id       BLOB NOT NULL,
        signature        BLOB NOT NULL,
        UNIQUE(content_type, content_id, category, classifier_id)
    );
    CREATE INDEX IF NOT EXISTS idx_content_labels_ref
        ON content_labels(content_type, content_id);
    CREATE INDEX IF NOT EXISTS idx_content_labels_category
        ON content_labels(category, confidence);
";

pub(super) const MIGRATIONS_OBLIGATION_ACTIONS: &str = "
    CREATE TABLE IF NOT EXISTS obligation_action_records (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        content_type    TEXT NOT NULL,
        content_id      TEXT NOT NULL,
        author_hex      TEXT NOT NULL DEFAULT '',
        obligation_id   BLOB NOT NULL,
        rule_index      INTEGER NOT NULL,
        category        TEXT NOT NULL,
        confidence      REAL NOT NULL,
        action_taken    INTEGER NOT NULL,
        label_id        INTEGER REFERENCES content_labels(id),
        timestamp       INTEGER NOT NULL,
        signature       BLOB NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_obligation_actions_content
        ON obligation_action_records(content_type, content_id);
    CREATE INDEX IF NOT EXISTS idx_obligation_actions_obligation
        ON obligation_action_records(obligation_id);
    CREATE INDEX IF NOT EXISTS idx_obligation_actions_author
        ON obligation_action_records(author_hex);
";

// User-initiated abuse reports (`moderation.md` § User-initiated reporting →
// *Where it lands*). One row per report: a local reporter's own report
// (`reporter_actor` set, `origin_nest_id` NULL) or a copy forwarded from a peer
// nest (`reporter_actor` NULL — the reporter's identity never crosses a nest
// boundary — with `origin_nest_id` + `origin_report_ref` naming it). Evidence
// for an admin, never a lever: nothing here feeds `sender_reports`,
// `content_reports` or `content_labels`. Resolved rows stay as the admin's
// audit record; withdrawal NULLs `note` and `excerpt` (the reporter's data).
pub(super) const MIGRATIONS_ABUSE_REPORTS: &str = "
    CREATE TABLE IF NOT EXISTS abuse_reports (
        id                TEXT PRIMARY KEY,
        created_at        INTEGER NOT NULL,
        reporter_actor    BLOB,
        origin_nest_id    TEXT,
        origin_report_ref TEXT,
        subject_kind      TEXT NOT NULL,
        subject_id        TEXT NOT NULL,
        -- The conversation channel of a message subject (NULL otherwise).
        subject_channel   TEXT,
        subject_actor     TEXT,
        reason            TEXT NOT NULL,
        note              TEXT,
        excerpt           TEXT,
        block_author      INTEGER NOT NULL DEFAULT 0,
        -- The peer nest's URL a local report was forwarded to, set only once
        -- the author's home nest accepted it (NULL when not).
        forwarded_to      TEXT,
        status            TEXT NOT NULL DEFAULT 'open',
        outcome           TEXT,
        resolved_at       INTEGER,
        resolved_by       BLOB,
        -- The verified nest id (hex) `forwarded_to` answered as: the one peer
        -- whose `outcome` this report accepts. On a forwarded copy,
        -- `origin_nest_id` is likewise the channel-verified origin's hex id.
        forwarded_nest_id TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_abuse_reports_reporter
        ON abuse_reports(reporter_actor, created_at);
    CREATE INDEX IF NOT EXISTS idx_abuse_reports_status
        ON abuse_reports(status, created_at);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_abuse_reports_origin_ref
        ON abuse_reports(origin_nest_id, origin_report_ref)
        WHERE origin_report_ref IS NOT NULL;

    -- The federation triad's durable send queue (`moderation.md` § Routing):
    -- one pending `fauna.federation.abuse_report.{deliver,withdraw,outcome}`
    -- call, retried with backoff until the peer answers or the entry ages
    -- out. `payload` is the canonical dag-cbor request; `peer_nest_id` (hex)
    -- pins the dial when the peer's identity is already known. Derived send
    -- state, not user data: a lost entry costs a forward, never a report.
    CREATE TABLE IF NOT EXISTS abuse_report_outbox (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        report_id       TEXT NOT NULL,
        kind            TEXT NOT NULL,
        peer_url        TEXT NOT NULL,
        peer_nest_id    TEXT,
        payload         BLOB NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_abuse_report_outbox_due
        ON abuse_report_outbox(next_attempt_at);
";

pub(super) const MIGRATIONS_SPAM_MODELS: &str = "
    CREATE TABLE IF NOT EXISTS spam_models (
        actor_id    BLOB PRIMARY KEY,
        model_json  BLOB NOT NULL,
        ham_count   INTEGER NOT NULL DEFAULT 0,
        spam_count  INTEGER NOT NULL DEFAULT 0,
        updated_at  INTEGER NOT NULL
    );
";

// Sealed personalization-model home (`docs/goal/behavior/topic-factors.md`
// § At rest — seal + home): `spam_models` generalized by factor key — one
// mutable row per (actor, factor), v1 factor namespace `topic:<hex>`.
// `sealed_blob` is sealed client-side under the BackupKey and stored
// verbatim-opaque (no server-side train path exists at all — the nest never
// holds a plaintext personalization model at any point in the lifecycle).
// `sample_count` is the client's ADVISORY training-event count
// (adopt-if-larger cross-device reconcile hint), never validated against the
// blob. User-irrecoverable data: rows are removed only by the owner's own
// `fauna.personalization.model.delete`, never by a migration.
pub(super) const MIGRATIONS_PERSONALIZATION_MODELS: &str = "
    CREATE TABLE IF NOT EXISTS personalization_models (
        actor_id     BLOB    NOT NULL,
        factor       TEXT    NOT NULL,
        sealed_blob  BLOB    NOT NULL,
        sample_count INTEGER NOT NULL DEFAULT 0,
        updated_at   INTEGER NOT NULL,
        PRIMARY KEY (actor_id, factor)
    );
";

pub(super) const MIGRATIONS_SPAM_PREFERENCES: &str = "
    CREATE TABLE IF NOT EXISTS spam_preferences (
        actor_id            BLOB PRIMARY KEY,
        spam_threshold      REAL NOT NULL DEFAULT 0.5,
        phishing_threshold  REAL NOT NULL DEFAULT 0.3,
        contribute_baseline INTEGER NOT NULL DEFAULT 0,
        -- Distributed report sharing opt-in (report-sharing.md § Report
        -- capture): default OFF; when 1 the actor's explicit spam flags emit
        -- a k-anonymized content_reports row.
        share_reports       INTEGER NOT NULL DEFAULT 0,
        -- Layer-B engagement-signal sharing opt-in (engagement-cues.md § Layer
        -- B nest legs): default OFF; when 1 the actor's derived cue verdicts
        -- (fauna.moderation.signal_contribute) emit k-anonymized signal:* rows
        -- into the SAME content_reports table. INDEPENDENT of share_reports —
        -- opting out of one family never wipes the other's rows (the opt-out
        -- sweep is factor-prefix-scoped).
        share_signals       INTEGER NOT NULL DEFAULT 0,
        updated_at          INTEGER NOT NULL
    );
";

// Distributed report sharing (report-sharing.md § Report capture + § The
// k-anonymity choke point). One row per (content_hash, factor, reporter):
// the caller-scoped fact that `reporter` flagged the content identified by
// the canonical 32-byte hash (mail report_hash; a post's content-addressed
// id). NEVER exported and NEVER served below the k floor — the ONLY read
// surfaces are the k-gated aggregate writer / export / transparency read,
// all of which consume `fauna_core::scoring::reports::exposed_report_count`;
// no RPC (admin included) SELECTs this table directly. Rows carry no content
// bytes; they survive message deletion (the judgment stands) and are deleted
// by the reporter's opt-out / ham-correction / undo.
pub(super) const MIGRATIONS_CONTENT_REPORTS: &str = "
    CREATE TABLE IF NOT EXISTS content_reports (
        content_hash BLOB    NOT NULL,
        factor       TEXT    NOT NULL,
        reporter     BLOB    NOT NULL,
        content_kind TEXT    NOT NULL,
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (content_hash, factor, reporter)
    );
    CREATE INDEX IF NOT EXISTS idx_content_reports_reporter
        ON content_reports(reporter);

    -- Peer-imported >=k report aggregates (report-sharing.md § Federation
    -- exchange). One row per (hash, factor, peer); latest-epoch-wins per
    -- peer. NEVER re-exported (no laundering) and NEVER scales local
    -- consensus — any number of rows across any peers collapses into ONE
    -- flat corroboration bucket at read time (federation.md hostile-signer
    -- invariant). Per-peer row count is capped (reports are per-item, an
    -- unbounded id universe unlike per-sender reputation) with
    -- oldest-updated eviction.
    CREATE TABLE IF NOT EXISTS peer_content_reports (
        content_hash  BLOB    NOT NULL,
        factor        TEXT    NOT NULL,
        peer_nest_id  BLOB    NOT NULL,
        claimed_count INTEGER NOT NULL,
        epoch         INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL,
        PRIMARY KEY (content_hash, factor, peer_nest_id)
    );
    CREATE INDEX IF NOT EXISTS idx_peer_content_reports_peer
        ON peer_content_reports(peer_nest_id, updated_at);

    -- Prior exchange partners (federation.md § the exchange originator plane):
    -- peer base URLs this nest successfully exchanged reputation/report
    -- aggregates with, recorded at ORIGINATION time only (the channel
    -- handshake carries no origin URL, so an inbound peer is unrecordable —
    -- it records us when its own originator dials back). Re-assembled into
    -- the peer set each cycle alongside the pairing URL and the distinct
    -- feed_contributors nests. Bounded by most-recent-success eviction.
    -- Ephemeral/derived by nature (re-grows from the other two sources) but
    -- kept across boots so partner links outlive contributor churn.
    CREATE TABLE IF NOT EXISTS exchange_peers (
        nest_url        TEXT    PRIMARY KEY,
        nest_id         BLOB    NOT NULL,
        last_success_at INTEGER NOT NULL,
        created_at      INTEGER NOT NULL
    );
";

// Peer-imported >=k trend entries (trending.md § Federation exchange), the
// distinct-peer ramp's input. One row per (content_id, peer_nest_id);
// latest-epoch-wins per peer. Unlike the flat report bucket, the trend ramp is a
// COUNT(DISTINCT peer_nest_id) over LIVE (un-expired) rows — presence-only, never
// the claimed magnitude (`score_pm`/`engager_count` are stored as a
// fetch-prioritization hint + the importer's k-gate re-validation input, NEVER
// summed into local consensus and NEVER re-exported: no laundering). Bounded two
// ways: a per-peer row cap (`MAX_PEER_TREND_ROWS`, oldest-updated evicted — the id
// universe is unbounded) and a 48 h age TTL (`TREND_PEER_TTL_HOURS` — an entry
// past the decay horizon is dead weight; the recompute filters by TTL at read and
// the sweep purges physically). Ephemeral/derived by construction (re-populated by
// the next exchange) — recreatable, never migrated (droppable, no user data).
pub(super) const MIGRATIONS_PEER_CONTENT_TRENDS: &str = "
    CREATE TABLE IF NOT EXISTS peer_content_trends (
        content_id    BLOB    NOT NULL,
        peer_nest_id  BLOB    NOT NULL,
        score_pm      INTEGER NOT NULL,
        engager_count INTEGER NOT NULL,
        epoch         INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL,
        PRIMARY KEY (content_id, peer_nest_id)
    );
    CREATE INDEX IF NOT EXISTS idx_peer_content_trends_peer
        ON peer_content_trends(peer_nest_id, updated_at);
    CREATE INDEX IF NOT EXISTS idx_peer_content_trends_updated
        ON peer_content_trends(updated_at);
";

// Deployment-wide spam baseline (`mail-spam.md` § Cold start, Path 2). A single
// row (`id = 0`) holding the aggregate `fauna_mail::spam::SpamModel` serde_json
// merged over the opt-in users' per-user `spam_models` (those with
// `spam_preferences.contribute_baseline = 1`), republished by an admin via
// `fauna.bridges.publish_spam_baseline`. The goal doc describes it as a
// `<data-dir>/spam-models/__baseline.bin` file, but per-user models live in the
// `spam_models` TABLE (not files), so the baseline is its DB sibling — same
// `model_json` shape, one canonical store. The merged n-gram weights are sums
// that do not identify which user contributed (`mail-spam.md` § Cold start
// Path 2). Read-only from each per-actor classifier's perspective (cold-start
// seeding is a read-time merge in `fetch_spam_model`, a later slice). No
// user-irrecoverable data: a re-publishable derived aggregate of the still-intact
// per-user models.
pub(super) const MIGRATIONS_SPAM_BASELINE: &str = "
    CREATE TABLE IF NOT EXISTS spam_baseline (
        id            INTEGER PRIMARY KEY CHECK (id = 0),
        model_json    BLOB    NOT NULL,
        ham_count     INTEGER NOT NULL DEFAULT 0,
        spam_count    INTEGER NOT NULL DEFAULT 0,
        contributors  INTEGER NOT NULL DEFAULT 0,
        published_at  INTEGER NOT NULL
    );
";

// The spam baseline's delta floor + standing publish state (`mail-spam.md`
// § Cold start, Path 2 → *The floor applies to every published DELTA* and
// *Standing publish*, ruled 2026-09-21).
//
// `spam_baseline_inclusions` is the nest-internal **inclusion record**: one
// row per contributor of the last SERVED publish — the actor and the
// `spam_models.updated_at` that was summed. A departure that leaves the account
// standing marks its row `departed` rather than deleting it (so leaving and
// rejoining before the next publish counts once). Actor-keyed, so an ordinary
// `ACTOR_TABLES` row (Purge; Move); never served to any client, the admin
// included.
//
// `spam_baseline_run_state` is its singleton (`id = 1`): the count of
// inclusion rows an account deletion purged since the last landed publish,
// `last_served_at` (set by the first landed publish and never cleared — the
// delta floor's reference survives a withdrawal), the last run's time and
// outcome (`deferred`, `skipped_contributors`) the admin read reports, and a
// `departures` generation every departure bumps, so a publish a departure
// raced does not land the departed counts.
//
// Recreatable bookkeeping, not user data: losing it costs at most one publish
// measured as a first publish.
pub(super) const MIGRATIONS_SPAM_BASELINE_DELTA: &str = "
    CREATE TABLE IF NOT EXISTS spam_baseline_inclusions (
        actor_id          BLOB    PRIMARY KEY,
        model_updated_at  INTEGER NOT NULL,
        departed          INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS spam_baseline_run_state (
        id                               INTEGER PRIMARY KEY CHECK (id = 1),
        purged_inclusions_since_publish  INTEGER NOT NULL DEFAULT 0,
        last_served_at                   INTEGER,
        last_run_at                      INTEGER,
        deferred                         INTEGER NOT NULL DEFAULT 0,
        skipped_contributors             INTEGER NOT NULL DEFAULT 0,
        departures                       INTEGER NOT NULL DEFAULT 0
    );
";

// Per-user spam training-history audit trail (`mail-spam.md` § Training-sample
// retention). One row per training event a capability holder (the user's client
// or the AUTH'd MDA session) wrote through `put_spam_model`, so the user can
// review what trained their filter (`list_spam_training_history`) and reverse a
// single event (the client's own `put_spam_model` history `Delete`). The row
// rests sealed: `model_delta_applied` is the distinct n-gram set the event added
// (`SpamModel::delta_ngrams`) and `sealed_subject` the message subject, both
// SEALED to the actor's own recipient key — the nest holds only the public half,
// stores them verbatim and can neither read nor seal them. `mailbox`, `label`
// and `source` are plaintext metadata, not message content. Rows are GC'd after
// `mail.spam.training_history_retention_days` (default 30) — derived audit
// metadata, not user-irrecoverable content (the n-gram weights persist in
// `spam_models`), so age-out loses no user-recoverable data.
pub(super) const MIGRATIONS_SPAM_TRAINING_HISTORY: &str = "
    CREATE TABLE IF NOT EXISTS spam_training_history (
        history_id          BLOB PRIMARY KEY,
        actor_id            BLOB    NOT NULL,
        message_id          BLOB    NOT NULL,
        mailbox             TEXT    NOT NULL,
        label               TEXT    NOT NULL,
        source              TEXT    NOT NULL,
        model_delta_applied BLOB    NOT NULL,
        created_at          INTEGER NOT NULL,
        sealed_subject      BLOB    NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_spam_training_history_actor
        ON spam_training_history (actor_id, created_at DESC, history_id DESC);
";

// Deployment-baseline holder copies (`mail-spam.md` § Encrypted-mode
// interaction, ratified 2026-07-13): an opt-in contributor's per-user spam
// model, HPKE-sealed by their own client/agent to the aggregation holder's
// pubkey (a `SpamModelCopyBlob` — nest-opaque). One row per (actor, holder);
// replaced atomically with each `put_spam_model` write that carries a
// `holder_copy`, served to the holder during a `publish_spam_baseline` drain
// run only while the paired keyless `content.read{spam-model}` grant stands,
// and deleted on opt-out / grant revocation / model reset. `holder_pubkey` is
// the same identity `capability_grants.holder_pubkey` keys grants by, so
// copy↔grant pairing is a join, not a blob parse. Recreatable derived data
// (the contributor's agent re-seals a fresh copy on its next write), so
// deletion loses no user-irrecoverable content.
pub(super) const MIGRATIONS_SPAM_MODEL_HOLDER_COPIES: &str = "
    CREATE TABLE IF NOT EXISTS spam_model_holder_copies (
        actor_id      BLOB    NOT NULL,
        holder_pubkey BLOB    NOT NULL,
        sealed_copy   BLOB    NOT NULL,
        updated_at    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, holder_pubkey)
    );
    CREATE INDEX IF NOT EXISTS idx_spam_model_holder_copies_holder
        ON spam_model_holder_copies (holder_pubkey);
";

// Per-account mail settings (one row per actor), sibling of `spam_preferences`.
// `forward_all_to` holds the per-account "forward all incoming mail to" address
// at the plaintext routing-metadata floor in BOTH storage modes
// (`mail-forwarding.md` § Where the forward config lives at rest — the same
// tier as `local_domains`/aliases/admin-forwarders; the separate-bridge-sealed
// form is the deferred N1b upgrade). `forward_per_hour` is the per-account
// forward rate cap (`mail.account.forward_per_hour`, default 100/h,
// `mail-forwarding.md:168`); no write path yet.
pub(super) const MIGRATIONS_MAIL_ACCOUNT_SETTINGS: &str = "
    CREATE TABLE IF NOT EXISTS mail_account_settings (
        actor_id         BLOB PRIMARY KEY,
        forward_all_to   TEXT,
        forward_per_hour INTEGER NOT NULL DEFAULT 100,
        spam_threshold_override INTEGER,
        updated_at       INTEGER NOT NULL
    );
";

/// Per-deployment SRS secret(s) (`mail-forwarding.md` § SRS secret). A
/// random 32-byte HMAC key, auto-seeded on first DB open (the admin only
/// *rotates* it via `rotate_srs_secret` (N5) and never reads its bytes —
/// `mail-forwarding.md:273`), so it is deployment crypto material, not a
/// CLI/admin-set value (product invariant: nest config from clients, not CLI).
/// Modelled as a multi-row table so the N5 2-secret rotation overlap
/// (`:97,:256`) inserts a new row alongside the old one without a migration;
/// N3 holds exactly one row. Encode (`srs_forward`) uses the newest row;
/// decode (`decode_srs_bounce`) tries every row so an in-flight bounce issued
/// under the prior secret still verifies during the overlap.
pub(super) const MIGRATIONS_MAIL_SRS_SECRETS: &str = "
    CREATE TABLE IF NOT EXISTS mail_srs_secrets (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        secret     BLOB    NOT NULL,
        created_at INTEGER NOT NULL
    );
";

/// Per-deployment RFC 8058 one-click-unsubscribe HMAC secret
/// (`docs/goal/behavior/mail-mass-mailing.md` § Token format: "a 32-byte
/// secret in nest state ... server-managed"). The exact sibling of
/// [`MIGRATIONS_MAIL_SRS_SECRETS`]: a random 32-byte HMAC key auto-seeded on
/// first DB open, **nest-held** (not wrapped to the MTA — it is signing/lookup
/// material, the same class as the SRS secret, which is the established
/// nest-held-plaintext pattern for deployment-wide mail HMAC secrets; the MTA
/// never needs it because the unsubscribe handlers resolve a token by the
/// cached `mail_list_members.one_click_unsubscribe_token` index, not by
/// re-deriving it MTA-side). The admin only *rotates* it (item #11 — which
/// re-tokenizes every member under the new secret + invalidates in-flight
/// tokens, so a single active secret suffices: no SRS-style overlap window).
/// `getrandom`-seeded, never a CLI/admin-set value (product invariant: nest
/// config from clients, not CLI). Modelled as a multi-row table for uniformity
/// with `mail_srs_secrets`; the newest row is the active secret.
pub(super) const MIGRATIONS_MAIL_LIST_UNSUBSCRIBE_SECRET: &str = "
    CREATE TABLE IF NOT EXISTS mail_list_unsubscribe_secrets (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        secret     BLOB    NOT NULL,
        created_at INTEGER NOT NULL
    );
";

// Per-actor forward holding queue (`mail-forwarding.md` § Queue ceiling
// `:181-185`). A forward that would exceed the per-account hourly rate cap is
// parked here instead of dispatched, then promoted into `outbound_mail_queue`
// at the rate-cap cadence by the N5 promotion step in `fetch_outbound_due`.
// Nest is authoritative, so the queue survives an MTA restart (`:185`).
//
// NOTE on columns vs. the doc: `mail-forwarding.md:185` names a
// `srs_encoded_envelope` column, but the shipped N3 design rewrites SRS at
// queue-out keyed on the `outbound_mail_queue` row id — which does not exist
// until promotion. So a parked forward stores the **original** envelope
// (`original_sender`) + the `raw_message`, and SRS encoding happens after
// promotion exactly like any other forwarded outbound row. The doc was
// reconciled to this shape in the same commit. `id` gives FIFO order
// (newest-evicts-oldest at the ceiling) and a stable promotion cursor.
pub(super) const MIGRATIONS_FORWARD_QUEUE: &str = "
    CREATE TABLE IF NOT EXISTS forward_queue (
        id                     INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id               BLOB    NOT NULL,
        queued_at              INTEGER NOT NULL,
        source_message_id      TEXT    NOT NULL,
        original_sender        TEXT    NOT NULL,
        destination_address    TEXT    NOT NULL,
        rule_id_or_forward_all TEXT    NOT NULL,
        raw_message            BLOB    NOT NULL,
        -- 'copy' | 'redirect' | NULL (unknown: parked before the column
        -- existed) -- the same column and reading as
        -- `outbound_mail_queue.forward_copy_mode` and carried across promotion.
        copy_mode              TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_forward_queue_actor_fifo
        ON forward_queue(actor_id, id);
";

pub(super) const MIGRATIONS_AUTO_REPLY_LOG: &str = "
    CREATE TABLE IF NOT EXISTS auto_reply_log (
        recipient_id BLOB NOT NULL,
        sender_hash  BLOB NOT NULL,
        last_sent_at INTEGER NOT NULL,
        PRIMARY KEY (recipient_id, sender_hash)
    );
";

pub(super) const MIGRATIONS_SUBSCRIPTIONS: &str = "
    CREATE TABLE IF NOT EXISTS subscription_tiers (
        author_id    BLOB NOT NULL,
        name         TEXT NOT NULL,
        rank         INTEGER NOT NULL,
        description  TEXT,
        price_hint   TEXT,
        payment_url  TEXT,
        auto_approve INTEGER NOT NULL DEFAULT 1,
        created_at   INTEGER NOT NULL,
        -- Per-post pay-to-unlock designation (monetization.md § Per-post
        -- pay-to-unlock, Q9): the hex post_id this tier sells access to,
        -- making it a degenerate single-post tier. NULL on every ordinary
        -- tier. Deliberately NO
        -- foreign key: the post is authored AFTER the tier exists (its id is
        -- blake3 over a body naming this tier), and deleting the post leaves
        -- the tier row and its claim audit trail intact.
        unlocks_post TEXT,
        -- The machine-comparable asking price (monetization.md § The asking
        -- price, Q10): an optional UNIT-TAGGED amount, stored as the pair it
        -- is. Both NULL on every tier that is not for sale to an *inferring*
        -- mechanism — the default, and permanently correct rather than a gap.
        -- Two columns, not one: the ratified value is `{value, unit}`, and a
        -- bare msat number is exactly the Lightning special-case the model
        -- forbids. Written and read only as a pair (both set or both NULL);
        -- the unit is stored verbatim even when this build does not know it,
        -- because a newer client may price in a unit that must round-trip
        -- while comparing as not-met, fail-closed.
        asking_price_value INTEGER,
        asking_price_unit  TEXT,
        -- Hidden from every offer surface (monetization.md § The unifying
        -- model — A tier may be hidden): only the author's own tiers.list
        -- carries it. DEFAULT 0 = offered.
        hidden INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (author_id, name)
    );

    CREATE TABLE IF NOT EXISTS subscribers (
        author_id     BLOB NOT NULL,
        subscriber_id BLOB NOT NULL,
        tier_name     TEXT NOT NULL,
        approved_at   INTEGER NOT NULL,
        -- Subscriber's published 1184-byte ML-KEM-768 encapsulation key (the
        -- X-Wing post-quantum half of the broadcast KeyBlob wrap). Nullable
        -- with no default (re-derivable from the
        -- subscriber's identity seed); classical-only subscribers leave it NULL.
        mlkem_encaps_key BLOB,
        -- End of a paid entitlement window, epoch seconds (monetization.md
        -- § Pillar 3: expiry self-heals via valid_until — no revocation list).
        -- NULL = no expiry (manual/auto grants, or a payment with no window).
        -- Enforced at the entitlement
        -- GATES (is_subscriber / get_subscribed_tiers), not the roster reads:
        -- the row stays until the author's client prunes it, but an expired
        -- row no longer entitles key-material serving.
        valid_until INTEGER,
        -- The membership quota pair FROZEN AT ADMISSION (monetization.md
        -- § Pillar 4 Rail C step 3). Non-NULL exactly on a MEMBERSHIP row —
        -- which is also what marks the row a membership rather than an ordinary
        -- content subscription, so the lapse reconcile never re-reads the live
        -- `membership_tiers` link. Deriving from the live link would strand members
        -- above `lapse_tier` forever whenever an admin re-points or clears the
        -- designation: the
        -- policy a member was admitted under is the policy they lapse under.
        -- Both nullable.
        admitted_tier TEXT,
        admitted_lapse_tier TEXT,
        PRIMARY KEY (author_id, subscriber_id, tier_name),
        FOREIGN KEY (author_id, tier_name) REFERENCES subscription_tiers (author_id, name)
    );
    CREATE INDEX IF NOT EXISTS idx_subscribers_author_tier
        ON subscribers (author_id, tier_name);

    CREATE TABLE IF NOT EXISTS subscribe_requests (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        author_id     BLOB NOT NULL,
        subscriber_id BLOB NOT NULL,
        tier_name     TEXT NOT NULL,
        created_at    INTEGER NOT NULL,
        kind          TEXT NOT NULL DEFAULT 'subscribe' CHECK(kind IN ('subscribe', 'unsubscribe')),
        -- Subscriber's published ML-KEM ek, carried from the `subscribe` request
        -- to the `subscribers` row at approval so it survives the enqueue→approve
        -- gap. NULL for unsubscribe rows / classical
        -- subscribers.
        mlkem_encaps_key BLOB,
        -- Verified-payment marker (monetization.md § Pillar 3): 1 = a payment
        -- provider verified this subscriber paid for this tier, so the author's
        -- drain pump approves it without creator judgment (the third grant
        -- source next to manual approval + tier auto_approve).
        payment_entitled INTEGER NOT NULL DEFAULT 0,
        -- The paid window the payment carried (epoch seconds; NULL = none),
        -- stamped onto the subscribers row at approval.
        valid_until INTEGER,
        UNIQUE (author_id, subscriber_id, tier_name, kind),
        FOREIGN KEY (author_id, tier_name) REFERENCES subscription_tiers (author_id, name)
    );
    CREATE INDEX IF NOT EXISTS idx_subscribe_requests_author
        ON subscribe_requests (author_id);

    CREATE TABLE IF NOT EXISTS current_key_blobs (
        author_id    BLOB NOT NULL,
        tier_name    TEXT NOT NULL,
        key_version  INTEGER NOT NULL,
        blob_hash    BLOB NOT NULL,
        blob_data    BLOB NOT NULL DEFAULT x'',
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (author_id, tier_name, key_version),
        FOREIGN KEY (author_id, tier_name) REFERENCES subscription_tiers (author_id, name)
    );
";

/// Payment-provider integration (monetization.md § Pillar 3): per-author
/// provider configs + post-payment claim codes. `payment_providers.tier_name`
/// deliberately carries NO foreign key to `subscription_tiers` — a creator
/// deleting a tier must not be blocked by a provider config; the webhook
/// handler surfaces the dangling mapping as a non-2xx instead.
pub(super) const MIGRATIONS_PAYMENTS: &str = "
    CREATE TABLE IF NOT EXISTS payment_providers (
        author_id      BLOB NOT NULL,
        kind           TEXT NOT NULL,
        -- Webhook-verification secret: verify-only (checks provider webhook
        -- signatures; can neither move money nor read payout accounts).
        webhook_secret TEXT NOT NULL,
        tier_name      TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        last_verified_at INTEGER,
        last_rejected_at INTEGER,
        PRIMARY KEY (author_id, kind)
    );

    CREATE TABLE IF NOT EXISTS payment_claim_codes (
        code         TEXT PRIMARY KEY,
        author_id    BLOB NOT NULL,
        tier_name    TEXT NOT NULL,
        provider     TEXT NOT NULL,
        -- Provider-side payment/event id: the idempotency key (providers
        -- redeliver webhooks) and the audit link to the provider dashboard.
        external_ref TEXT NOT NULL,
        valid_until  INTEGER,
        created_at   INTEGER NOT NULL,
        redeemed_by  BLOB,
        redeemed_at  INTEGER,
        -- Set when a refund/dispute lands before redemption; a voided claim
        -- can no longer be redeemed (rows are kept for audit, never deleted).
        voided_at    INTEGER,
        UNIQUE (author_id, provider, external_ref)
    );
    CREATE INDEX IF NOT EXISTS idx_payment_claim_codes_author
        ON payment_claim_codes (author_id);
";

/// Membership designation (monetization.md § Pillar 4 — paid nest access): the
/// link `(admin actor, subscription tier) → { admin_tier, lapse_tier }` that
/// makes one of an admin's own `subscription_tiers` rows mean *membership of
/// this nest*. `admin_tier` is the quota tier an admitted member is assigned,
/// `lapse_tier` (default `free`) the one a lapsed member degrades to.
///
/// The two tier systems — `subscription_tiers` (the entitlement object) and
/// `tiers` (the resource/quota policy) — stay **distinct concepts joined by
/// this explicit link, never merged**. Per-`(admin, tier)` rather than a
/// singleton so multiple membership tiers compose (bronze/silver/gold, each
/// linked to its own quota tier).
///
/// `admin_tier` / `lapse_tier` DO carry foreign keys to `tiers(name)`: admission
/// literally assigns `users.tier` from the link (`users.tier` declares the same
/// FK), so a designation naming a nonexistent quota tier is a guaranteed future
/// failure — the FK makes it unrepresentable instead of deferring it.
///
/// `(admin_id, tier_name)` deliberately carries **no** foreign key to
/// `subscription_tiers` — the same call `payment_providers` above makes, for the
/// same reason: a payee deleting a tier must not be blocked by a satellite
/// config row (`foreign_keys` is ON, so an FK here would make
/// `delete_subscription_tier` fail). A dangling designation is inert — admission
/// needs an entitlement naming the tier, which cannot exist once the tier is
/// gone — and existence + ownership are enforced at `set` time by the handler.
pub(super) const MIGRATIONS_MEMBERSHIP_TIERS: &str = "
    CREATE TABLE IF NOT EXISTS membership_tiers (
        admin_id   BLOB NOT NULL,
        tier_name  TEXT NOT NULL,
        admin_tier TEXT NOT NULL REFERENCES tiers(name),
        lapse_tier TEXT NOT NULL DEFAULT 'free' REFERENCES tiers(name),
        created_at INTEGER NOT NULL,
        PRIMARY KEY (admin_id, tier_name)
    );
";

pub(super) const MIGRATIONS_EMAIL_DOMAINS: &str = "
    CREATE TABLE IF NOT EXISTS email_domains (
        domain TEXT PRIMARY KEY,
        dkim_selector TEXT NOT NULL DEFAULT 'default',
        dkim_ed25519_selector TEXT NOT NULL DEFAULT 'ed25519',
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS email_domain_users (
        domain TEXT NOT NULL REFERENCES email_domains(domain),
        actor_id BLOB NOT NULL,
        local_part TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (domain, local_part),
        UNIQUE (domain, actor_id)
    );
";

/// Client-set NAT axis singleton, **mutable** — a public↔private flip touches
/// no at-rest data, so the admin panel upserts this row freely (`db/nest_nat_mode.rs` `ON CONFLICT(id) DO UPDATE`). `mode` is the
/// lowercase wire string (`'public'` | `'private'`); `set_at` is unix seconds.
/// Who set it is captured in the audit log (`nest.nat_mode_set`). Absent ⇒ the
/// nest follows the `config.nest.mode` seed (`FAUNA_MODE`); present ⇒ the row
/// wins (`2026-06-15-nest-nat-mode-client-set-design.md` §§ 2-3).
pub(super) const MIGRATIONS_NEST_NAT_MODE: &str = "
    CREATE TABLE IF NOT EXISTS nest_nat_mode (
        id      INTEGER PRIMARY KEY CHECK (id = 1),
        mode    TEXT NOT NULL CHECK (mode IN ('public','private')),
        set_at  INTEGER NOT NULL
    );
";

/// Client-set subhandles singleton (`db/node_policy.rs`), settable both ways
/// (the admin flips it via `fauna.admin.set_subhandles`). Absent ⇒ the nest
/// follows the `config.nest.subhandles` seed; present ⇒ the row wins,
/// boot-resolved into the live `AppState` RwLock field. Who set it is in the
/// audit log (`public-mode.md`: `subhandles` is client-set).
pub(super) const MIGRATIONS_NEST_SUBHANDLES: &str = "
    CREATE TABLE IF NOT EXISTS nest_subhandles (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

/// Client-set registration-posture singleton (`db/node_policy.rs`) — the one knob
/// deciding whether a new account can be created, and how. `mode` is the wire
/// string of [`fauna_protocol::node_policy::RegistrationMode`] (`open` /
/// `invite_required` / `closed`), stored as TEXT rather than an int so the at-rest
/// value stays self-describing and cannot drift with variant order. `max_free_users`
/// is the orthogonal free-tier ceiling (nullable ⇒ no limit), carried on the same
/// row because it is written by the same admin Save.
pub(super) const MIGRATIONS_NEST_REGISTRATION_MODE: &str = "
    CREATE TABLE IF NOT EXISTS nest_registration_mode (
        id              INTEGER PRIMARY KEY CHECK (id = 1),
        mode            TEXT NOT NULL,
        max_free_users  INTEGER,
        set_at          INTEGER NOT NULL
    );
";

/// Client-set "accept only signups carrying app age verification" singleton
/// (`db/node_policy.rs`; `family-safety.md` § The account age band D5+D6,
/// gating scope `public-mode.md` § Age at registration). Same bool-toggle
/// shape as `nest_subhandles`. Deliberately **no config/env seed** — the
/// hard-coded default is off (absent row ⇒ off), and there is no pre-claim
/// moment where the knob matters, so a seed would be configuration-file
/// theatre.
pub(super) const MIGRATIONS_NEST_AGE_VERIFICATION_REQUIRED: &str = "
    CREATE TABLE IF NOT EXISTS nest_age_verification_required (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

/// Client-set node-wide storage cap singleton (`db/node_policy.rs`). The int
/// counterpart of the bool toggles above: `max_bytes` is **nullable** — a
/// present row with `NULL` is an admin who explicitly cleared the cap (no limit),
/// distinct from an absent row (never set ⇒ the nest follows the
/// `config.nest.max_storage_bytes` seed). Settable-both-ways
/// (`fauna.admin.set_max_storage_bytes`); present ⇒ the row wins, boot-resolved
/// into the live `AppState` RwLock field. Per the product invariant that this
/// is client-set, not CLI/env/hand-edited config.
pub(super) const MIGRATIONS_NEST_MAX_STORAGE_BYTES: &str = "
    CREATE TABLE IF NOT EXISTS nest_max_storage_bytes (
        id         INTEGER PRIMARY KEY CHECK (id = 1),
        max_bytes  INTEGER,
        set_at     INTEGER NOT NULL
    );
";

/// Client-set CORS allow-list singleton (`db/node_policy.rs`). The **list**
/// counterpart of the bool/int knobs above: the whole `Vec<String>` of trusted
/// browser origins is stored as one JSON blob on the fixed `id = 1` row
/// (mirrors `db/transport_policy.rs`, itself a no-config-on-disk migration), so
/// an **absent row** is "never set ⇒ follow the `config.nest.cors_origins` seed"
/// and a **present row** (even one holding `[]`) is an admin-set list that wins —
/// the row-presence carries the unset/set-empty distinction a multi-row table
/// could not. Settable-both-ways (`fauna.admin.set_cors_origins`); boot-resolved
/// into the live `AppState.cors_origins` ArcSwap. Per the product invariant
/// that this is client-set, not the `--cors-origins` CLI/env seed.
pub(super) const MIGRATIONS_NEST_CORS_ORIGINS: &str = "
    CREATE TABLE IF NOT EXISTS nest_cors_origins (
        id            INTEGER PRIMARY KEY CHECK (id = 1),
        origins_json  TEXT NOT NULL,
        set_at        INTEGER NOT NULL
    );
";

/// The admin's web-app origin choice singleton (`db/node_policy.rs`) — what
/// this nest's reserved `/app` answers (`web-content-hosting.md` § Same-origin
/// security model → *The nest-served `/app/` and the central origin*): `mode`
/// is `bundled` or `central`. An **absent row** is bundled, the
/// works-out-of-the-box default; there is no seed (the choice is app-set only,
/// `fauna.admin.web_app_origin.set`). Boot-resolved into the live
/// `AppState.web_app_origin` ArcSwap.
pub(super) const MIGRATIONS_NEST_WEB_APP_ORIGIN: &str = "
    CREATE TABLE IF NOT EXISTS nest_web_app_origin (
        id      INTEGER PRIMARY KEY CHECK (id = 1),
        mode    TEXT NOT NULL,
        set_at  INTEGER NOT NULL
    );
";

/// The deployment's own public address(es) (`dns-management.md` § Records
/// covered + § Implementation status today; Slice 2b). Singleton row (id = 1),
/// upserted by the `fauna.dns.set_host_address` onboarding hand-off. The address
/// is **public, not a secret** (unlike DNS-provider credentials, which stay
/// client-held), so it is plaintext nest state in both storage modes. Two
/// address roles because `mail.<primary>` (the MX target) may resolve to a
/// different IP than the apex `<primary>` (the mail server can be a separate
/// box): `nest_ipv4`/`nest_ipv6` → apex `A`/`AAAA`; `mail_ipv4`/`mail_ipv6` →
/// `mail.<primary>` `A`/`AAAA` + the advisory `PTR`. `*_ipv6` is NULL until the
/// deployment has an IPv6 address.
pub(super) const MIGRATIONS_NEST_HOST_ADDRESS: &str = "
    CREATE TABLE IF NOT EXISTS nest_host_address (
        id         INTEGER PRIMARY KEY CHECK (id = 1),
        nest_ipv4  TEXT NOT NULL,
        nest_ipv6  TEXT,
        mail_ipv4  TEXT NOT NULL,
        mail_ipv6  TEXT,
        updated_at INTEGER NOT NULL
    );
";

/// Share-link control-plane registry. A ShareToken is **client-minted, stateless
/// and self-verifying** (`fauna_core::share::ShareToken`, base64url'd into a
/// `/share/{token}` URL); this table registers a minted token's metadata so the
/// author can `fauna.share.list` their live shares and `fauna.share.revoke` one,
/// and `GET /share/{token}` can refuse a revoked token with `410 Gone`. The PK
/// `token_id` is `blake3` of the canonical signed wire bytes (the base64url-
/// decoded `EmbedAsBytes`), so registration is idempotent and the GET path can
/// derive the same id from the URL. `author` is indexed for the per-actor list.
/// Registration is NOT a serving gate — an *unregistered* token still serves
/// statelessly (a client-minted token whose registration failed or was never sent); the row exists only for list/revoke.
/// Spec: `docs/goal/architecture/api-layers.md` § Share.
pub(super) const MIGRATIONS_SHARE_TOKENS: &str = "
    CREATE TABLE IF NOT EXISTS share_tokens (
        token_id      BLOB    PRIMARY KEY,
        author        BLOB    NOT NULL,
        manifest_hash BLOB    NOT NULL,
        expires_at    INTEGER NOT NULL,
        public        INTEGER NOT NULL,
        revoked       INTEGER NOT NULL DEFAULT 0,
        created_at    INTEGER NOT NULL,
        -- SEALED LABEL over the shared file's name: the AUTHOR's share-list
        -- copy and the only form the name rests in. The recipient path is
        -- unaffected because `GET /share/{token}` reads the filename off the
        -- URL-presented signed token and never this row. Random-nonce mode: a
        -- row is keyed by token rather than by name.
        filename_sealed BLOB NOT NULL,
        -- A fragment-keyed private link's KEY ENVELOPE (additive 2026-09-27;
        -- share-links.md § The private-file extension): the per-chunk keys of
        -- the linked file's chunks + their hashes + the name. AEAD-sealed by
        -- the author's client under a link key that rides only the URL
        -- fragment. Opaque to the nest (it never holds that key) and served
        -- verbatim to the link's viewer. NULL on every public link.

        key_envelope BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_share_tokens_author ON share_tokens(author);
";

pub(super) const MIGRATIONS_NEST_SIGNING: &str = "
    CREATE TABLE IF NOT EXISTS nest_keypair (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        secret_key  BLOB NOT NULL,
        public_key  BLOB NOT NULL,
        created_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS device_authorizations (
        author_id   BLOB NOT NULL PRIMARY KEY,
        device_key  BLOB NOT NULL,
        payload     BLOB NOT NULL,
        created_at  INTEGER NOT NULL
    );

    -- The append-only deployment-seed rotation log (`nest/box-recovery.md`
    -- § Deployment-seed rotation). One row per rotation this box has performed,
    -- `seq` starting at 1; the whole ordered set is the **chain** a pinned client
    -- walks from the identity it holds to the identity it was presented with.
    --
    -- ⚠ Rows are NEVER deleted or rewritten. A missing hop is not a smaller
    -- chain, it is an unwalkable one: every client pinned at or before that hop
    -- loses its silent re-pin and falls back to the identity-changed warning. The
    -- log is also what makes a superseded ancestor *recognisable* — the boot
    -- reconcile and the rotate handler both read it to tell a stale on-disk key
    -- the DB already superseded from the established identity, so dropping
    -- rows re-opens the downgrade this ceremony exists to close.
    --
    -- `statement` is the canonical DAG-CBOR `SignedNestRotation`
    -- (`fauna_protocol::nest_rotation`) exactly as served on the wire — stored
    -- whole rather than re-derived, because the signatures are over bytes only
    -- the transaction that held both keys could produce.
    CREATE TABLE IF NOT EXISTS nest_rotation_log (
        seq            INTEGER PRIMARY KEY,
        old_actor_id   BLOB NOT NULL,
        new_actor_id   BLOB NOT NULL,
        statement      BLOB NOT NULL,
        rotated_at     INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_nest_rotation_log_old
        ON nest_rotation_log(old_actor_id);
";

/// The conversation kind's blob-reachability floor
/// (`encryption-at-rest.md` § Per-content-kind conformance → Conversation
/// messages row, ratified 2026-09-08): the plaintext content addresses of the
/// sealed attachment blobs a conv record's sealed body names, listed by the
/// sender beside the envelope (`ChannelSendRequest::attachment_refs`) and
/// written in the same transaction as the record's `segment_records` mirror
/// row. Read by the blob GC alone (step 2g), joined against the mirror on
/// `(channel_id, seq)` for liveness; rows are never deleted (`db/conv_attachment_refs.rs`).
pub(super) const MIGRATIONS_CONV_ATTACHMENT_REFS: &str = "
    CREATE TABLE IF NOT EXISTS conv_attachment_refs (
        channel_id BLOB    NOT NULL,
        seq        INTEGER NOT NULL,
        blob_hash  BLOB    NOT NULL,
        PRIMARY KEY (channel_id, seq, blob_hash)
    );
";

/// The nest-attested author of each conv record
/// (`caldav-server.md` § Who may mutate an existing event over the inbound
/// rail): the actor this nest authenticated on the send that appended the
/// record — same-nest the `channel.send` caller (for the MDA scheduling gateway
/// the `on_behalf_of_actor`-scoped organizer), relayed the
/// `requesting_actor_id` the home bound at `require_foreign_member`. The
/// envelope is sealed, so this is the only place the nest can ever record whose
/// record it was. A side table keyed by `(channel_id, seq)` rather than a
/// `segment_records` column because compaction rebuilds mirror rows from the
/// segment floor, and `seq` is the coordinate that survives it. Written in the
/// same transaction as the mirror row; rows are never deleted (an attestation
/// dropped on a transient inconsistency fails a legitimate cancel closed).
pub(super) const MIGRATIONS_CONV_RECORD_AUTHORS: &str = "
    CREATE TABLE IF NOT EXISTS conv_record_authors (
        channel_id BLOB    NOT NULL,
        seq        INTEGER NOT NULL,
        author     BLOB    NOT NULL,
        PRIMARY KEY (channel_id, seq)
    );
";

/// Blobs withheld from `GET /api/v1/blob/{id}` by a legal takedown.
///
/// A **derived** set, never a floor: it records nothing the box does not
/// already hold, and is rebuilt wholesale from `legal_takedown_ref` joined
/// against the reference sets the blob GC already walks
/// (`crate::moderation_withhold`). Rebuilt by the takedown/restore handler at
/// the instant an admin flips a flag, and again by every complete blob-GC
/// sweep, which is what keeps it true as records come and go. Withheld is
/// **not** deleted: the GC pin is untouched, so a restore re-serves the same
/// bytes (`moderation.md` § Legal takedown → the blob-serve door).
pub(super) const MIGRATIONS_BLOB_LEGAL_WITHHOLD: &str = "
    CREATE TABLE IF NOT EXISTS blob_legal_withhold (
        blob_hash BLOB NOT NULL PRIMARY KEY
    );
";

/// Posts destroyed by their own author **while under a legal takedown**
/// — the one place the compelled fact survives the row it sat on.
///
/// Unlike [`MIGRATIONS_BLOB_LEGAL_WITHHOLD`] beside it this **is** a floor, and
/// deliberately: every post-side withhold keys on
/// `content_meta.legal_takedown_ref`, and the author's delete removes exactly
/// that row, so after it the box holds nothing to recompute from. The
/// obligation ledger cannot stand in — `post_legal_takedown_txn` writes a
/// `TakenDown` row only for a takedown and an overturn writes no counter-row,
/// so *taken down → deleted* and *taken down → restored → deleted* read
/// identically there, and keying the withhold to it would withhold a whole
/// segment pair forever for a post that was legitimately restored first.
///
/// `blob_digests` is the concatenation of the 32-byte digests the record named
/// (`Post::blob_refs`), captured in the delete because
/// `segments::post::load_post_body` answers `None` for a tombstoned record —
/// so this is the last moment they are readable, and the blob door's rebuild
/// has no other source for them.
///
/// Rows are permanent: a takedown is a legal fact, the table is tiny, and a
/// row whose bytes compaction and the GC have since reclaimed is simply inert.
pub(super) const MIGRATIONS_LEGAL_TAKEDOWN_DELETED_POSTS: &str = "
    CREATE TABLE IF NOT EXISTS legal_takedown_deleted_posts (
        post_id         BLOB NOT NULL PRIMARY KEY,
        author_id       BLOB NOT NULL,
        legal_reference TEXT NOT NULL,
        blob_digests    BLOB NOT NULL,
        deleted_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_legal_takedown_deleted_posts_author
        ON legal_takedown_deleted_posts (author_id);
";

pub(super) const MIGRATIONS_ACTOR_CHANNELS: &str = "
    CREATE TABLE IF NOT EXISTS actor_channels (
        actor_id   BLOB NOT NULL,
        channel_id BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (actor_id, channel_id)
    );
    CREATE INDEX IF NOT EXISTS idx_ac_actor ON actor_channels(actor_id);

    -- Foreign (cross-nest) channel members: an actor on a peer nest that THIS nest
    -- (the channel's home) relayed an MLS Welcome to. `home_nest_id` is the
    -- member's home nest, recorded so a `fauna.federation.channel.fetch` from that
    -- nest can be authorized to pull the channel's application messages (the
    -- open-federation cross-nest message delivery for unpaired nests;
    -- direct-messages.md § Technical Flow — Cross-Nest, step 3).
    CREATE TABLE IF NOT EXISTS channel_foreign_members (
        channel_id   BLOB NOT NULL,
        actor_id     BLOB NOT NULL,
        home_nest_id BLOB NOT NULL,
        created_at   INTEGER NOT NULL,
        nest_url TEXT,
        confirmed_at INTEGER,
        handle TEXT,
        handle_domain TEXT,
        PRIMARY KEY (channel_id, actor_id)
    );
";

/// Per-channel MLS **commit high-water mark** — the highest conv `seq` at which
/// a `ChannelEnvelope::Commit` record has landed on the channel. Backs the
/// device-owned-epoch commit gate (`fauna.conversations.channel.send`'s
/// `expect_no_commit_since` precondition; `docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync): `∃ commit with seq > N ⟺
/// last_commit_seq > N`, since commits are monotonic in `seq`. The mark is set
/// at append time under the per-channel conv seq lock and only ever advances
/// (`MAX`), so it is robust to record tombstoning/compaction — a
/// compacted-away commit still blocks a stale sender, where a live-record scan
/// would silently miss it. Derived state (recomputable by scanning the channel
/// log for `Commit` envelopes) ⇒ re-creatable, but never dropped in normal
/// operation (no-user-data-loss: it is not the user's data, only an index).
///
/// `last_commit_sender` is the actor this nest observed sending the commit that
/// set the mark — the **authorship** half of the floor roster's commit-order
/// guard (`conversation-rooms.md` § The floor roster). The guard orders a
/// mirror's reports by the position of the commit each follows; without the
/// sender it asks only *which* commit a report names, never *whose* it was, so
/// the newest position — the one the committing device's own honest report is
/// about to claim — was claimable by any live member, the member that commit
/// just removed included. Written under the same per-channel conv seq
/// lock as the mark and only when the mark actually advances, so the pair never
/// splits. Nullable: a NULL sender is unknown, and a position whose sender is
/// unknown is admitted on its position alone.
pub(super) const MIGRATIONS_CHANNEL_COMMIT_WATERMARK: &str = "
    CREATE TABLE IF NOT EXISTS channel_commit_watermark (
        channel_id         BLOB PRIMARY KEY,
        last_commit_seq    INTEGER NOT NULL,
        last_commit_sender BLOB
    );
";

pub(super) const MIGRATIONS_SENDER_BEHAVIOR: &str = "
    CREATE TABLE IF NOT EXISTS sender_behavior (
        actor_id       BLOB NOT NULL,
        event_type     TEXT NOT NULL,
        target_actor   BLOB,
        timestamp      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_sender_behavior_actor
        ON sender_behavior(actor_id, timestamp DESC);
";

pub(super) const MIGRATIONS_BACKUP_SNAPSHOTS: &str = crate::backup::SCHEMA_BACKUP_SNAPSHOTS;

pub(super) const MIGRATIONS_ADMIN_ACTOR_IDS: &str = "
    CREATE TABLE IF NOT EXISTS admin_actor_ids (
        actor_id BLOB PRIMARY KEY,
        added_at INTEGER NOT NULL,
        role TEXT NOT NULL DEFAULT 'superadmin'
    );
";

pub(super) const MIGRATIONS_SYNC_CONFLICTS: &str = "
    CREATE TABLE IF NOT EXISTS sync_conflicts (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        device_id   BLOB NOT NULL,
        path        TEXT NOT NULL,
        conflict_type TEXT NOT NULL,
        details     TEXT,
        created_at  INTEGER NOT NULL,
        resolved_at INTEGER,
        -- HASH COMPANION of `path` via `fauna_core::sync::path_hash` --
        -- stamped at insert (the client's own salt when it sent one, else
        -- derived from the plaintext). The conflict-winner propagation reads
        -- it instead of re-deriving from plaintext.
        path_hash   BLOB NOT NULL,
        -- SEALED LABEL over `path` -- convergent under `path_hash`.
        path_sealed    BLOB,
        -- SEALED LABEL over the free-text `details` -- random-nonce mode
        -- because `details` is mutable under that same salt.
        details_sealed BLOB,
        winning_manifest_hash BLOB,
        resolution TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_sync_conflicts_fs ON sync_conflicts(folder_id, resolved_at);
";

// Rich conflict resolution (Slice 2): a
// conflict carries the diverging candidate versions the user may choose
// between, and the chosen winner is recorded on the conflict row. See
// `docs/goal/behavior/file-sync.md` § Conflicts.
pub(super) const MIGRATIONS_SYNC_CONFLICT_CANDIDATES: &str = "
    CREATE TABLE IF NOT EXISTS sync_conflict_candidates (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        conflict_id   INTEGER NOT NULL REFERENCES sync_conflicts(id) ON DELETE CASCADE,
        manifest_hash BLOB NOT NULL,
        device_id     BLOB NOT NULL,
        size_bytes    INTEGER NOT NULL,
        created_at    INTEGER NOT NULL,
        content_key_version INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_sync_conflict_candidates_cid ON sync_conflict_candidates(conflict_id);
";

pub(super) const MIGRATIONS_ACTOR_LAST_IP: &str = "
    CREATE TABLE IF NOT EXISTS actor_last_ip (
        actor_id    BLOB PRIMARY KEY,
        ip_address  TEXT NOT NULL,
        updated_at  INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_PENDING_ACTIONS: &str = "
    CREATE TABLE IF NOT EXISTS pending_actions (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        action_type     TEXT NOT NULL,
        actor_id        BLOB NOT NULL,
        target          TEXT,
        payload         TEXT,
        status          TEXT NOT NULL DEFAULT 'pending',
        created_at      INTEGER NOT NULL,
        execute_after   INTEGER NOT NULL,
        executed_at     INTEGER,
        cancelled_by    BLOB,
        cancelled_at    INTEGER,
        requires_quorum INTEGER NOT NULL DEFAULT 0,
        approvals       TEXT NOT NULL DEFAULT '[]',
        ip_address      TEXT,
        chain_hash      BLOB,
        chain_hash_version INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_pending_actions_status ON pending_actions (status, execute_after);
    CREATE INDEX IF NOT EXISTS idx_pending_actions_actor ON pending_actions (actor_id, status);
";

pub(super) const MIGRATIONS_NOTIFICATIONS: &str = "
    CREATE TABLE IF NOT EXISTS notifications (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id    BLOB NOT NULL,
        notif_type  TEXT NOT NULL,
        source      TEXT NOT NULL DEFAULT 'fauna',
        sender_id   BLOB,
        content_id  BLOB,
        subject_uri TEXT,
        summary     TEXT NOT NULL,
        is_read     INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL,
        body_key    TEXT,
        body_args   TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_notif_actor
        ON notifications(actor_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_notif_actor_unread
        ON notifications(actor_id, is_read) WHERE is_read = 0;
    CREATE INDEX IF NOT EXISTS idx_notif_dedup
        ON notifications(actor_id, notif_type, sender_id, content_id);
";

pub(super) const MIGRATIONS_PUSH_SUBSCRIPTIONS: &str = "
    CREATE TABLE IF NOT EXISTS push_subscriptions (
        id          INTEGER PRIMARY KEY,
        actor_id    BLOB NOT NULL,
        device_id   TEXT NOT NULL,
        transport   TEXT NOT NULL,
        endpoint    TEXT NOT NULL,
        key_p256dh  TEXT,
        key_auth    TEXT,
        created_at  INTEGER NOT NULL,
        UNIQUE(actor_id, device_id)
    );
    CREATE INDEX IF NOT EXISTS idx_push_subs_actor
        ON push_subscriptions(actor_id);
";

// Self-generated VAPID keypair (`apps/common.md` § Push Notifications — "target:
// self-generated per nest"): pure nest infrastructure no user/admin ever chooses,
// so it is generated once on first boot and persisted here rather than taken as a
// CLI flag (`nest/common.md` § CLI Flags → Web Push, "Recorded invariant
// violation"). Singleton row, same `id = 1` shape as `nest_keypair` above.
pub(super) const MIGRATIONS_VAPID_KEYPAIR: &str = "
    CREATE TABLE IF NOT EXISTS vapid_keypair (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        pem         BLOB NOT NULL,
        created_at  INTEGER NOT NULL
    );
";

// The nest's **replica id** (`account-sync-plane.md` § The bind leg, ruling
// 2): a random 16-byte id minted once, with this database, by
// [`seed_genesis_rows`], and returned on the account-state feed reply so a
// device keys what it remembers about "the nest" by the replica it learned it
// from. Deliberately NOT derivable from the deployment seed and NOT
// recreatable: a box rebuilt over an empty database under the same identity
// must read as a different replica. Singleton row, same `id = 1` shape as
// `nest_keypair`.
pub(super) const MIGRATIONS_NEST_REPLICA: &str = "
    CREATE TABLE IF NOT EXISTS nest_replica (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        replica_id  BLOB NOT NULL CHECK (length(replica_id) = 16),
        created_at  INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_BRIDGE_ABSORPTION: &str = "
    -- Service-user enrollment. One row per bridge process keypair.
    -- status='pending'  → admin has registered the keypair via HTTP but
    --                     not approved it; bridge cannot call any kind
    --                     except register_service_user.
    -- status='approved' → bridge can call kinds permitted by its role.
    -- status='revoked'  → bridge cannot call any kind.
    CREATE TABLE IF NOT EXISTS bridge_service_users (
        ed25519_pubkey   BLOB PRIMARY KEY,
        x25519_pubkey    BLOB,
        -- `mlkem_ek` (post-quantum PQ-CAP-2) holds the holder's 1184-byte
        -- ML-KEM-768 encapsulation key, derived from the bridge's Ed25519
        -- identity seed (`fauna.bridge.service-user-mlkem.v1`) and published at
        -- register_service_user alongside x25519_pubkey. The client mint seals a
        -- capability grant X-Wing to `from_parts(mlkem_ek, x25519_pubkey)` when
        -- present; NULL = a classical-only bridge → the mint
        -- degrades to the classical wrap.
        mlkem_ek         BLOB,
        -- 'content-processor' is the generic holder role for user-minted
        -- capability grants (design § 2.4); 'atproto.pds' is the out-of-process
        -- ATProto PDS host bridge (atproto-pds-bridge.md). The fine-grained
        -- boundary is the wrapped-key scope / method allowlist, not the role.
        role             TEXT NOT NULL CHECK(role IN ('mta', 'mda', 'content-processor', 'atproto.pds')),
        bridge_id        TEXT NOT NULL,
        -- 1 = an IN-PROCESS holder: its secret key material lives inside the
        -- nest process itself (today: the web-serve paywall holder,
        -- `web_content::holder` — seed file in the nest data dir). Such a
        -- holder is a legitimate grant target for its own ratified readable
        -- class (web-paywalled content), but it must NEVER be resolved as a
        -- seal target for content that must rest nest-opaque — a copy sealed
        -- to it rests beside its own key (`encryption-at-rest.md` § Don't do
        -- these) — and it never dials in over WS, so pokes addressed to it are
        -- always dropped. Set by the in-process holder's own self-enrollment
        -- on every boot (keyed on its actual pubkey, not its bridge_id); every
        -- future in-process holder must mark its row the same way. External
        -- enrollment paths (WS register_service_user, admin HTTP pre-register)
        -- never set it — a dialing-in bridge is off-box by construction.
        in_process       INTEGER NOT NULL DEFAULT 0,
        status           TEXT NOT NULL CHECK(status IN ('pending', 'approved', 'revoked')),
        created_at       INTEGER NOT NULL,
        approved_at      INTEGER,
        revoked_at       INTEGER,
        -- The ADMIN who approved this registration, as an AUDIT fact — and
        -- deliberately NOT a foreign key to `admin_actor_ids`.
        --
        -- ⚠ The FK made an *audit* column depend on a *live authority* row, and
        -- admin rows are legitimately deleted: `remove_admin_actor` de-admins
        -- someone, and `record_succession`'s admin leg is a delete-then-insert.
        -- With `PRAGMA foreign_keys = ON` (`db/mod.rs`) neither is a no-op — both
        -- abort. So an admin who had approved any bridge could not be de-admined
        -- and, far worse, **could not succeed their identity at all**: the one
        -- ceremony that ends a key compromise was denied to the box's most
        -- privileged accounts, and the whole transaction rolled back.
        -- (`successions::tests::an_admin_who_approved_a_bridge_can_still_succeed`
        -- and `admin::tests::de_admining_an_approver_does_not_abort` pin both.)
        --
        -- The recorded value stays as written — the retired identity genuinely
        -- did approve that bridge, and `actor_successions` links it forward for
        -- anyone who needs to resolve who that is today
        -- (`actor_tables::SUCCESSION_REFERENCES` carries the ruling). Nothing
        -- resolves *through* this pointer: the bridge's authority is its `status`
        -- column plus `bridge_method_allowlist`, never its approver.
        approved_by_actor_id BLOB,
        -- The bridge's last CONFINEMENT SELF-PROBE (security.md § Co-resident
        -- process trust boundary → Confinement self-probe): what the bridge
        -- process observed about its own sandbox at startup, reported on the
        -- register_service_user call it already makes every cold boot. These
        -- answer whether this DEPLOYED box is actually isolated — a question
        -- that previously needed an SSH session, which a provisioned box has
        -- no key for (testing.md § Gap 3).
        --
        -- ⚠ PROVISIONING DIAGNOSTICS, never an attestation: a compromised
        -- bridge reports whatever it likes. Nothing may gate a security
        -- decision on them. Projected to admin-class callers only.
        --
        -- ⚠ Deliberately NOT set-once, unlike x25519_pubkey / mlkem_ek beside
        -- them. Those freeze because a changed value means an attacker
        -- redirecting a seal target; these describe the *currently running*
        -- process, so each boot must overwrite the last — freezing them would
        -- pin a report from an image that is no longer deployed. Do not copy
        -- the freeze pattern here.
        --
        -- confinement_uid          — getuid() of the bridge process (slice 1).
        -- confinement_sealed_store — its own attempted read of nest.db:
        --                            'denied' / 'readable' / 'absent' / 'unknown'.
        -- confinement_landlock     — 'fully' / 'partial' / 'off' / 'unknown',
        --                            relayed from the fauna-sandbox wrapper.
        -- confinement_seccomp      — 'filter' / 'strict' / 'off' / 'unknown'.
        -- confinement_reported_at  — epoch-millis of the report, so a stale one
        --                            reads as stale instead of as current fact.
        --
        -- All nullable. NULL = no probe reported yet (a bridge that has never
        -- connected).
        confinement_uid          INTEGER,
        confinement_sealed_store TEXT,
        confinement_landlock     TEXT,
        confinement_seccomp      TEXT,
        confinement_reported_at  INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_su_role_bridge_id
        ON bridge_service_users(role, bridge_id) WHERE status != 'revoked';

    -- Per-(actor_id, credential_id) wrapped MLS-key blob.
    CREATE TABLE IF NOT EXISTS bridge_wrapped_mls_blobs (
        actor_id      BLOB NOT NULL,
        credential_id TEXT NOT NULL,
        blob          BLOB NOT NULL,
        created_at    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, credential_id)
    );

    -- Per-actor MLS state snapshot. Single row per actor.
    CREATE TABLE IF NOT EXISTS bridge_mls_snapshot_blobs (
        actor_id   BLOB PRIMARY KEY,
        blob       BLOB NOT NULL,
        created_at INTEGER NOT NULL
    );

    -- Per-actor WebDAV served-set key blob (webdav-server.md § Key model).
    -- The MSEK-sealed sibling of bridge_mls_snapshot_blobs: opaque ciphertext
    -- (fauna_mls::wrapped_blob::WebdavKeysBlob) carrying the content keys of the
    -- sets the user flagged for WebDAV serving. Single row per actor; nest never
    -- decodes the blob.
    CREATE TABLE IF NOT EXISTS bridge_webdav_keys_blobs (
        actor_id   BLOB PRIMARY KEY,
        blob       BLOB NOT NULL,
        created_at INTEGER NOT NULL
    );

    -- Per-(actor_id, credential_id) submission-token wrapped blob.
    CREATE TABLE IF NOT EXISTS bridge_wrapped_submission_tokens (
        actor_id      BLOB NOT NULL,
        credential_id TEXT NOT NULL,
        blob          BLOB NOT NULL,
        created_at    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, credential_id)
    );

    -- Per-(bridge_role, bridge_id, domain) TLS certificate wrapped blob.
    CREATE TABLE IF NOT EXISTS bridge_tls_cert_blobs (
        bridge_role TEXT NOT NULL,
        bridge_id   TEXT NOT NULL,
        domain      TEXT NOT NULL,
        blob        BLOB NOT NULL,
        created_at  INTEGER NOT NULL,
        PRIMARY KEY (bridge_role, bridge_id, domain)
    );


    -- Per-actor MLS public encryption key. The MTA bridge fetches this
    -- via `fauna.bridges.fetch_recipient_mls_pubkey` to encrypt inbound
    -- mail bodies to the recipient at the perimeter, so nest never sees
    -- plaintext. The pubkey is provisioned by the recipient's own client
    -- out-of-band (see I3 for the upload path).
    --
    -- `mlkem_ek` (post-quantum S3c) holds the recipient's 1184-byte
    -- ML-KEM-768 encapsulation key, MSEK-derived alongside the X25519
    -- `mls_pubkey`. Required: the provision door writes both halves in one
    -- statement, so every recipient's mail is sealed under the hybrid suite.
    CREATE TABLE IF NOT EXISTS actor_mls_pubkeys (
        actor_id    BLOB PRIMARY KEY,
        mls_pubkey  BLOB NOT NULL,
        updated_at  INTEGER NOT NULL,
        mlkem_ek    BLOB NOT NULL
    );

    -- Per-actor index public encryption key. Sibling of actor_mls_pubkeys:
    -- the MTA bridge fetches this via
    -- `fauna.bridges.fetch_recipient_index_key` to encrypt the inbound
    -- message's canonical-token-set index hint at the perimeter. Held
    -- separately from the MLS pubkey so a future deployment can scope
    -- the index-builder's read access to just the index hint set without
    -- granting body-read capability. Provisioning is a Phase E concern
    -- (no production RPC yet — tests pre-seed via `put_actor_index_pubkey`).
    CREATE TABLE IF NOT EXISTS actor_index_pubkeys (
        actor_id     BLOB PRIMARY KEY,
        index_pubkey BLOB NOT NULL,
        updated_at   INTEGER NOT NULL
    );

    -- Per-actor, per-epoch mail sealing PUBLIC keys (content-sealing epochs
    -- design 2026-07-18 § 3). The owner's client pre-publishes a
    -- horizon of future weekly epochs' public halves (additive epoch_keys
    -- field on provision_recipient_mls_pubkey, capability-gated on
    -- 'mail-epoch-schedule') so the MTA can epoch-seal inbound mail with no
    -- client online. Public keys only — floor-safe; nothing here opens
    -- content. The D2 resolver prefers the row covering the current epoch,
    -- degrades to the newest published earlier row, then to the standing
    -- actor_mls_pubkeys key (never bounces mail) — selection is inert until
    -- the MAIL_EPOCH_SEALING_WRITE_DEFAULT flip. Rows for long-past epochs
    -- are prunable but harmless.
    CREATE TABLE IF NOT EXISTS actor_epoch_seal_keys (
        actor_id     BLOB NOT NULL,
        epoch        INTEGER NOT NULL,
        mls_pubkey   BLOB NOT NULL,
        mlkem_ek     BLOB NOT NULL,
        published_at INTEGER NOT NULL,
        PRIMARY KEY (actor_id, epoch)
    );

    -- Bridge-reported audit events. Append-only modulo idempotency:
    -- duplicate (bridge, actor, credential, result, source_ip, occurred_at,
    -- reason) tuples are collapsed via the UNIQUE idempotency_hash so a
    -- flaky bridge retrying report_auth_event doesn't multiply rows.
    CREATE TABLE IF NOT EXISTS bridge_audit_events (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        received_at       INTEGER NOT NULL,
        bridge_actor_id   BLOB NOT NULL,
        actor_id          BLOB NOT NULL,
        credential_id     TEXT NOT NULL,
        result            TEXT NOT NULL,
        source_ip         TEXT NOT NULL,
        occurred_at       INTEGER NOT NULL,
        reason            TEXT,
        idempotency_hash  BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_audit_actor
        ON bridge_audit_events(actor_id, received_at DESC);
    CREATE INDEX IF NOT EXISTS idx_bridge_audit_bridge
        ON bridge_audit_events(bridge_actor_id, received_at DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_audit_idempotency
        ON bridge_audit_events(idempotency_hash)
        WHERE idempotency_hash IS NOT NULL;

    -- Bridge-reported session-close events. Same idempotency model as
    -- bridge_audit_events: partial UNIQUE on a SHA-256 of the natural
    -- key tuple so retries collapse cleanly.
    CREATE TABLE IF NOT EXISTS bridge_session_close_events (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        received_at       INTEGER NOT NULL,
        bridge_actor_id   BLOB NOT NULL,
        actor_id          BLOB NOT NULL,
        credential_id     TEXT NOT NULL,
        reason            TEXT NOT NULL,
        occurred_at       INTEGER NOT NULL,
        idempotency_hash  BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_session_close_actor
        ON bridge_session_close_events(actor_id, received_at DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_session_close_idempotency
        ON bridge_session_close_events(idempotency_hash)
        WHERE idempotency_hash IS NOT NULL;

    -- Per-actor daily submission counter for `fauna.bridges.check_submission_quota`.
    -- day_bucket = epoch_secs / 86_400. Atomic read+upsert under the SQLite
    -- mutex; no transaction needed because the connection is single-threaded.
    CREATE TABLE IF NOT EXISTS bridge_submission_quota (
        actor_id   BLOB NOT NULL,
        day_bucket INTEGER NOT NULL,
        used       INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (actor_id, day_bucket)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_submission_quota_day
        ON bridge_submission_quota(day_bucket);

    -- Per-record floor mirror for the message-segment-store
    -- (`docs/goal/architecture/message-segment-store.md` § `segment_records`
    -- SQLite mirror; spec D3). Authoritative copy of the per-record floor
    -- lives in the segment file's footer; this mirror is the SQL-queryable
    -- index. Rebuildable from segment files during recovery.
    CREATE TABLE IF NOT EXISTS segment_records (
        -- The record's audience scope: the owning actor for mail and the
        -- channel id for conv (`message-segment-store.md` § segment_records).
        scope_id     BLOB    NOT NULL,
        kind         TEXT    NOT NULL,
        segment_id   INTEGER NOT NULL,
        -- 36-byte `fauna_cbor::Cid` (v1 + dag-cbor 0x71 + blake3-256 + 32-byte
        -- digest). The CARv2 index in the `.dat` keys records by this full Cid;
        -- the mirror stores it verbatim so reads probe the index without
        -- reconstructing it (`message-segment-store.md` § segment_records). The
        -- IMAP `message_id` stays the 32-byte digest tail on the Go-bridge wire
        -- — joins compare `substr(record_cid, 5) = message_id`.
        record_cid   BLOB    NOT NULL,
        bucket       TEXT    NOT NULL,
        -- No byte_offset / byte_length columns: random-access reads address a
        -- record by its Cid through the CARv2 MultihashIndexSorted index, and
        -- IMAP RFC822.SIZE / SEARCH / quota derive the record's block length
        -- from that same index by `record_cid` — never a mirrored offset or
        -- byte column (`message-segment-store.md` § segment_records).
        tombstoned   INTEGER NOT NULL DEFAULT 0,
        -- The content scope's change cursor — the coordinate the
        -- generalized account-data feed's class-1 arm pages on
        -- (`account-sync-plane.md` § Feeds and cursors; the arm lives in
        -- `sync_handlers.rs`, the assignment in `records_db::next_changed_seq`).
        -- Monotonic within one `(scope_id, kind)` scope, assigned on append and
        -- **re-assigned on tombstone**: a replica's frontier walk only ever sees
        -- what moved past its cursor, so a delete that left this untouched would
        -- be invisible to every replica that had already walked past the record.
        -- `0` means never-assigned — a compaction survivor is re-inserted at
        -- `0` — and `backfill_segment_records_changed_seq` gives such rows
        -- ordinals at the top of their scope on the next boot.
        changed_seq  INTEGER NOT NULL DEFAULT 0,
        -- Mail-kind sparse floor (NULL for other kinds).
        received_at  INTEGER,
        sender_dom   TEXT,
        spam_disp    TEXT,
        is_own_submission INTEGER,
        -- Conversation per-channel sequence (Plan 7); NULL for non-conv
        -- kinds. Calendar / post columns are added by their respective
        -- rollout plans (Plan N); leave the table extensible via ALTER.
        seq          INTEGER,
        -- Legal-obligation takedown flag for conv-kind records (the
        -- conversation twin of `content_meta.legal_takedown_ref`;
        -- moderation.md § Categories & enforcement item 1 /
        -- content-moderation-and-ranking.md Q5). NULL = live; non-NULL =
        -- taken down under legal compulsion, the value being the legal-
        -- obligation *reference* the tombstone cites. Keys on the record's
        -- `record_cid` only (no body inspection → works on the sealed E2E
        -- conv envelope the nest cannot read). A mirror-only
        -- mutable flag exactly like `tombstoned`. Best-effort at the relay:
        -- withholds future per-record fetches, cannot recall already-delivered
        -- content or reach opaque client-sealed snapshot blobs.
        legal_takedown_ref TEXT,
        -- Canonical 32-byte report-hash for distributed report sharing
        -- (report-sharing.md § Content identity). Mail-kind sparse: NULL for
        -- other kinds. The segment footer
        -- (MailFloorMetadata.report_hash) is authoritative; this column is
        -- the hot-path lookup by message id.
        report_hash BLOB,
        -- Continuation-record role (message-segment-store.md § Continuation
        -- records): 0 = ordinary inline record, 1 = a continuation PART (a raw
        -- ciphertext range; no placement/projection row; reaped when headless),
        -- 2 = a continuation HEAD (envelope v3, pins the ordered part-CID list;
        -- placed + served like a normal record). Mirrors
        -- `MailFloorMetadata.continuation_role` (the segment footer is
        -- authoritative; this column is the SQL-queryable copy the
        -- headless-part reaper filters on).
        continuation_role INTEGER NOT NULL DEFAULT 0,
        -- When THIS nest stored the record (epoch ms), as opposed to when the
        -- message was received (`received_at`, forwarded verbatim by the relay
        -- because it drives INTERNALDATE + the co-location bucket). Mirrors
        -- `MailFloorMetadata.stored_at` (the segment footer is authoritative;
        -- this column is the SQL-queryable copy the headless-part reaper's grace
        -- keys on). NULL = unknown (a floor whose `stored_at` is 0): the reaper
        -- treats NULL as NOT-reapable so such a row can only ever leak a part
        -- and never lose one. Mail-kind sparse like the rest of the floor
        -- columns.
        stored_at    INTEGER,
        PRIMARY KEY (scope_id, kind, segment_id, record_cid)
    );
    CREATE INDEX IF NOT EXISTS idx_segment_records_scope_kind_bucket
        ON segment_records(scope_id, kind, bucket);
    CREATE INDEX IF NOT EXISTS idx_segment_records_record_cid
        ON segment_records(record_cid, kind);

    -- Per-message IMAP placement. One row per (actor, mailbox, uid). A message
    -- (segment_records.record_cid, kind='mail') may have several placements
    -- (COPY duplicates; ingest creates one). flags is a space-separated list of IMAP flag tokens
    -- (\\Seen \\Answered \\Flagged \\Deleted \\Draft + arbitrary keywords); empty = none.
    -- The four *_norm columns are case-folded substring indexes for the
    -- IMAP SEARCH header axes (imap-server.md § SEARCH). In encrypted mode
    -- only from_norm is populated (from public_metadata.sender_domain);
    -- subject/to/cc remain empty and degrade to no-match per § SEARCH.
    -- Each column is capped at 1024 bytes by the writer.
    CREATE TABLE IF NOT EXISTS bridge_imap_messages (
        actor_id      BLOB NOT NULL,
        mailbox       TEXT NOT NULL,
        uid           INTEGER NOT NULL,
        message_id    BLOB NOT NULL,
        flags         TEXT NOT NULL DEFAULT '',
        modseq        INTEGER NOT NULL DEFAULT 1,
        internal_date INTEGER NOT NULL,
        created_at    INTEGER NOT NULL,
        from_norm     TEXT NOT NULL DEFAULT '',
        to_norm       TEXT NOT NULL DEFAULT '',
        cc_norm       TEXT NOT NULL DEFAULT '',
        subject_norm  TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (actor_id, mailbox, uid)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_imap_msgs_msgid
        ON bridge_imap_messages(actor_id, message_id);
    CREATE INDEX IF NOT EXISTS idx_bridge_imap_msgs_modseq
        ON bridge_imap_messages(actor_id, mailbox, modseq);

    -- Per-(actor, mailbox) IMAP collection state. uid_next = next UID to hand out;
    -- highestmodseq = max modseq ever used in this mailbox.
    CREATE TABLE IF NOT EXISTS bridge_imap_mailbox_state (
        actor_id      BLOB NOT NULL,
        mailbox       TEXT NOT NULL,
        uid_validity  INTEGER NOT NULL DEFAULT 1,
        uid_next      INTEGER NOT NULL DEFAULT 1,
        highestmodseq INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (actor_id, mailbox)
    );

    -- Expunged-UID log for CONDSTORE/QRESYNC VANISHED responses (RFC 7162).
    CREATE TABLE IF NOT EXISTS bridge_imap_expunged (
        actor_id    BLOB NOT NULL,
        mailbox     TEXT NOT NULL,
        uid         INTEGER NOT NULL,
        modseq      INTEGER NOT NULL,
        expunged_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_imap_expunged_modseq
        ON bridge_imap_expunged(actor_id, mailbox, modseq);

    -- Per-(actor, mailbox) IMAP subscription set (RFC 9051 §6.3.7 /
    -- §6.3.8). SUBSCRIBE inserts, UNSUBSCRIBE deletes; both are
    -- idempotent on PK conflict. Subscriptions are decoupled from
    -- mailbox existence per RFC 9051 §6.3.7: a client may SUBSCRIBE
    -- a mailbox that does not (yet) exist or that has been DELETEd,
    -- and the subscription persists across CREATE/DELETE. LSUB and
    -- LIST (SUBSCRIBED) join through this table.
    CREATE TABLE IF NOT EXISTS bridge_imap_subscriptions (
        actor_id BLOB NOT NULL,
        mailbox  TEXT NOT NULL,
        PRIMARY KEY (actor_id, mailbox)
    );

    -- Per-calendar CalDAV collection state (encrypted-mode bridge). metadata
    -- (name, color, timezone, visibility, ...) is sealed under the owner's
    -- MLS read key — nest never reads it. ctag and highestmodseq are the
    -- plaintext-floor sync counters; they bump on every PUT / DELETE into
    -- the calendar.
    CREATE TABLE IF NOT EXISTS bridge_caldav_calendars (
        actor_id            BLOB NOT NULL,
        calendar_id         BLOB NOT NULL,
        encrypted_metadata  BLOB NOT NULL,
        ctag                INTEGER NOT NULL DEFAULT 0,
        highestmodseq       INTEGER NOT NULL DEFAULT 1,
        created_at          INTEGER NOT NULL,
        PRIMARY KEY (actor_id, calendar_id)
    );

    -- Per-event CalDAV placement (encrypted-mode bridge). The sealed iCalendar
    -- VEVENT rests in the actor's `__calendar` segment, addressed by record_cid;
    -- uid_hash = blake3(plaintext_iCalendar_UID) floors only the lookup index.
    -- event_id = blake3(
    -- \"fauna.bridges.put_event_ciphertext.v1\" || actor || timestamp_le_i64
    -- || encrypted_body) — deterministic so transport-retry collapses on PK.
    -- etag = format!(\"{:016x}\", modseq); served upstream to the MUA for If-Match.
    -- encrypted_fauna_ext: nullable sealed Fauna-extension sidecar (the
    -- `interested` RSVP refinement + per-attendee nest-url hints). NULL when
    -- the row was last written by a MUA (no sidecar);
    -- never served to a CalDAV MUA — only Fauna apps read it
    -- (caldav-server.md § Event resources). A MUA PUT (sidecar absent) on an
    -- UPDATE preserves the prior sidecar; a Fauna write replaces both halves.
    CREATE TABLE IF NOT EXISTS bridge_caldav_events (
        actor_id              BLOB NOT NULL,
        calendar_id           BLOB NOT NULL,
        event_id              BLOB NOT NULL,
        uid_hash              BLOB NOT NULL,
        encrypted_index_hint  BLOB NOT NULL,
        etag                  TEXT NOT NULL,
        modseq                INTEGER NOT NULL DEFAULT 1,
        ciphertext_size       INTEGER NOT NULL,
        internal_date         INTEGER NOT NULL,
        created_at            INTEGER NOT NULL,
        encrypted_fauna_ext   BLOB,
        -- The record's content-hash filing CID (36-byte Cid; message-segment-
        -- store.md § Record identity per kind). Written at every row birth /
        -- body replacement; the identity is not re-derivable from event_id —
        -- so reads resolve through this. A live row never has NULL.
        record_cid            BLOB,
        PRIMARY KEY (actor_id, calendar_id, event_id)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_caldav_events_uid
        ON bridge_caldav_events(actor_id, calendar_id, uid_hash);
    CREATE INDEX IF NOT EXISTS idx_bridge_caldav_events_modseq
        ON bridge_caldav_events(actor_id, calendar_id, modseq);
    -- The supersede/delete paths' does-another-row-still-reference-this
    -- content record count, across every calendar of the actor.
    CREATE INDEX IF NOT EXISTS idx_bridge_caldav_events_record_cid
        ON bridge_caldav_events(actor_id, record_cid);

    -- Deletion tombstones for sync_calendar_since (RFC 6578 sync-collection).
    CREATE TABLE IF NOT EXISTS bridge_caldav_expunged (
        actor_id     BLOB NOT NULL,
        calendar_id  BLOB NOT NULL,
        event_id     BLOB NOT NULL,
        uid_hash     BLOB NOT NULL,
        modseq       INTEGER NOT NULL,
        expunged_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_caldav_expunged_modseq
        ON bridge_caldav_expunged(actor_id, calendar_id, modseq);
";

// Phase-E CardDAV collection/card state (encrypted-mode bridge) — a structural
// twin of the `bridge_caldav_*` tables above, for address books / vCards.
// CardDAV design proposal (tracked internally), § 3.
// metadata is sealed under the owner's MLS read key — nest never reads it; ctag
// and highestmodseq are the plaintext-floor sync counters, bumping on every PUT
// / DELETE. These tables are user-irrecoverable at-rest data (a user's address
// book), so they are NEVER dropped/recreated (the no-user-data-loss invariant;
// see `docs/goal/architecture/version-compatibility.md`).
pub(super) const MIGRATIONS_BRIDGE_CARDDAV: &str = "
    CREATE TABLE IF NOT EXISTS bridge_carddav_addressbooks (
        actor_id            BLOB NOT NULL,
        addressbook_id      BLOB NOT NULL,
        encrypted_metadata  BLOB NOT NULL,
        ctag                INTEGER NOT NULL DEFAULT 0,
        highestmodseq       INTEGER NOT NULL DEFAULT 1,
        created_at          INTEGER NOT NULL,
        PRIMARY KEY (actor_id, addressbook_id)
    );

    -- Per-card CardDAV placement (encrypted-mode bridge). The sealed vCard rests
    -- in the actor's `__card` segment, addressed by record_cid; uid_hash =
    -- blake3(plaintext_vCard_UID) floors only the lookup index. card_id =
    -- blake3(domain-tag || actor || timestamp_le_i64 || encrypted_body) —
    -- deterministic so transport-retry collapses on PK. etag =
    -- format!(\"{:016x}\", modseq); served upstream to the MUA for If-Match.
    -- encrypted_fauna_ext: nullable sealed Fauna-extension sidecar (e.g. the
    -- X-FAUNA-ACTOR-ID linkage to a social contact). NULL when the row was last
    -- written by a MUA (no sidecar); never served to a
    -- CardDAV MUA — only Fauna apps read it.
    CREATE TABLE IF NOT EXISTS bridge_carddav_cards (
        actor_id              BLOB NOT NULL,
        addressbook_id        BLOB NOT NULL,
        card_id               BLOB NOT NULL,
        uid_hash              BLOB NOT NULL,
        encrypted_index_hint  BLOB NOT NULL,
        etag                  TEXT NOT NULL,
        modseq                INTEGER NOT NULL DEFAULT 1,
        ciphertext_size       INTEGER NOT NULL,
        internal_date         INTEGER NOT NULL,
        created_at            INTEGER NOT NULL,
        encrypted_fauna_ext   BLOB,
        -- The record's content-hash filing CID — the calendar twin's column
        -- with the same rules (a live row never has NULL).
        record_cid            BLOB,
        PRIMARY KEY (actor_id, addressbook_id, card_id)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_carddav_cards_uid
        ON bridge_carddav_cards(actor_id, addressbook_id, uid_hash);
    CREATE INDEX IF NOT EXISTS idx_bridge_carddav_cards_modseq
        ON bridge_carddav_cards(actor_id, addressbook_id, modseq);
    -- The card twin of `idx_bridge_caldav_events_record_cid`.
    CREATE INDEX IF NOT EXISTS idx_bridge_carddav_cards_record_cid
        ON bridge_carddav_cards(actor_id, record_cid);

    -- Deletion tombstones for sync_addressbook_since (RFC 6578 sync-collection).
    CREATE TABLE IF NOT EXISTS bridge_carddav_expunged (
        actor_id       BLOB NOT NULL,
        addressbook_id BLOB NOT NULL,
        card_id        BLOB NOT NULL,
        uid_hash       BLOB NOT NULL,
        modseq         INTEGER NOT NULL,
        expunged_at    INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_carddav_expunged_modseq
        ON bridge_carddav_expunged(actor_id, addressbook_id, modseq);
";

/// User-minted capability grants (capability-mediated content processing,
/// design spec § Phase 2 Step 2 § 2.4). One row per `(owner_actor_id,
/// grant_id)`. The `blob` is a canonical-dag-cbor `fauna-mls::wrapped_blob::
/// GrantBlob`, HPKE-sealed to `holder_pubkey` — the nest stores ciphertext it
/// cannot open (`encryption-at-rest.md` § nest holds no content key). The
/// `holder_pubkey` index serves `fauna.capabilities.fetch` (holder-scoped);
/// `epoch_end` is the expiry filter surfaced to fetch/settings **without**
/// opening the blob.
pub(super) const MIGRATIONS_CAPABILITY_GRANTS: &str = "
    CREATE TABLE IF NOT EXISTS capability_grants (
        owner_actor_id BLOB NOT NULL,
        grant_id       BLOB NOT NULL,
        holder_pubkey  BLOB NOT NULL,
        blob           BLOB NOT NULL,
        epoch_end      INTEGER NOT NULL,
        created_at     INTEGER NOT NULL,
        PRIMARY KEY (owner_actor_id, grant_id)
    );
    CREATE INDEX IF NOT EXISTS idx_capability_grants_holder
        ON capability_grants(holder_pubkey);
";

/// Nest-side store for the user-granted `NestBackupKey` (nest-side segment
/// backup slice 2, design tracked internally;
/// `key-material-hierarchy.md` § Path A-sibling-0). One row per owner actor:
/// the user's own client mints `NestBackupKey` from its identity seed and
/// grants the 32-byte key to its source nest at destination-enroll, so the
/// in-process backup coordinator can seal that owner's segments under it.
///
/// **Unlike `capability_grants`, this key is stored PLAINTEXT and is
/// nest-readable by design** — the coordinator must open it to seal, and it
/// reveals nothing the nest does not already host (the segment files it seals
/// rest plaintext-framed in the same data dir; design record § Trust-domain
/// analysis + `key-material-hierarchy.md` § Path A-sibling-0). It sits beside
/// the deployment key material nest.db already holds (the `nest_keypair`
/// row). Revoke = delete the row (idempotent).
pub(super) const MIGRATIONS_NEST_BACKUP_KEYS: &str = "
    CREATE TABLE IF NOT EXISTS nest_backup_keys (
        owner_actor_id BLOB NOT NULL PRIMARY KEY,
        backup_key     BLOB NOT NULL,
        granted_at     INTEGER NOT NULL
    );
";

/// **Destination-side** nest-writer grant store (nest-side segment backup
/// slice 3; `federation.md` § Nest-writer backup plane,
/// `segment-backup-protocol.md` § Cross-location backup protocol → *The writer
/// seat*). **One row per owner — the writer seat**: the owner's client, over
/// its **own** authed connection to the destination, seats the one source nest
/// that may write that owner's segment-backup custody here. A second source
/// box is refused while the owner's custody holds a live path, and the seat
/// moves only by the owner-carried `succeeds` handover (`db/
/// backup_writer_grants.rs` holds the four arms).
///
/// **This row IS the authorization.** A federated backup write carries a
/// verified `origin_nest_id` (the channel handshake authenticates the peer),
/// but a nest id is self-minted and free — signature is *attribution*, never
/// permission. The gate is therefore state **this** nest wrote at the owner's
/// direction, the same shape as the `channel.fetch` foreign-member gate and as
/// `folder_member_access` for cross-nest folder writers. Revoke = mark the row
/// `revoked` (idempotent), which refuses the next `write_token.mint` — an
/// outstanding token's residual window is one TTL, the accepted contract
/// restated from the folder write plane. The row stays, because a revoke says
/// *this box may no longer write* and never *another box may now overwrite
/// what it wrote*.
///
/// `seated_at` is the seat's clock: it restarts when another writer takes a
/// seat whose custody holds no live path, a handover keeps it, and
/// `fauna.backup.generation.restore` refuses a generation superseded before
/// it. Its `DEFAULT 0` is the value a row seated before the column existed
/// carries — a clock no retained generation precedes; every writer names it.
///
/// Deliberately **not** pairing-gated: `is_paired` stays scoped to the private
/// nest-sync surface, so a user can back up to any nest they can authenticate
/// to without opening the paired-peer surface between the two boxes.
pub(super) const MIGRATIONS_BACKUP_WRITER_GRANTS: &str = "
    CREATE TABLE IF NOT EXISTS backup_writer_grants (
        owner_actor_id BLOB NOT NULL PRIMARY KEY,
        writer_nest_id BLOB NOT NULL,
        granted_at     INTEGER NOT NULL,
        -- 1 = the holder's grant is revoked. The seat stays held.
        revoked        INTEGER NOT NULL DEFAULT 0,
        -- Unix seconds the seat was taken (the seat's clock).
        seated_at      INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_backup_writer_grants_writer
        ON backup_writer_grants(writer_nest_id);
";

/// **Source-side** per-owner backup destination registry (nest-side segment
/// backup slice 3; `message-segment-store.md` § Cross-location backup protocol,
/// `backup-restore.md` § Background Tasks). One row per
/// `(owner_actor_id, destination_id)`: where the source nest's in-process
/// coordinator backs an owner up.
///
/// **Why the nest needs this at all:** the owner's destinations live in
/// the `fauna.state.backup` plane entries, which the nest only holds as opaque
/// client-sealed ciphertext (it never holds `BackupKey`), so the
/// destination list is not derivable nest-side. The client registers each
/// destination here explicitly at enroll (`fauna.backup.destination.register`).
/// `nest_url` + `nest_id` are what the coordinator's federation dial needs (URL
/// to reach + 32-byte expected-peer id). None of it is secret —
/// the user's own chosen backup targets — so the row rests plaintext.
pub(super) const MIGRATIONS_BACKUP_DESTINATIONS: &str = "
    CREATE TABLE IF NOT EXISTS backup_destinations (
        owner_actor_id BLOB NOT NULL,
        destination_id TEXT NOT NULL,
        nest_url       TEXT NOT NULL,
        nest_id        BLOB NOT NULL,
        added_at       INTEGER NOT NULL,
        kind TEXT NOT NULL DEFAULT 'nest',
        custodian_device_id TEXT,
        capacity_cap_bytes INTEGER,
        PRIMARY KEY (owner_actor_id, destination_id)
    );
";

/// Ordinary-folder destination **coverage** — which of the owner's folders each
/// registered destination holds (`docs/goal/behavior/backup-destinations.md`
/// § Ordinary-folder coverage — destination places). One row per
/// `(owner, destination_id, folder_id)`, written by the idempotent USER-class
/// `fauna.backup.destination.attach_folder` / `.detach_folder` pair and read by
/// the coordinator sweep (nest kind, push) and `fauna.backup.destination.list`
/// (client-device kind, pull; the folders-page render).
///
/// Lives beside `backup_destinations` for the same reason that table exists:
/// the client's authoritative copy (the `fauna.state.backup` destination rows with
/// `folder_name = "__folder/…"`) is client-sealed and the nest cannot read it,
/// and the nest drives the push. `folder_id` references `folders.id` on this
/// same nest — coverage is `FolderRef::Local` only by ratified design, so a
/// foreign folder has nothing to reference. Nothing here is secret (the owner's
/// own targets), so the rows rest plaintext like the registry itself.
pub(super) const MIGRATIONS_BACKUP_DESTINATION_FOLDERS: &str = "
    CREATE TABLE IF NOT EXISTS backup_destination_folders (
        owner_actor_id BLOB    NOT NULL,
        destination_id TEXT    NOT NULL,
        folder_id      INTEGER NOT NULL,
        added_at       INTEGER NOT NULL,
        PRIMARY KEY (owner_actor_id, destination_id, folder_id)
    );
";

/// Client-device custodian **check-ins** — the source nest's only knowledge of
/// what a custodian holds (`message-segment-store.md` § Client-device custodian
/// (pull) → *Check-in*).
///
/// One row per `(owner_actor_id, destination_id)`: the latest check-in wins, so
/// this is a projection rather than a log. The device is the owner's own and
/// reports on itself, so none of the adversarial-writer machinery (grace window
/// T, destination-derived charging) applies — there is no foreign writer.
///
/// **`caught_up_at` is deliberately separate from `checked_in_at`.**
/// `behavior/backup-destinations.md` § Third destination kind defines the row's *last synced* as
/// "the last time the device checked in **having caught up**" — a device that
/// checks in while still behind must not advance it, or a permanently-lagging
/// custodian would render as freshly synced on every pass. NULL = has never
/// caught up.
///
/// Nothing here is secret: it is the owner's own device reporting its own
/// progress, so the row rests plaintext like `backup_destinations` itself.
pub(super) const MIGRATIONS_BACKUP_CUSTODIAN_CHECKINS: &str = "
    CREATE TABLE IF NOT EXISTS backup_custodian_checkins (
        owner_actor_id BLOB    NOT NULL,
        destination_id TEXT    NOT NULL,
        high_water     INTEGER NOT NULL,
        held_bytes     INTEGER NOT NULL,
        cap_state      TEXT    NOT NULL,
        checked_in_at  INTEGER NOT NULL,
        caught_up_at   INTEGER,
        audit_state         TEXT,
        last_audit_passed_at INTEGER,
        PRIMARY KEY (owner_actor_id, destination_id)
    );
";

/// Custody-**hosting** rows — the custodian-nest runtime's stage (b)
/// (`account-data-plane.md` § Replica posture → The custody grant + ceremony,
/// the device-or-nest bullet, item 6). One row per `(host_actor_id, grant_id)`:
/// a custody this nest's own user accepted NEST-anchored, deposited over
/// `fauna.custody.hosting.register` so the nest's pump can run the pull leg
/// with no host device running.
///
/// **Why the nest needs this at all:** the ceremony record (`HeldCustody`)
/// lives in the host's client-sealed `fauna.state.custody-ceremony` entries, which this nest cannot read —
/// the same reason `backup_destinations` exists. The row is a projection of
/// that record: witness verbatim (owner-signed, self-contained), the owner's
/// nest URL (the pull leg's only dial anchor), the opaque owner-fleet
/// endpoints snapshot, and the host-chosen budget. Nothing here is secret —
/// the witness is a signed public artifact and the custodied planes it admits
/// stay sealed — so the row rests plaintext.
///
/// `held_bytes` / `last_receipt_at` are pump-written metering, read back over
/// `fauna.custody.hosting.list` for the host UI's "what my nest holds for
/// others" render.
pub(super) const MIGRATIONS_CUSTODY_HOSTING: &str = "
    CREATE TABLE IF NOT EXISTS custody_hosting (
        host_actor_id      BLOB    NOT NULL,
        grant_id           BLOB    NOT NULL,
        owner_actor_id     BLOB    NOT NULL,
        witness            BLOB    NOT NULL,
        owner_nest_url     TEXT    NOT NULL,
        owner_devices      BLOB    NOT NULL,
        retained_bytes_cap INTEGER NOT NULL,
        stopped            INTEGER NOT NULL DEFAULT 0,
        updated_at         INTEGER NOT NULL,
        held_bytes         INTEGER NOT NULL DEFAULT 0,
        last_receipt_at    INTEGER NOT NULL DEFAULT 0,
        last_receipt_degraded INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (host_actor_id, grant_id)
    );
";

/// Staged custody **receipts** — the owner-side half of stage (c)
/// (`account-data-plane.md` § Replica posture → The custody grant + ceremony,
/// item 6): the newest signed receipt each custodian NEST deposited for one
/// of this owner's grants, staged latest-per-grant for the owner's fleet to
/// fetch and fold at sync. Monotone in the receipt's own `attested_at` — an
/// older deposit never displaces a newer one.
///
/// The blob is the signed envelope VERBATIM: the fleet re-verifies the very
/// signature a re-encode would invalidate. Nothing here is secret (a receipt
/// is a signed attestation about held-bytes/coverage shape), and the row is
/// pure derived redundancy telemetry — the custodian re-attests on its own
/// cadence, so a lost row is re-earned within a day.
pub(super) const MIGRATIONS_CUSTODY_RECEIPTS_STAGED: &str = "
    CREATE TABLE IF NOT EXISTS custody_receipts_staged (
        owner_actor_id BLOB    NOT NULL,
        grant_id       BLOB    NOT NULL,
        receipt        BLOB    NOT NULL,
        attested_at    INTEGER NOT NULL,
        staged_at      INTEGER NOT NULL,
        PRIMARY KEY (owner_actor_id, grant_id)
    );
";

/// RecoveryKey **registration chain** (identity-succession slice 2;
/// `docs/goal/behavior/identity-succession.md` § The RecoveryKey).
///
/// One row per link in an identity's chain: the verbatim canonical DAG-CBOR
/// bytes the owner's client signed and submitted, plus the two columns the nest
/// itself indexes on (`seq`, `recovery_pubkey`). Storing `record` verbatim is
/// deliberate — the serve path (`fauna.recovery.registration.chain`) replays
/// exactly what was signed instead of re-encoding a record the nest did not
/// author, so a canonicalization drift could never invalidate a chain already
/// on disk.
///
/// `seq` is monotonic **per identity and shared with succession statements**
/// (`identity-succession.md:56` — a succession advances "the chain the consumer
/// last saw"), so this is one sequence per actor, not two. The PK enforces
/// that: a replayed `seq` cannot land twice.
///
/// Nothing here is secret — the same `recovery_pubkey` rides the actor's public
/// signed `Profile` — so the rows rest plaintext, like `backup_destinations`.
pub(super) const MIGRATIONS_RECOVERY_REGISTRATIONS: &str = "
    CREATE TABLE IF NOT EXISTS recovery_registrations (
        actor_id        BLOB NOT NULL,
        seq             INTEGER NOT NULL,
        recovery_pubkey BLOB NOT NULL,
        record          BLOB NOT NULL,
        created_at      INTEGER NOT NULL,
        PRIMARY KEY (actor_id, seq)
    );
";

/// The **seed-escrow blob** store (identity-succession slice 2;
/// `identity-succession.md` § Seed escrow). One row per identity: the identity
/// seed HPKE-sealed to the RecoveryKey's escrow public half.
///
/// Unlike `recovery_registrations`, this row IS secret-bearing — but the nest
/// holds no half of the sealing key, so what rests here is ciphertext it cannot
/// open (key-material rule #4). It is stored as an opaque BLOB for exactly that
/// reason; no nest-side code parses its structure.
///
/// One row per actor (PK on `actor_id`), replaced in place: the blob is
/// rewritten only when the sealed value or the sealing key changes, and a
/// superseded blob's kit no longer exists to open it. **This is a
/// user-irrecoverable value** — losing the row costs the owner their only
/// recovery path after total device loss — so nothing may drop or rebuild this
/// table.
pub(super) const MIGRATIONS_RECOVERY_ESCROW: &str = "
    CREATE TABLE IF NOT EXISTS recovery_escrow (
        actor_id   BLOB PRIMARY KEY,
        blob       BLOB NOT NULL,
        updated_at INTEGER NOT NULL
    );
";

/// The **pending seed-initiated RecoveryKey replacement** store
/// (identity-succession slice 2; `identity-succession.md:37`). One row per
/// identity: a `SignedRecoveryKeyRegistration` (no `prior_recovery_sig`)
/// parked for `RECOVERY_REPLACE_GRACE_SECS`, vetoable instantly by the current
/// RecoveryKey, landed onto `recovery_registrations` by the periodic sweep
/// only if the window ran uncontested AND the record still advances the
/// then-current head.
///
/// `record` rides verbatim (the embed-as-bytes rule), `record_digest` is
/// BLAKE3 of those bytes — the idempotence key: a re-request of the identical
/// record keeps the ORIGINAL `requested_at`, so a replayed request cannot
/// extend the window. `new_recovery_pubkey`/`seq` are denormalized for the
/// status projection and the landing sweep's cheap pre-check.
///
/// A row here confers no authority until landed; losing one costs a requester
/// a re-request.
pub(super) const MIGRATIONS_RECOVERY_PENDING: &str = "
    CREATE TABLE IF NOT EXISTS recovery_pending_replacements (
        actor_id            BLOB PRIMARY KEY,
        record              BLOB NOT NULL,
        record_digest       BLOB NOT NULL,
        new_recovery_pubkey BLOB NOT NULL,
        seq                 INTEGER NOT NULL,
        requested_at        INTEGER NOT NULL
    );
";

/// The **succession** store (identity-succession slice 3;
/// `identity-succession.md` § Enforcement on the home nest, step 1) — "the row
/// every enforcement point consults".
///
/// One row per *succeeded* identity. `old_actor_id` is the PRIMARY KEY, which
/// makes the design's first-succession-wins rule structural: an identity is
/// succeeded **at most once**, and a later ceremony for the same owner is a
/// succession of the *successor* (`B → C`), authorized by the successor's own
/// registration chain. Without that PK a second statement at a higher `seq`
/// could re-point an already-succeeded account a second time — a handle-takeover
/// primitive, since the old RecoveryKey retires with the old identity
/// (`identity-succession.md:42`) but its signatures stay verifiable forever.
///
/// `statement` rides **verbatim** (the embed-as-bytes rule, `transport.md`) so
/// the pre-identity lookup kind replays exactly the bytes the client signed —
/// a re-encode could invalidate a statement the nest itself accepted.
/// `new_actor_id` is indexed because the lookup walks `old → new` repeatedly and
/// the reverse direction ("was this identity itself a successor?") is what bounds
/// a chain walk.
///
/// `seq` belongs to the **old** identity's one monotonic chain, shared with
/// `recovery_registrations` — it is the value the
/// statement had to advance past to be accepted.
///
/// Nothing here is secret: a succession statement is public by construction
/// (peers, MLS members and federation all consume it). This table is
/// **user-irrecoverable** — dropping it silently un-supersedes a stolen identity,
/// i.e. hands the account back to the thief — so nothing may rebuild it.
pub(super) const MIGRATIONS_ACTOR_SUCCESSIONS: &str = "
    CREATE TABLE IF NOT EXISTS actor_successions (
        old_actor_id BLOB PRIMARY KEY,
        new_actor_id BLOB NOT NULL,
        statement    BLOB NOT NULL,
        seq          INTEGER NOT NULL,
        succeeded_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_actor_successions_new
        ON actor_successions(new_actor_id);
";

/// A succession's **owed nests** (`identity-succession.md` § Enforcement on
/// the home nest → *Every nest the identity is linked to*, **The road**): the
/// destinations of the `nest_pairings` rows the succession transaction burned,
/// kept so the successor's devices can carry the statement to each of them.
///
/// Keyed on the **retired** identity, as `actor_successions` is: an entry says
/// "the identity `old_actor_id` was paired with `nest_id` when it was
/// succeeded here, and that nest has not been told". `nest_id` and `nest_url`
/// are the burned row's `private_nest_id` and `nest_url`, verbatim, and
/// `created_at` is that row's own `created_at` — the order the list was kept
/// in and is served in, oldest pairing first.
///
/// Written only inside `record_succession`, capped per succession
/// (`db::successions::MAX_OWED_NESTS`), read by
/// `fauna.recovery.succession.status` for the caller's whole predecessor path,
/// and deleted one entry at a time by `fauna.recovery.succession.owed_settle`.
/// The nest never dials an entry.
///
/// **User-irrecoverable.** The pairing rows it was copied from are gone, and a
/// device keeps one nest per identity, so nothing can rebuild the list; without
/// it the retired identity keeps signing in at every nest it names.
pub(super) const MIGRATIONS_SUCCESSION_OWED_NESTS: &str = "
    CREATE TABLE IF NOT EXISTS succession_owed_nests (
        old_actor_id BLOB NOT NULL,
        nest_id      BLOB NOT NULL,
        nest_url     TEXT,
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (old_actor_id, nest_id)
    );
";

/// The nest's **id→URL directory**: every address at which this nest has ever
/// seen a peer identity, one row per `(nest_id, nest_url)` pair.
///
/// `channel_foreign_members.nest_url` above was serving as this directory, but
/// its PK is the *membership* pair `(channel_id, actor_id)` — so
/// `register_foreign_channel_member`'s upsert, which exists to keep a membership
/// row current, **overwrote the address in place**. `created_at` and rowid stay
/// with the row, so a planted address inherited the honest row's sighting *and*
/// deleted it; for an identity known through a single cross-nest row the
/// candidate walk added had nothing left to fail over to, restoring the
/// permanent silent denial that walk was built to close.
///
/// Splitting the directory out makes that erasure **unrepresentable** rather
/// than guarded: a differing address for a known identity is a new row, so
/// addresses only ever *join*. This is also the behavior
/// `db/channels.rs::resolve_foreign_nest_urls` already documented and could not
/// deliver — a genuine address migration resolves because both addresses are
/// candidates and the stale one simply fails its identity proof.
///
/// `proven_at` records that a federation `hello` handshake actually bound this
/// identity to this URL (`federation_channel::dial` — a signed possession
/// proof), as opposed to a merely *sighted* address that some caller asserted.
/// Readers order proven-first, which an attacker cannot reach without the
/// honest nest's key. Write-once-ish: it is only ever set, never cleared, and
/// `first_seen` never moves later on a re-sighting.
///
/// `channel_foreign_members.nest_url` keeps being written beside it, so the
/// membership row stays self-describing. Derived trust state throughout: every
/// row is re-learnable from a later Welcome relay.
pub(super) const MIGRATIONS_NEST_ADDRESSES: &str = "
    CREATE TABLE IF NOT EXISTS nest_addresses (
        nest_id    BLOB    NOT NULL,
        nest_url   TEXT    NOT NULL,
        first_seen INTEGER NOT NULL,
        proven_at  INTEGER,
        PRIMARY KEY (nest_id, nest_url)
    );
    CREATE INDEX IF NOT EXISTS idx_nest_addresses_lookup
        ON nest_addresses(nest_id, first_seen);
";

/// The **recovery chain head this nest has learned per foreign identity**
/// (identity-succession slice-4 hardening; `identity-succession.md`
/// § Propagation — the peer-side anchor). One row per remote actor: the
/// `(recovery_pubkey, seq)` head of the registration chain last verified from
/// that identity's anchored home nest, written by the succession pull/verify
/// path and consulted as `verify_registration_chain`'s `known` parameter — so
/// a chain served later must *extend* what this nest already saw, and a seed
/// thief's from-scratch re-mint is refused even if the serving source turns
/// hostile. `seq` only ever advances (the DAO refuses regressions).
///
/// Derived trust state, not user data: every row is re-learnable by
/// re-fetching and re-verifying the identity's public chain.
pub(super) const MIGRATIONS_FOREIGN_RECOVERY_HEADS: &str = "
    CREATE TABLE IF NOT EXISTS foreign_recovery_heads (
        actor_id        BLOB PRIMARY KEY,
        recovery_pubkey BLOB NOT NULL,
        seq             INTEGER NOT NULL,
        learned_at      INTEGER NOT NULL,
        anchor_nest_id BLOB NOT NULL
    );
";

/// Web-paywall **sealed rendered output** (`monetization.md` § Pillar 2;
/// `encryption-at-rest.md` § Readable classes item 2). One row per
/// `(actor_id, path)` — the sealed twin of `web_rendered`: `blob_hash` points
/// at ChaCha20-Poly1305 ciphertext in the blob store, sealed under
/// `derive_web_render_key(tier period_key, post_id)`, so the box can serve
/// the full rendered page only while the web-serve holder wields the
/// creator's live capability grant for `tier` (revoke ⇒ unopenable ⇒ the
/// public teaser at the same path in `web_rendered` is all that serves).
/// `tier` + `post_id` are the serve-time derive inputs; both are floor
/// metadata already carried plaintext on the gated post record. This table
/// is derived render output — cleared and rebuilt on every re-render
/// (recreatable, like `web_rendered`).
pub(super) const MIGRATIONS_WEB_RENDERED_SEALED: &str = "
    CREATE TABLE IF NOT EXISTS web_rendered_sealed (
        actor_id     BLOB NOT NULL,
        path         TEXT NOT NULL,
        blob_hash    BLOB NOT NULL,
        content_type TEXT NOT NULL,
        tier         TEXT NOT NULL,
        post_id      BLOB NOT NULL,
        updated_at   INTEGER NOT NULL,
        PRIMARY KEY (actor_id, path)
    );
";

/// The **owed-render marker** (`web-content-hosting.md` § Routing, render,
/// serving → *A revoke is durable*). One row per actor whose rendered site
/// still carries content a committed state change removed: every revoking door
/// writes it in the SAME transaction as its state change, a render that
/// completes (or a fail-closed clear) discharges it, and the boot drain
/// renders whatever is still owed — the single decision point plus boot
/// reconcile of `nest/common.md` § Client-state recoverability.
///
/// `nonce` is re-rolled by every mark, and a discharge deletes only the nonce
/// it read before its render began — so a revoke committed WHILE a render is
/// running (whose listing may predate it) is never discharged by that render.
///
/// Derived bookkeeping, not user data: recreatable by re-rendering every site.
pub(super) const MIGRATIONS_WEB_RENDER_OWED: &str = "
    CREATE TABLE IF NOT EXISTS web_render_owed (
        actor_id BLOB PRIMARY KEY NOT NULL,
        nonce    INTEGER NOT NULL,
        owed_at  INTEGER NOT NULL
    );
";

/// The **owed restore** (`web-content-hosting.md` § Routing, render, serving →
/// *A blanked site is owed its restore*). One row per actor whose rendered
/// site a fail-closed clear took dark: the clear writes it in its OWN
/// transaction, any render that completes discharges it, and the boot drain
/// and the restore retry render whatever is still owed. Deliberately not the
/// owed-render marker above — that one promises that nothing withdrawn still
/// serves, and a clear keeps that promise; this one promises only that a dark
/// site is tried again.
///
/// `nonce` works as the marker's does: a discharge deletes only the generation
/// it read before its render began, so a clear that lands WHILE a render is
/// running is never discharged by that render.
///
/// Derived bookkeeping, not user data: recreatable by re-rendering every site.
pub(super) const MIGRATIONS_WEB_RESTORE_OWED: &str = "
    CREATE TABLE IF NOT EXISTS web_restore_owed (
        actor_id BLOB PRIMARY KEY NOT NULL,
        nonce    INTEGER NOT NULL,
        owed_at  INTEGER NOT NULL
    );
";

/// The **render generation** (`web-content-hosting.md` § Routing, render,
/// serving → *One render writes at a time*). One row per actor whose site has
/// ever been rendered, holding a counter bumped by every render at its
/// *listing* and by every fail-closed clear. A render carries the value it
/// bumped to, and every write it makes to `web_rendered` / `web_rendered_sealed`
/// — the clear included — is conditional on that value still being current, in
/// the write's own statement. So the render with the **newest listing** owns the
/// site, and one still in flight from an older listing writes nothing rather
/// than putting back what a revoke just removed.
///
/// Why a generation and not a lock: an author controls both how long a render
/// runs and how often one starts (§ Routing, render, serving's safety limits),
/// so a mutex would let them queue a moderator's legal takedown behind hours of
/// their own renders. A generation blocks nobody — the takedown's render clears
/// at once and the older render simply abandons.
///
/// Derived bookkeeping, not user data: a lost row costs at most one
/// unsuppressed stale write in the rare overlap that outlives it.
pub(super) const MIGRATIONS_WEB_RENDER_GENERATION: &str = "
    CREATE TABLE IF NOT EXISTS web_render_generation (
        actor_id   BLOB PRIMARY KEY NOT NULL,
        generation INTEGER NOT NULL
    );
";

/// The **staged render bodies** (`backup-restore.md` § 9 step 2h). A render
/// streams each page body to the blob store as it goes, with its
/// `blob_metadata` row, but writes no `web_rendered` row until its one
/// replacing transaction — so between the two the body is named by nothing on
/// the site, and the blob sweep would take it. A row here, written under the
/// render's claim BEFORE the body is stored, is the reference that keeps it:
/// the replacing transaction deletes the actor's rows as its rendered rows take
/// the reference over, and every generation bump (a newer render's listing,
/// the fail-closed clear) deletes them because the render that staged them can
/// no longer commit.
///
/// Ephemeral bookkeeping, recreatable by construction: a lost row costs at most
/// one in-flight body the sweep may take before its render commits, and a row
/// a crashed render leaves only over-pins until the actor's next render
/// begins.
pub(super) const MIGRATIONS_WEB_RENDER_STAGED: &str = "
    CREATE TABLE IF NOT EXISTS web_render_staged (
        actor_id  BLOB NOT NULL,
        blob_hash BLOB NOT NULL,
        PRIMARY KEY (actor_id, blob_hash)
    );
";

/// ATProto full-PDS state (`atproto-pds-full.md` § Nest state schema).
/// `actor_id` is the 32-byte pubkey BLOB (nest
/// convention — NOT the consume-side crate's legacy TEXT ids). Durability
/// classes: credential verifiers + OAuth grants + the native-records journal +
/// preferences + account settings are product data (never dropped; the journal
/// tombstones via `deleted_at`, never DELETE — re-derivability of the repo
/// requires the tombstone). Sessions are re-derivable/expirable (registered so
/// revocation is *visible*, D4). An `atproto_blobs` row unreferenced past the
/// reference window is GC-able (spec-sanctioned transient upload state —
/// recreatable by re-upload). The session-secret blob is key material — never
/// dropped.
// TP5 — the nest-held OAuth issuer signing key SET
// (`architecture/key-material-hierarchy.md` § Audience: deployment
// infrastructure -> *Issuer signing key*; the surface is
// `behavior/authorization-server.md` § The issuer).
//
// ⚠ Its OWN const, deliberately not a few lines appended to
// MIGRATIONS_ATPROTO_PDS below. The goal doc rules that the authorization
// server is up whenever the nest is up, and that custody moved to the nest
// precisely so the issuer stops depending on an optional bridge. Both consts
// are applied unconditionally today, so the difference costs nothing at
// runtime — but a table named for TP5 living inside a const named for ATProto
// is a claim about ownership that would come true the first time somebody
// gated the ATProto block.
pub(super) const MIGRATIONS_OAUTH_ISSUER: &str = "
    -- A SET, not a key: rotation adds a key and retires the outgoing one after
    -- the access-token horizon, so more than one row is live at once and
    -- /oauth/jwks serves every unexpired public half. That is what lets a token
    -- minted seconds before a rotation still verify.
    --
    -- `kid` is the RFC 7638 JWK thumbprint of the public half, so it is derived
    -- from the key rather than assigned; it is the primary key because two rows
    -- sharing one kid would make the JWKS ambiguous at exactly the moment a
    -- verifier is trying to pick a key.
    --
    -- `secret_wrapped` is the raw 32-byte P-256 scalar sealed under
    -- nest_kek::OAUTH_ISSUER_CONTEXT — the same wire shape as every other
    -- member of that family, and registered in nest_kek::SATELLITES so a
    -- deployment-seed rotation re-keys it rather than stranding it.
    --
    -- `retired_at` NULL = the active signer; non-NULL = still served in the
    -- JWKS, no longer used to sign. A row is deleted only once past the horizon.
    -- Losing this table invalidates outstanding access tokens (clients
    -- re-authorize) and nothing a user authored.
    --
    -- `x`/`y` are the JWK coordinates, stored rather than recomputed so the
    -- JWKS read path is a field read.
    CREATE TABLE IF NOT EXISTS oauth_issuer_keys (
        kid            TEXT PRIMARY KEY,
        secret_wrapped BLOB NOT NULL,
        x              TEXT NOT NULL,
        y              TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        retired_at     INTEGER
    );

    -- The AS's SECOND signer: the HS256 secret its refresh tokens are MACed
    -- under (key-material-hierarchy.md § Audience: deployment infrastructure
    -- -> *OAuth refresh-token signing secret*). One row, `id = 0`: a
    -- deployment has one OAuth refresh plane.
    --
    -- Not a set, unlike the issuer keys above, and the asymmetry is the
    -- two-signers ruling rather than an oversight. The issuer key is a PUBLIC
    -- artifact a stranger verifies, so a rotation must keep the outgoing half
    -- servable while tokens minted under it are alive. This secret is
    -- presented back to this nest and to nothing else, so nothing outside the
    -- box ever needs to verify under a generation this box has replaced --
    -- there is no horizon to serve, and a second row would only be a second
    -- way to sign.
    --
    -- `secret_wrapped` is the raw 32-byte HMAC-SHA-256 key sealed under
    -- nest_kek::OAUTH_SESSION_CONTEXT, registered in nest_kek::SATELLITES so a
    -- deployment-seed rotation re-keys it rather than stranding it. Losing
    -- this row invalidates outstanding OAuth refresh tokens -- clients
    -- re-authorize -- and nothing a user authored.
    CREATE TABLE IF NOT EXISTS oauth_session_secret (
        id             INTEGER PRIMARY KEY CHECK(id = 0),
        secret_wrapped BLOB NOT NULL,
        created_at     INTEGER NOT NULL
    );
";

pub(super) const MIGRATIONS_ATPROTO_PDS: &str = "
    CREATE TABLE IF NOT EXISTS atproto_app_credentials (
        actor_id      BLOB NOT NULL,
        credential_id TEXT NOT NULL,
        label         TEXT NOT NULL,
        verifier      TEXT NOT NULL,
        dm_allowed    INTEGER NOT NULL DEFAULT 0,
        created_at    INTEGER NOT NULL,
        last_used_at  INTEGER,
        PRIMARY KEY (actor_id, credential_id)
    );

    CREATE TABLE IF NOT EXISTS atproto_sessions (
        actor_id          BLOB NOT NULL,
        session_id        BLOB NOT NULL,
        plane             TEXT NOT NULL,
        credential_id     TEXT,
        client_note       TEXT,
        created_at        INTEGER NOT NULL,
        last_refreshed_at INTEGER,
        expires_at        INTEGER NOT NULL,
        revoked_at        INTEGER,
        -- Rotate-on-use refresh (F1 detail): `session_id` is the immutable
        -- family id (the INITIAL refresh jti); this column tracks the one
        -- currently-valid refresh jti and is UPDATEd on each rotation. A
        -- presented jti that is neither current nor initial-on-a-fresh-row
        -- is a replay -> the row is revoked (family kill). NULL means
        -- not-yet-rotated (current jti == session_id).
        current_refresh_jti BLOB,
        PRIMARY KEY (actor_id, session_id)
    );
    CREATE INDEX IF NOT EXISTS idx_atproto_sessions_credential
        ON atproto_sessions(actor_id, credential_id);

    CREATE TABLE IF NOT EXISTS atproto_oauth_grants (
        actor_id     BLOB NOT NULL,
        grant_id     BLOB NOT NULL,
        client_id    TEXT NOT NULL,
        client_name  TEXT,
        scopes       TEXT NOT NULL,
        dpop_jkt     TEXT,
        created_at   INTEGER NOT NULL,
        last_used_at INTEGER,
        expires_at   INTEGER,
        revoked_at   INTEGER,
        -- The permission sets `scopes` was expanded from -- frozen at the
        -- ceremony and JSON-encoded (`ConsentSetInfo`) so the connected-apps
        -- row can say WHERE a scope came from long after the card is gone.
        -- NULL on every grant that named no set.
        sets         TEXT,
        -- WHICH authorization server minted this grant: 'nest' for the
        -- nest-hosted AS (`oauth_as_routes`), the one issuer standing since
        -- the bridge AS retired at schema 82. A forced session-secret rotation
        -- kills exactly the families MACed under the nest's own HS256 refresh
        -- secret (§ The issuer -> Two HS256 secrets not one), and this mark
        -- is how that set is spelled.
        issuer       TEXT NOT NULL,
        PRIMARY KEY (actor_id, grant_id)
    );

    -- The third-party principal (third-party.md § The principal model, TP1):
    -- the nest-side identity of ONE approved metadata document for ONE
    -- account, minted by the consent act in the same transaction as the
    -- grant row above (`record_atproto_oauth_grant`). Plaintext-floor routing
    -- metadata -- a client identity and a public key -- and never key
    -- material. `holder_x25519` is the X25519 public key the client attested
    -- at PAR, NULL for a client that presented none; `execution_form` is
    -- 'remote' | 'device' for a consent-minted principal, 'wasm' for a
    -- hosted plugin ('container' waits for the supervisor); `declared_kinds`
    -- is a JSON array of the `ext.*` kinds the document's verified manifest
    -- declares ('[]' for a document without one) and `publisher_key` the
    -- manifest's Ed25519 key (third-party-kinds.md § The manifest), both as
    -- the latest consented document said; `writer_ed25519` is the Ed25519
    -- key the client attested for its own rows (§ Principal write
    -- authority), NULL for none; `declared_bridge` is the manifest's
    -- validated `bridge` block as JSON (third-party.md § The manifest → The
    -- `bridge` block), NULL for a principal that is not a conversation
    -- bridge, its `id` unique per account at consent; `declared_service_auth`
    -- is the manifest's `service_auth` member as a JSON array of
    -- `{aud, lxm}` entries (third-party.md § The manifest), '[]' for a
    -- document that declares none -- the set the oracle's
    -- `atproto.service_auth` class is bounded by; `events_uri` is the
    -- manifest's validated `events_uri` (transport.md § Push events → Third-
    -- party event doors, the webhook), NULL for a document that declares
    -- none. Revocation DELETEs the row (rule 4: one verb).
    --
    -- A HOSTED plugin (third-party.md § The runner contract) has one INSTALL
    -- row under the reserved nest-owner actor (`NEST_OWNER_ACTOR`, all zero
    -- -- the slot every nest-side-not-an-account position already uses) plus
    -- one ordinary BINDING row per account whose own consent bound it, all
    -- sharing the plugin's `client_id` and the host-minted holder key.
    CREATE TABLE IF NOT EXISTS third_party_principals (
        actor_id       BLOB NOT NULL,
        principal_id   BLOB NOT NULL,
        client_id      TEXT NOT NULL,
        holder_x25519  BLOB,
        execution_form TEXT NOT NULL,
        declared_kinds TEXT NOT NULL DEFAULT '[]',
        granted_scopes TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        last_used_at   INTEGER,
        label          TEXT,
        publisher_key  BLOB,
        writer_ed25519 BLOB,
        declared_bridge TEXT,
        declared_service_auth TEXT NOT NULL DEFAULT '[]',
        events_uri     TEXT,
        PRIMARY KEY (actor_id, principal_id),
        UNIQUE (actor_id, client_id)
    );
    -- One key, one principal per account: revoke ends the capability grants
    -- whose holder is the row's key, so two rows sharing a key would let
    -- revoking one silently end the other's.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_third_party_principals_holder
        ON third_party_principals(actor_id, holder_x25519)
        WHERE holder_x25519 IS NOT NULL;
    -- One writer key, one principal per account -- the writer twin of the
    -- index above: a `content.write` grant names its writer, so two rows
    -- sharing one would each be authorized as the other.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_third_party_principals_writer
        ON third_party_principals(actor_id, writer_ed25519)
        WHERE writer_ed25519 IS NOT NULL;

    -- The hosted-form half of an installed plugin's principal (third-party.md
    -- § Execution forms → WASM components): what the install leg resolved
    -- from the document's `execution` member and what the runner needs to
    -- start it. One row per install row (`principal_id` under
    -- `NEST_OWNER_ACTOR`); `client_id` is unique, so a document installs
    -- once. `module_digest` is the `sha256:` digest the document pinned and
    -- the fetch verified; the component bytes rest in the data directory at
    -- `plugins/<principal_id hex>/plugin.wasm`, beside the holder secret
    -- (`holder.key`, mode 0600 -- the host side of the sandbox's state scope,
    -- never a column the plugin's `state` import could read). `hosts`,
    -- `ingress`, `settings_schema` are JSON copies of the verified members.
    -- Uninstall DELETEs this row, the install row, every account's binding
    -- row for `client_id` (through the one revoke cascade) and the plugin's
    -- state.
    CREATE TABLE IF NOT EXISTS hosted_plugins (
        principal_id    BLOB PRIMARY KEY,
        client_id       TEXT NOT NULL UNIQUE,
        module_digest   TEXT NOT NULL,
        hosts           TEXT NOT NULL DEFAULT '[]',
        ingress         TEXT NOT NULL DEFAULT '[]',
        settings_schema TEXT,
        installed_by    BLOB NOT NULL,
        installed_at    INTEGER NOT NULL
    );

    -- A hosted plugin's own state scope: the key/value store behind its
    -- `state` import. Deleted with the plugin. Plaintext to the nest -- what
    -- a plugin keeps here is its working state, never a user's key (the
    -- holder secret lives beside the module, above).
    CREATE TABLE IF NOT EXISTS plugin_state (
        principal_id BLOB NOT NULL,
        key          TEXT NOT NULL,
        value        BLOB NOT NULL,
        PRIMARY KEY (principal_id, key)
    );

    CREATE TABLE IF NOT EXISTS atproto_native_records (
        actor_id   BLOB NOT NULL,
        collection TEXT NOT NULL,
        rkey       TEXT NOT NULL,
        cid        TEXT NOT NULL,
        record     BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        deleted_at INTEGER,
        PRIMARY KEY (actor_id, collection, rkey)
    );

    CREATE TABLE IF NOT EXISTS atproto_preferences (
        actor_id    BLOB PRIMARY KEY,
        preferences BLOB NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS atproto_blobs (
        actor_id      BLOB NOT NULL,
        cid           TEXT NOT NULL,
        media_ref     BLOB NOT NULL,
        created_at    INTEGER NOT NULL,
        referenced_at INTEGER,
        PRIMARY KEY (actor_id, cid)
    );

    -- `integration_level` (S4-A) is the depth selector's stored user intent
    -- (`ui/atproto.md` § State & data shape): 'off' | 'linked' |
    -- 'hosted_visible' | 'hosted_full'. Defaulting to the ratified
    -- OFF consent posture, so a sparse row (the kill-switch path inserts one
    -- without a level) and an absent row read alike. No CHECK constraint on
    -- purpose: `reconcile_added_columns` re-adds a missing column from
    -- `pragma_table_info`, which cannot carry a CHECK, so one here would guard
    -- fresh databases only and leave every upgraded one silently unguarded.
    -- The uniform guard is `IntegrationLevel::from_wire` at the transition
    -- handler — the column's only writer.
    CREATE TABLE IF NOT EXISTS atproto_account_settings (
        actor_id              BLOB PRIMARY KEY,
        external_apps_enabled INTEGER NOT NULL DEFAULT 1,
        integration_level     TEXT NOT NULL DEFAULT 'off',
        updated_at            INTEGER NOT NULL
    );

    -- Bridge-wide sealed HS256 session-token secret (F1 remainder;
    -- atproto-pds-full.md § Key material inventory). Opaque ciphertext
    -- sealed to the bridge's attested x25519 (the bridge_tls_cert_blobs
    -- keying minus the domain); minted nest-side on first fetch
    -- (provision-on-read, the DKIM discipline). Key material — never
    -- dropped: losing the row invalidates every outstanding session token.
    CREATE TABLE IF NOT EXISTS atproto_session_secret_blobs (
        bridge_role TEXT NOT NULL,
        bridge_id   TEXT NOT NULL,
        blob        BLOB NOT NULL,
        created_at  INTEGER NOT NULL,
        PRIMARY KEY (bridge_role, bridge_id)
    );

    -- D10 delegated per-account authoring sub-key K (atproto-pds-full.md
    -- D10, key-material-hierarchy.md § Nest-internal key-encryption key).
    -- One Ed25519 sub-key per account, minted nest-side on first fetch
    -- (provision-on-read, first-write-wins — the session-secret discipline):
    -- `k_secret_wrapped` is the secret at rest, wrapped under the
    -- nest-internal KEK with its own domain-separated context (the Nostr /
    -- bunker key-crypto pattern — NOT sealed to the bridge x25519), `k_pub`
    -- the public key, `cert` the uploaded identity-signed DeviceAuthorization
    -- embed-as-bytes (NULL until provision). Key material, but the ONE
    -- deletable at-rest key state (D10 § Revocation): a client-driven
    -- revoke_authoring_delegation, an un-host, or an account deactivation
    -- DELETEs the row — recreatable, since re-enabling a hosted level
    -- re-mints a fresh K and re-provisions a fresh cert (already-published
    -- posts stay verifiable from their embedded cert).
    --
    -- `last_used_at` is ADVISORY ONLY (D10 § Audit, `atproto-pds-full.md`):
    -- epoch-millis of the last time an external app's write actually APPLIED
    -- under this delegation. It is nest-maintained, so it is emphatically NOT
    -- forensics — a compromised nest can under-report it at will, and it says
    -- nothing about attempts that were refused. It exists to help an owner
    -- notice a delegation they have forgotten about; the real audit surface is
    -- the delegated CONTENT, verified client-side (the same honest-bound
    -- framing `ui/nests.md`'s grant row uses). NULL = never used.
    CREATE TABLE IF NOT EXISTS atproto_authoring_keys (
        actor_id         BLOB PRIMARY KEY,
        k_pub            BLOB NOT NULL,
        k_secret_wrapped BLOB NOT NULL,
        cert             BLOB,
        created_at       INTEGER NOT NULL,
        cert_updated_at  INTEGER,
        last_used_at     INTEGER
    );

    -- F4 slice 6a — pending OAuth consent requests (D3 rung 2; the ceremony in
    -- atproto-pds-full.md § F4 detail's *Consent ceremony wire flow*). The
    -- bridge opens one when a browser reaches `/oauth/authorize`; the user's
    -- apps render it as an approval card and answer it over their own authed
    -- WS-RPC.
    --
    -- Why this is NEST state while the PAR request and the auth code stay
    -- bridge memory: the apps read and resolve it, and a WS-RPC kind is how
    -- they reach anything. Nothing here is precious (C6 still holds — the row
    -- is a minutes-long question, recreatable by retrying the flow), so an
    -- expired or swept row is not user data loss.
    --
    -- `actor_id` is NULLABLE by design: a PAR carrying no `login_hint` names no
    -- account, and § F4 detail's ceremony says such a request is still visible
    -- in-app. It is bound to whoever RESOLVES it — the approver's own
    -- identity is the answer to `which account is this grant for`.
    --
    -- `code` is the binding code the browser page and the approval card both
    -- display, minted HERE so the bridge structurally cannot show one the row
    -- does not hold. Display form (grouped/hyphenated) because both surfaces
    -- render it verbatim and nothing compares it programmatically -- the
    -- comparison is a human's.
    --
    -- `client_id` is stored verbatim and rendered verbatim: it is a URL, and
    -- deriving an origin from it here would be a second parser over the one
    -- string F4 already rules is parsed exactly once, by the component that
    -- dials it. `logo_uri` is deliberately ABSENT from this table -- see the
    -- ceremony bullet's ruling.
    --
    -- `approved` is NULL while pending; `resolved_at` NOT NULL is the
    -- resolution fact and `approved` is which way it went.
    CREATE TABLE IF NOT EXISTS atproto_consent_requests (
        consent_id  BLOB PRIMARY KEY,
        actor_id    BLOB,
        code        TEXT NOT NULL,
        client_id   TEXT NOT NULL,
        client_name TEXT,
        scopes      TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        expires_at  INTEGER NOT NULL,
        resolved_at INTEGER,
        approved    INTEGER,
        -- The permission sets behind `scopes` -- frozen as PAR expanded them
        -- -- JSON-encoded (`fauna_protocol::atproto_pds::ConsentSetInfo`).
        -- NULL on every request that named no set.
        --
        -- Opaque to SQL on purpose -- the nest never queries inside it. It is
        -- carried so the approval card can group the expansion under the set
        -- the user is being asked to trust -- and it is read back only whole.
        sets        TEXT,
        -- Which consent start opened the row (authorization-server.md
        -- § Consent): NULL for the browser start. 'typed_code' for the RFC
        -- 8628 device start -- unassigned and listed to nobody until the
        -- user who typed its code claims it. 'push' for the CIBA quiet push
        -- -- one live row per client and account and never listed unassigned.
        consent_start TEXT,
        -- The ceremony's attested keys (32 raw bytes each, NULL when none was
        -- presented) and the client document's kind manifest, the compact JWS
        -- verbatim (NULL for a document without one) -- carried so the card
        -- says what a records grant will wrap and to whom
        -- (third-party-kinds.md § The record doors). Display facts only: the
        -- principal row takes its keys from the code the same DPoP key redeems.
        holder_x25519  BLOB,
        writer_ed25519 BLOB,
        fauna_manifest TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_atproto_consent_pending
        ON atproto_consent_requests(actor_id, expires_at);

    -- The per-client block (authorization-server.md § Consent rule c): an
    -- account's 'never show requests from this app'. A quiet push from a
    -- blocked client opens nothing and answers exactly as an unresolved hint
    -- does. User-authored nest state -- read and removed only through the
    -- account's own kind -- so it is never swept.
    CREATE TABLE IF NOT EXISTS oauth_client_blocks (
        actor_id   BLOB NOT NULL,
        client_id  TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (actor_id, client_id)
    );
";

/// S2 (atproto-pds-bridge.md § State & data shape): per-user ATProto identity
/// rows (DID-as-data with provenance; nothing assumes minted-at-enable — a
/// future `imported` provenance lands additively) plus the sealed
/// bridge-custodied key blobs (canonical-CBOR `AtprotoIdentityBlob`, sealed to
/// the atproto.pds bridge's attested x25519 — nest stores opaque ciphertext,
/// the DKIM/TLS blob discipline).
pub(super) const MIGRATIONS_ATPROTO_IDENTITIES: &str = "
    CREATE TABLE IF NOT EXISTS atproto_identities (
        actor_id            BLOB PRIMARY KEY,
        method              TEXT NOT NULL CHECK (method IN ('plc', 'web')),
        -- 'deactivated' (S4-A) is layer-2's retained-but-inactive state: the
        -- DID and sealed keys survive a step-down so re-enabling restores the
        -- same identity. 'deleted' (S5 slice 5) is the stronger action's
        -- terminal state: the bridge sweeps every projected record, announces
        -- #account(deleted) and purges the repo — but the DID and sealed keys
        -- are retained here too, which is what makes § Disable & revocation's
        -- promise of a sweep that is still reversible in identity terms
        -- literally true. The fifth value is the terminal one (S5 slice 5b):
        -- the user's client published a PLC tombstone signed with their senior
        -- rotation key, so the DID itself is retired and no later operation can
        -- revive it. It is the one status this box cannot reach on its own --
        -- nest holds no key that could sign that op -- so the row only ever
        -- records what the client reports back.
        status              TEXT NOT NULL DEFAULT 'pending'
                                 CHECK (status IN ('pending', 'active', 'deactivated', 'deleted', 'tombstoned')),
        -- The user opted into permanently retiring the identity, inside the
        -- delete-presence ceremony (S5 slice 5b). Durable here rather than in
        -- the client because the act it authorizes spans a crash: the client
        -- signs and submits the tombstone on a later converge pass, once the
        -- sweep has finished, and without this row a client that died between
        -- the confirm and the submit would leave the user believing their
        -- identity was retired when it was not.
        tombstone_requested INTEGER NOT NULL DEFAULT 0,
        did                 TEXT,
        provenance          TEXT,
        user_rotation_pub   TEXT NOT NULL DEFAULT '',
        signing_pub         TEXT,
        bridge_rotation_pub TEXT,
        genesis_cid         TEXT,
        -- The user's history-backfill opt-in (S4-A), carried by the transition
        -- that enters a hosted level and consumed by S5's projection loop to
        -- pick a genesis watermark over a forward-only one. Default 0 = the
        -- ratified forward-only default.
        history_backfill    INTEGER NOT NULL DEFAULT 0,
        -- The projection watermark START derived from that opt-in at mint
        -- (S5 slice 2a, `atproto-pds-bridge.md` § Projection & backfill table):
        -- 0 = genesis (history opt-in), >0 = the enable instant in epoch MICROS
        -- (forward-only, the ratified default). `fetch_public_posts` filters the
        -- stream on it, so a post predating the user's consent never leaves this
        -- box. It is the stream's start, never a running clamp — see
        -- `list_public_projection_page`.
        projection_floor_micros INTEGER NOT NULL DEFAULT 0,
        created_at          INTEGER NOT NULL,
        updated_at          INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_atproto_identities_did
        ON atproto_identities(did) WHERE did IS NOT NULL;

    CREATE TABLE IF NOT EXISTS atproto_identity_key_blobs (
        actor_id    BLOB PRIMARY KEY,
        blob        BLOB NOT NULL,
        created_at  INTEGER NOT NULL
    );
";

/// S5 slice 5b follow-on: the append-only record of identities whose DID was
/// permanently retired at the PLC directory (`atproto-pds-bridge.md`
/// § Disable & revocation layer 2).
///
/// It exists because a retirement must not be a permanent product lockout. A
/// user who destroys their ATProto identity and later wants a new one should get
/// a new one, exactly as if they had never enabled — but `atproto_identities` is
/// one row per actor, so a second identity needs somewhere for the first to go.
///
/// **Never `DELETE`d and never updated.** A retired DID no longer resolves
/// anywhere, so this row is the last thing anyone can say about it: which DID
/// this actor published, under which keys, and when it ended. Losing that would
/// destroy user-irrecoverable provenance for the sake of a table's tidiness, and
/// unlike the repo it is not re-derivable from anything. A second retirement
/// appends a second row rather than overwriting the first, which is the whole
/// reason this is a table and not two columns on the live row.
///
/// `did` carries no UNIQUE index, deliberately: uniqueness belongs to the LIVE
/// identity (`idx_atproto_identities_did`), and a constraint here could only
/// ever refuse to record a fact that already happened.
pub(super) const MIGRATIONS_ATPROTO_RETIRED_IDENTITIES: &str = "
    CREATE TABLE IF NOT EXISTS atproto_retired_identities (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id            BLOB NOT NULL,
        method              TEXT NOT NULL,
        did                 TEXT NOT NULL,
        provenance          TEXT,
        user_rotation_pub   TEXT NOT NULL DEFAULT '',
        signing_pub         TEXT,
        bridge_rotation_pub TEXT,
        genesis_cid         TEXT,
        -- The consent floor the retired identity published under. Kept with the
        -- rest: it is the record of what the user had agreed to publish, and a
        -- fresh identity derives its own from its own enable instant.
        projection_floor_micros INTEGER NOT NULL DEFAULT 0,
        -- When the identity row was first created, carried over verbatim, and
        -- when it was archived here.
        created_at          INTEGER NOT NULL,
        retired_at          INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_atproto_retired_identities_actor
        ON atproto_retired_identities(actor_id);
";

/// The genesis, block by block, in the order [`apply_genesis`] runs it. Each
/// block is pure `CREATE … IF NOT EXISTS` DDL (the one exception is the region
/// tier's idempotent floor re-seed — see [`MIGRATIONS_REGION_TIER`]), so the
/// whole list re-runs on every boot.
const GENESIS_BLOCKS: &[(&str, &str)] = &[
    ("MIGRATIONS", MIGRATIONS),
    ("MIGRATIONS_SCHEMA_META", MIGRATIONS_SCHEMA_META),
    ("MIGRATIONS_EMAIL", MIGRATIONS_EMAIL),
    ("MIGRATIONS_WORKER", MIGRATIONS_WORKER),
    ("MIGRATIONS_RPC_IDEMPOTENCY", MIGRATIONS_RPC_IDEMPOTENCY),
    ("MIGRATIONS_BLOB", MIGRATIONS_BLOB),
    ("MIGRATIONS_SYNC", MIGRATIONS_SYNC),
    ("MIGRATIONS_ROOMS", MIGRATIONS_ROOMS),
    (
        "MIGRATIONS_BRIDGED_CONVERSATIONS",
        MIGRATIONS_BRIDGED_CONVERSATIONS,
    ),
    ("MIGRATIONS_FOLDERS", MIGRATIONS_FOLDERS),
    ("MIGRATIONS_KEY_PACKAGES", MIGRATIONS_KEY_PACKAGES),
    ("MIGRATIONS_CONTACTS", MIGRATIONS_CONTACTS),
    ("MIGRATIONS_INBOX_MODE", MIGRATIONS_INBOX_MODE),
    ("MIGRATIONS_FEEDS", MIGRATIONS_FEEDS),
    ("MIGRATIONS_SNAPSHOTS", MIGRATIONS_SNAPSHOTS),
    ("MIGRATIONS_SNAPSHOTS_V3", MIGRATIONS_SNAPSHOTS_V3),
    ("MIGRATIONS_EMAIL_FILTERS", MIGRATIONS_EMAIL_FILTERS),
    ("MIGRATIONS_REGISTRATION", MIGRATIONS_REGISTRATION),
    (
        "MIGRATIONS_BRIDGE_FEED_SUBSCRIPTIONS",
        MIGRATIONS_BRIDGE_FEED_SUBSCRIPTIONS,
    ),
    ("MIGRATIONS_EVICTION_TOKEN", MIGRATIONS_EVICTION_TOKEN),
    (
        "MIGRATIONS_FEED_GLOBAL_FACTORS",
        MIGRATIONS_FEED_GLOBAL_FACTORS,
    ),
    ("MIGRATIONS_FEED_CONTRIBUTORS", MIGRATIONS_FEED_CONTRIBUTORS),
    ("MIGRATIONS_OPERATION_LOCKS", MIGRATIONS_OPERATION_LOCKS),
    ("MIGRATIONS_CONTENT_LABELS", MIGRATIONS_CONTENT_LABELS),
    (
        "MIGRATIONS_OBLIGATION_ACTIONS",
        MIGRATIONS_OBLIGATION_ACTIONS,
    ),
    ("MIGRATIONS_ABUSE_REPORTS", MIGRATIONS_ABUSE_REPORTS),
    ("MIGRATIONS_SPAM_MODELS", MIGRATIONS_SPAM_MODELS),
    ("MIGRATIONS_SPAM_PREFERENCES", MIGRATIONS_SPAM_PREFERENCES),
    ("MIGRATIONS_SPAM_BASELINE", MIGRATIONS_SPAM_BASELINE),
    (
        "MIGRATIONS_SPAM_TRAINING_HISTORY",
        MIGRATIONS_SPAM_TRAINING_HISTORY,
    ),
    (
        "MIGRATIONS_SPAM_MODEL_HOLDER_COPIES",
        MIGRATIONS_SPAM_MODEL_HOLDER_COPIES,
    ),
    (
        "MIGRATIONS_PERSONALIZATION_MODELS",
        MIGRATIONS_PERSONALIZATION_MODELS,
    ),
    ("MIGRATIONS_CONTENT_REPORTS", MIGRATIONS_CONTENT_REPORTS),
    (
        "MIGRATIONS_PEER_CONTENT_TRENDS",
        MIGRATIONS_PEER_CONTENT_TRENDS,
    ),
    (
        "MIGRATIONS_MAIL_ACCOUNT_SETTINGS",
        MIGRATIONS_MAIL_ACCOUNT_SETTINGS,
    ),
    ("MIGRATIONS_FORWARD_QUEUE", MIGRATIONS_FORWARD_QUEUE),
    ("MIGRATIONS_MAIL_SRS_SECRETS", MIGRATIONS_MAIL_SRS_SECRETS),
    (
        "MIGRATIONS_MAIL_LIST_UNSUBSCRIBE_SECRET",
        MIGRATIONS_MAIL_LIST_UNSUBSCRIBE_SECRET,
    ),
    ("MIGRATIONS_AUTO_REPLY_LOG", MIGRATIONS_AUTO_REPLY_LOG),
    ("MIGRATIONS_SUBSCRIPTIONS", MIGRATIONS_SUBSCRIPTIONS),
    ("MIGRATIONS_PAYMENTS", MIGRATIONS_PAYMENTS),
    ("MIGRATIONS_MEMBERSHIP_TIERS", MIGRATIONS_MEMBERSHIP_TIERS),
    ("MIGRATIONS_NEST_SIGNING", MIGRATIONS_NEST_SIGNING),
    ("MIGRATIONS_NEST_NAT_MODE", MIGRATIONS_NEST_NAT_MODE),
    ("MIGRATIONS_ACTOR_CHANNELS", MIGRATIONS_ACTOR_CHANNELS),
    (
        "MIGRATIONS_CONV_ATTACHMENT_REFS",
        MIGRATIONS_CONV_ATTACHMENT_REFS,
    ),
    (
        "MIGRATIONS_CONV_RECORD_AUTHORS",
        MIGRATIONS_CONV_RECORD_AUTHORS,
    ),
    (
        "MIGRATIONS_BLOB_LEGAL_WITHHOLD",
        MIGRATIONS_BLOB_LEGAL_WITHHOLD,
    ),
    (
        "MIGRATIONS_LEGAL_TAKEDOWN_DELETED_POSTS",
        MIGRATIONS_LEGAL_TAKEDOWN_DELETED_POSTS,
    ),
    (
        "MIGRATIONS_CHANNEL_COMMIT_WATERMARK",
        MIGRATIONS_CHANNEL_COMMIT_WATERMARK,
    ),
    ("MIGRATIONS_EMAIL_DOMAINS", MIGRATIONS_EMAIL_DOMAINS),
    ("MIGRATIONS_SENDER_BEHAVIOR", MIGRATIONS_SENDER_BEHAVIOR),
    ("MIGRATIONS_BACKUP_SNAPSHOTS", MIGRATIONS_BACKUP_SNAPSHOTS),
    ("MIGRATIONS_ADMIN_ACTOR_IDS", MIGRATIONS_ADMIN_ACTOR_IDS),
    ("MIGRATIONS_SYNC_CONFLICTS", MIGRATIONS_SYNC_CONFLICTS),
    (
        "MIGRATIONS_SYNC_CONFLICT_CANDIDATES",
        MIGRATIONS_SYNC_CONFLICT_CANDIDATES,
    ),
    ("MIGRATIONS_PENDING_ACTIONS", MIGRATIONS_PENDING_ACTIONS),
    ("MIGRATIONS_ACTOR_LAST_IP", MIGRATIONS_ACTOR_LAST_IP),
    ("MIGRATIONS_RESTORE_HISTORY", MIGRATIONS_RESTORE_HISTORY),
    (
        "MIGRATIONS_BRIDGE_RESTORE_DIVERGENCE",
        MIGRATIONS_BRIDGE_RESTORE_DIVERGENCE,
    ),
    ("MIGRATIONS_NOTIFICATIONS", MIGRATIONS_NOTIFICATIONS),
    (
        "MIGRATIONS_PUSH_SUBSCRIPTIONS",
        MIGRATIONS_PUSH_SUBSCRIPTIONS,
    ),
    ("MIGRATIONS_INVITE_REQUESTS", MIGRATIONS_INVITE_REQUESTS),
    ("MIGRATIONS_FAMILY", MIGRATIONS_FAMILY),
    ("MIGRATIONS_GUARDIAN_MAIL", MIGRATIONS_GUARDIAN_MAIL),
    (
        "MIGRATIONS_GUARDIAN_TRANSFERS",
        MIGRATIONS_GUARDIAN_TRANSFERS,
    ),
    (
        "MIGRATIONS_GUARDIAN_CONTACT_REQUESTS",
        MIGRATIONS_GUARDIAN_CONTACT_REQUESTS,
    ),
    (
        "MIGRATIONS_GUARDIAN_FEED_REQUESTS",
        MIGRATIONS_GUARDIAN_FEED_REQUESTS,
    ),
    ("MIGRATIONS_GUARDIAN_DM_PEERS", MIGRATIONS_GUARDIAN_DM_PEERS),
    (
        "MIGRATIONS_GUARDIAN_CONTENT_NOTICES",
        MIGRATIONS_GUARDIAN_CONTENT_NOTICES,
    ),
    ("MIGRATIONS_GUARDIAN_USAGE", MIGRATIONS_GUARDIAN_USAGE),
    ("MIGRATIONS_ACCOUNT_AGE_BANDS", MIGRATIONS_ACCOUNT_AGE_BANDS),
    ("MIGRATIONS_BRIDGE_ABSORPTION", MIGRATIONS_BRIDGE_ABSORPTION),
    ("MIGRATIONS_CAPABILITY_GRANTS", MIGRATIONS_CAPABILITY_GRANTS),
    ("MIGRATIONS_NEST_BACKUP_KEYS", MIGRATIONS_NEST_BACKUP_KEYS),
    (
        "MIGRATIONS_BACKUP_WRITER_GRANTS",
        MIGRATIONS_BACKUP_WRITER_GRANTS,
    ),
    (
        "MIGRATIONS_BACKUP_DESTINATIONS",
        MIGRATIONS_BACKUP_DESTINATIONS,
    ),
    (
        "MIGRATIONS_BACKUP_CUSTODIAN_CHECKINS",
        MIGRATIONS_BACKUP_CUSTODIAN_CHECKINS,
    ),
    (
        "MIGRATIONS_BACKUP_DESTINATION_FOLDERS",
        MIGRATIONS_BACKUP_DESTINATION_FOLDERS,
    ),
    ("MIGRATIONS_CUSTODY_HOSTING", MIGRATIONS_CUSTODY_HOSTING),
    (
        "MIGRATIONS_CUSTODY_RECEIPTS_STAGED",
        MIGRATIONS_CUSTODY_RECEIPTS_STAGED,
    ),
    (
        "MIGRATIONS_RECOVERY_REGISTRATIONS",
        MIGRATIONS_RECOVERY_REGISTRATIONS,
    ),
    ("MIGRATIONS_RECOVERY_ESCROW", MIGRATIONS_RECOVERY_ESCROW),
    ("MIGRATIONS_RECOVERY_PENDING", MIGRATIONS_RECOVERY_PENDING),
    ("MIGRATIONS_ACTOR_SUCCESSIONS", MIGRATIONS_ACTOR_SUCCESSIONS),
    (
        "MIGRATIONS_SUCCESSION_OWED_NESTS",
        MIGRATIONS_SUCCESSION_OWED_NESTS,
    ),
    ("MIGRATIONS_NEST_ADDRESSES", MIGRATIONS_NEST_ADDRESSES),
    (
        "MIGRATIONS_FOREIGN_RECOVERY_HEADS",
        MIGRATIONS_FOREIGN_RECOVERY_HEADS,
    ),
    (
        "MIGRATIONS_WEB_RENDERED_SEALED",
        MIGRATIONS_WEB_RENDERED_SEALED,
    ),
    ("MIGRATIONS_WEB_RENDER_OWED", MIGRATIONS_WEB_RENDER_OWED),
    ("MIGRATIONS_WEB_RESTORE_OWED", MIGRATIONS_WEB_RESTORE_OWED),
    (
        "MIGRATIONS_WEB_RENDER_GENERATION",
        MIGRATIONS_WEB_RENDER_GENERATION,
    ),
    ("MIGRATIONS_WEB_RENDER_STAGED", MIGRATIONS_WEB_RENDER_STAGED),
    ("MIGRATIONS_BRIDGE_CARDDAV", MIGRATIONS_BRIDGE_CARDDAV),
    (
        "MIGRATIONS_ATPROTO_IDENTITIES",
        MIGRATIONS_ATPROTO_IDENTITIES,
    ),
    (
        "MIGRATIONS_ATPROTO_RETIRED_IDENTITIES",
        MIGRATIONS_ATPROTO_RETIRED_IDENTITIES,
    ),
    ("MIGRATIONS_ATPROTO_PDS", MIGRATIONS_ATPROTO_PDS),
    ("MIGRATIONS_OAUTH_ISSUER", MIGRATIONS_OAUTH_ISSUER),
    ("MIGRATIONS_MAIL_DOMAINS", MIGRATIONS_MAIL_DOMAINS),
    ("MIGRATIONS_MAIL_ALIASES", MIGRATIONS_MAIL_ALIASES),
    ("MIGRATIONS_MAIL_LISTS", MIGRATIONS_MAIL_LISTS),
    (
        "MIGRATIONS_MAIL_DOMAIN_RENAMES",
        MIGRATIONS_MAIL_DOMAIN_RENAMES,
    ),
    ("MIGRATIONS_MAIL_POLICY", MIGRATIONS_MAIL_POLICY),
    ("MIGRATIONS_TRANSPORT_POLICY", MIGRATIONS_TRANSPORT_POLICY),
    ("MIGRATIONS_MAIL_ENABLED", MIGRATIONS_MAIL_ENABLED),
    ("MIGRATIONS_CALDAV_ENABLED", MIGRATIONS_CALDAV_ENABLED),
    ("MIGRATIONS_CARDDAV_ENABLED", MIGRATIONS_CARDDAV_ENABLED),
    ("MIGRATIONS_WEBDAV_ENABLED", MIGRATIONS_WEBDAV_ENABLED),
    ("MIGRATIONS_CALDAV_PORT", MIGRATIONS_CALDAV_PORT),
    ("MIGRATIONS_SERVING_PORT", MIGRATIONS_SERVING_PORT),
    (
        "MIGRATIONS_MAIL_AUTO_ENABLE_NEW_USERS",
        MIGRATIONS_MAIL_AUTO_ENABLE_NEW_USERS,
    ),
    (
        "MIGRATIONS_NEST_REGISTRATION_MODE",
        MIGRATIONS_NEST_REGISTRATION_MODE,
    ),
    ("MIGRATIONS_NEST_SUBHANDLES", MIGRATIONS_NEST_SUBHANDLES),
    (
        "MIGRATIONS_NEST_AGE_VERIFICATION_REQUIRED",
        MIGRATIONS_NEST_AGE_VERIFICATION_REQUIRED,
    ),
    (
        "MIGRATIONS_NEST_MAX_STORAGE_BYTES",
        MIGRATIONS_NEST_MAX_STORAGE_BYTES,
    ),
    ("MIGRATIONS_NEST_CORS_ORIGINS", MIGRATIONS_NEST_CORS_ORIGINS),
    (
        "MIGRATIONS_NEST_WEB_APP_ORIGIN",
        MIGRATIONS_NEST_WEB_APP_ORIGIN,
    ),
    ("MIGRATIONS_WEB_APEX_ACTOR", MIGRATIONS_WEB_APEX_ACTOR),
    (
        "MIGRATIONS_WEB_SUBDOMAIN_ENABLED",
        MIGRATIONS_WEB_SUBDOMAIN_ENABLED,
    ),
    (
        "MIGRATIONS_ACTOR_MAIL_SERVING",
        MIGRATIONS_ACTOR_MAIL_SERVING,
    ),
    ("MIGRATIONS_IMPORT_SESSIONS", MIGRATIONS_IMPORT_SESSIONS),
    (
        "MIGRATIONS_ACTOR_MESSAGE_DEDUP",
        MIGRATIONS_ACTOR_MESSAGE_DEDUP,
    ),
    ("MIGRATIONS_EXPORT_SESSIONS", MIGRATIONS_EXPORT_SESSIONS),
    (
        "MIGRATIONS_MESSAGE_SCAN_RESULTS",
        MIGRATIONS_MESSAGE_SCAN_RESULTS,
    ),
    ("MIGRATIONS_CONTENT_SCORES", MIGRATIONS_CONTENT_SCORES),
    ("MIGRATIONS_MODEL_VERSIONS", MIGRATIONS_MODEL_VERSIONS),
    ("MIGRATIONS_LABELERS", MIGRATIONS_LABELERS),
    ("MIGRATIONS_GREYLIST_TUPLES", MIGRATIONS_GREYLIST_TUPLES),
    ("MIGRATIONS_NEST_HOST_ADDRESS", MIGRATIONS_NEST_HOST_ADDRESS),
    ("MIGRATIONS_SHARE_TOKENS", MIGRATIONS_SHARE_TOKENS),
    ("MIGRATIONS_BACKUP_CUSTODY", MIGRATIONS_BACKUP_CUSTODY),
    (
        "MIGRATIONS_BACKUP_CUSTODY_GENERATIONS",
        MIGRATIONS_BACKUP_CUSTODY_GENERATIONS,
    ),
    (
        "MIGRATIONS_FOLDER_CONTENT_KEYS",
        MIGRATIONS_FOLDER_CONTENT_KEYS,
    ),
    (
        "MIGRATIONS_FOLDER_CHANNEL_CLAIMS",
        MIGRATIONS_FOLDER_CHANNEL_CLAIMS,
    ),
    (
        "MIGRATIONS_FOLDER_MEMBER_ROLES",
        MIGRATIONS_FOLDER_MEMBER_ROLES,
    ),
    ("MIGRATIONS_FEATURE_GATE", MIGRATIONS_FEATURE_GATE),
    ("MIGRATIONS_REGION_TIER", MIGRATIONS_REGION_TIER),
    ("MIGRATIONS_VAPID_KEYPAIR", MIGRATIONS_VAPID_KEYPAIR),
    ("MIGRATIONS_NEST_REPLICA", MIGRATIONS_NEST_REPLICA),
    ("MIGRATIONS_DOMAIN_EXPIRY", MIGRATIONS_DOMAIN_EXPIRY),
    ("MIGRATIONS_GENERATION_ESCROW", MIGRATIONS_GENERATION_ESCROW),
    (
        "MIGRATIONS_MEDIA_TICKET_SECRET",
        MIGRATIONS_MEDIA_TICKET_SECRET,
    ),
    ("MIGRATIONS_MAIL_DKIM_KEYS", MIGRATIONS_MAIL_DKIM_KEYS),
    (
        "MIGRATIONS_FOLDER_DEPOSIT_INBOX",
        MIGRATIONS_FOLDER_DEPOSIT_INBOX,
    ),
    (
        "MIGRATIONS_SPAM_BASELINE_DELTA",
        MIGRATIONS_SPAM_BASELINE_DELTA,
    ),
];

/// Apply the genesis (§ Genesis in the module doc): the unified content tables,
/// then every block of [`GENESIS_BLOCKS`] in order.
///
/// **Data-free**, because it is also the throwaway reference schema
/// [`reconcile_added_columns`] diffs every table against. Seeded rows belong in
/// [`seed_genesis_rows`], which runs after the reconcile.
fn apply_genesis(conn: &Connection) -> Result<()> {
    // FK enforcement is per connection. `CacheDb::open` already sets it; this
    // covers every caller that runs the genesis on a bare connection (the
    // reference build, the test fixtures), so their schema behaves like the
    // served one.
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .context("enable foreign_keys")?;
    // The unified content tables come first: other blocks reference them.
    super::schema::apply_unified_schema(conn).context("apply the unified content schema")?;
    for (block, sql) in GENESIS_BLOCKS {
        conn.execute_batch(sql)
            .with_context(|| format!("apply genesis block {block}"))?;
    }
    Ok(())
}

/// The rows a nest database carries from its first boot: the built-in tiers,
/// and the deployment's own secrets — the mail SRS HMAC key (mail-forwarding
/// N3), the one-click-unsubscribe HMAC key (`mail-mass-mailing.md` § Token
/// format; 32 bytes = `fauna_mail::lists::UNSUBSCRIBE_SECRET_BYTES`) and the
/// nest's Ed25519 signing keypair — plus the replica id
/// ([`MIGRATIONS_NEST_REPLICA`]). Each is minted only when absent, so every
/// later boot is a no-op; the admin rotates the mail secrets and never sets
/// them.
fn seed_genesis_rows(conn: &Connection) -> Result<()> {
    conn.execute_batch(SEED_TIERS).context("seed tiers")?;
    let now = super::now_epoch_secs();
    for (table, what) in [
        ("mail_srs_secrets", "mail SRS secret"),
        ("mail_list_unsubscribe_secrets", "list-unsubscribe secret"),
    ] {
        let present: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .with_context(|| format!("read {what}"))?;
        if present == 0 {
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret).expect("getrandom failed");
            conn.execute(
                &format!("INSERT INTO {table} (secret, created_at) VALUES (?1, ?2)"),
                rusqlite::params![secret.as_slice(), now],
            )
            .with_context(|| format!("seed {what}"))?;
        }
    }
    let has_keypair: i64 = conn
        .query_row("SELECT COUNT(*) FROM nest_keypair", [], |r| r.get(0))
        .context("read nest keypair")?;
    if has_keypair == 0 {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("getrandom failed");
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret);
        let public_key = signing_key.verifying_key().to_bytes();
        conn.execute(
            "INSERT OR IGNORE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![secret.as_slice(), public_key.as_slice(), now],
        )
        .context("seed nest keypair")?;
    }
    let mut replica_id = [0u8; 16];
    getrandom::fill(&mut replica_id).expect("getrandom failed");
    conn.execute(
        "INSERT OR IGNORE INTO nest_replica (id, replica_id, created_at) VALUES (1, ?1, ?2)",
        rusqlite::params![replica_id.as_slice(), now],
    )
    .context("seed nest replica id")?;
    Ok(())
}

/// Reconcile the `model_versions` registry to this binary's built-in scorer
/// versions (capability-mediated content-processing design § 2.5). One row per
/// [`fauna_core::scoring::builtin_factor_versions`] entry, keyed by factor name.
///
/// **Monotonic:** each built-in factor's row is raised to
/// `max(existing, built-in)` — a code deploy that bumps a `scorer_version::*`
/// constant advances the registry (firing the re-score obligation for content
/// scored under the old version), while a runtime bump *above* the constant (a
/// future community-model load calling `upsert_model_version`) is never lowered.
/// **Idempotent:** re-running with an unchanged constant is a no-op (the `MAX`
/// leaves the version and the `CASE` leaves `updated_at`). Runs from
/// [`run_migrations`] only — the reference-schema build ([`apply_genesis`])
/// stays data-free.
fn seed_builtin_model_versions(conn: &Connection) -> Result<()> {
    let now = super::now_epoch_secs();
    for (model_kind, version) in fauna_core::scoring::builtin_factor_versions() {
        conn.execute(
            "INSERT INTO model_versions (model_kind, version, updated_at)
                 VALUES (?1, ?2, ?3)
             ON CONFLICT(model_kind) DO UPDATE SET
                 version    = MAX(model_versions.version, excluded.version),
                 updated_at = CASE WHEN excluded.version > model_versions.version
                                   THEN excluded.updated_at
                                   ELSE model_versions.updated_at END",
            rusqlite::params![model_kind, version as i64, now],
        )
        .context("seed built-in model version")?;
    }
    Ok(())
}

/// Bring a nest database to this binary's schema: the genesis, the additive
/// backstop, the genesis rows, the standing boot reconciles, and the version
/// stamp — in that order, every boot.
///
/// A column added to a block's `CREATE TABLE IF NOT EXISTS` — a no-op on a
/// database whose table already exists — still reaches that database, through
/// [`reconcile_added_columns`], with no hand-written `ALTER`. That split is the
/// structural fix for the class of outage where a forgotten `ALTER` crash-looped
/// the mail bridge against a live database — the off-box-only brick
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability
/// outlaws. The standing passes that follow each exist because a CURRENT writer
/// still produces their input; a pass written for a database some earlier
/// binary left behind does not belong here (§ Genesis).
pub fn run_migrations(conn: &Connection) -> Result<()> {
    apply_genesis(conn)?;
    reconcile_added_columns(conn)?;
    // A guarded one-shot step (§ Genesis, the non-additive bullet) goes here,
    // keyed on the version the database was stamped with before this boot.
    // None exists: the genesis is the only schema this line has had.
    // The file-format mark `check_genesis` reads. Stamped on every boot (a
    // header write, and a no-op once it holds this value).
    conn.execute_batch(&format!(
        "PRAGMA application_id = {NEST_DB_APPLICATION_ID};"
    ))
    .context("stamp the nest database's application_id")?;
    seed_genesis_rows(conn)?;
    // A code deploy that bumps a `scorer_version::*` constant advances the
    // registry here, firing the re-score obligation for content scored under
    // the old version. Monotonic and idempotent.
    seed_builtin_model_versions(conn)?;
    // Give every content record a feed coordinate: segment compaction
    // re-inserts its survivors at the `0` sentinel.
    backfill_segment_records_changed_seq(conn)?;
    // Enqueue the rank fan-out rows a grant missed: the standing self-heal for
    // a crash between an approve and its fan-out enqueue.
    reconcile_unlock_fanout_requests(conn)?;
    // The nest-computable hash companions a writer left unset (reserved
    // folders are inserted without a `name_hash`).
    reconcile_path_sealing_companions(conn)?;
    // Scrub resting plaintext wherever its sealed sibling now rests. A scrubbed
    // value's page stays on the freelist with its bytes intact (`secure_delete`
    // is off), so a boot that cleared anything rewrites the file: the byte-level
    // guarantee `tests/conformance_at_rest_byte_scan.rs` pins, not merely the
    // column-level one. Outside any transaction, which VACUUM requires.
    let scrubbed: usize = run_scrub_plaintext(conn)?.iter().map(|(_, n)| n).sum();
    if scrubbed > 0 {
        conn.execute_batch("VACUUM;")
            .context("VACUUM after the plaintext scrub")?;
        tracing::info!(scrubbed, "plaintext scrubbed where a sealed sibling rests");
    }
    // Re-derive every Search-corpus profile row from `users.handle` — the heal
    // for whatever a writer that bypasses `fts::sync_profile_row` leaves.
    // Derived data only, rebuildable from `users` at any time (see the fn's
    // doc).
    let healed = super::fts::reconcile_profile_rows(conn)?;
    if healed > 0 {
        tracing::info!(
            healed,
            "search corpus: profile rows re-derived from users.handle"
        );
    }
    // Stamp the schema version LAST, after everything above succeeds
    // (version-compatibility.md § 2.2). `CacheDb::open` already gated out the
    // `Incompatible` verdict before calling this, so the only verdicts that
    // reach here are `UpgradeOrCurrent` (stamp up to this binary) and
    // `NewerCompatible` (`record_schema_meta`'s guard declines to stamp down).
    record_schema_meta(conn)?;
    Ok(())
}

/// Refuse a database the genesis did not write, **before** anything touches it
/// (`CacheDb::open` calls this ahead of [`check_schema_compatibility`]).
///
/// The genesis stamps [`NEST_DB_APPLICATION_ID`] into the SQLite header, so a
/// database carrying any other `application_id` is either not a nest database
/// at all, or one written before the genesis existed — whose schema-version
/// numbers (1..=78) were retired with the history the genesis replaced and
/// would read as merely older than the genesis's own (§ Genesis). No migration exists
/// for either, so the open fails loudly and writes nothing. The one mark-less
/// file accepted is an EMPTY one: that is a nest's first boot, and the genesis
/// about to run stamps it.
pub fn check_genesis(conn: &Connection) -> Result<()> {
    let application_id: i32 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .context("read the database's application_id")?;
    if application_id == NEST_DB_APPLICATION_ID {
        return Ok(());
    }
    let objects: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
        .context("count schema objects")?;
    if application_id == 0 && objects == 0 {
        return Ok(());
    }
    Err(PreGenesisDatabase { application_id }.into())
}

/// Returned by [`check_genesis`] (so by `CacheDb::open`) for a database the
/// genesis did not write. Unlike [`SchemaIncompatible`] it has no degraded
/// serve: no nest binary of this schema line can ever operate such a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a nest database of this schema line (application_id={application_id:#010x}): \
     it predates the nest's genesis schema or was written by something else, and no \
     migration exists for it"
)]
pub struct PreGenesisDatabase {
    pub application_id: i32,
}

/// The boot-time schema-compatibility verdict (version-compatibility.md § 2.2),
/// comparing the DB's recorded `(schema_version, min_reader_version)` against
/// this binary's [`CURRENT_SCHEMA_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaVerdict {
    /// `db_v ≤ bin_v` — this binary is up-to-date with or newer than the DB.
    /// Run migrations to upgrade the DB forward, then restamp to this binary.
    UpgradeOrCurrent,
    /// `db_v > bin_v` **and** `db_min ≤ bin_v` — the DB is newer than this
    /// binary but only *additively* (within this binary's reader floor). Operate
    /// normally: `reconcile_added_columns` tolerates the extra columns
    /// (I2 backward-compat). Recording declines to restamp the version down.
    NewerCompatible,
    /// `db_v > bin_v` **and** `db_min > bin_v` — the DB carries a **breaking**
    /// change this binary predates. Surface the honest typed error and boot
    /// degraded; never run destructive migrations or fail lazily at first insert.
    Incompatible { db_v: u32, db_min: u32, bin_v: u32 },
}

/// Returned by [`crate::db::CacheDb::open`] when the on-disk DB carries a
/// breaking schema change this binary predates ([`SchemaVerdict::Incompatible`]).
/// It is **not** a generic open failure: the boot path downcasts it
/// (`err.downcast_ref::<SchemaIncompatible>()`) to enter the degraded
/// "needs-update" serve mode instead of crash-looping (off-box brick —
/// `nest/common.md` § Client-state recoverability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "incompatible nest database: it was written by a newer nest \
     (schema_version={db_v}, min_reader_version={db_min}) that requires a binary \
     at schema_version >= {db_min}, but this binary is at schema_version {bin_v} \
     — this nest binary must be updated"
)]
pub struct SchemaIncompatible {
    pub db_v: u32,
    pub db_min: u32,
    pub bin_v: u32,
}

/// Read the DB's recorded `(schema_version, min_reader_version)`. An absent
/// `schema_meta` table (a fresh DB — one the genesis did not write is refused
/// at open) or an absent row both read as
/// `(BASELINE_SCHEMA_VERSION, BASELINE_SCHEMA_VERSION)`. Thin wrapper over the
/// shared [`fauna_core::sqlite_schema_meta::read_schema_meta`] (the SQL every
/// native SQLite-backed store composing the scheme shares).
fn read_schema_meta(conn: &Connection) -> Result<(u32, u32)> {
    fauna_core::sqlite_schema_meta::read_schema_meta(conn, BASELINE_SCHEMA_VERSION)
}

/// The § 2.2 verdict as a pure function of the three numbers: a DB's stamped
/// `(schema_version, min_reader_version)` against a binary's
/// `CURRENT_SCHEMA_VERSION`. Unchanged since the scheme shipped (2026-06-15),
/// so it is also the rule every retired-lineage binary applies at its own
/// version — which is what lets a test pin how one of them reads the genesis.
pub fn classify_schema(db_v: u32, db_min: u32, bin_v: u32) -> SchemaVerdict {
    if db_v <= bin_v {
        SchemaVerdict::UpgradeOrCurrent
    } else if db_min <= bin_v {
        SchemaVerdict::NewerCompatible
    } else {
        SchemaVerdict::Incompatible {
            db_v,
            db_min,
            bin_v,
        }
    }
}

/// Compare the on-disk schema against this binary, **without mutating the DB**
/// (version-compatibility.md § 2.2 table). Called by `CacheDb::open` *before*
/// `run_migrations`, so an `Incompatible` DB is never migrated.
pub fn check_schema_compatibility(conn: &Connection) -> Result<SchemaVerdict> {
    let (db_v, db_min) = read_schema_meta(conn)?;
    Ok(classify_schema(db_v, db_min, CURRENT_SCHEMA_VERSION))
}

/// Stamp `(CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION, now)` into the
/// single `schema_meta` row. The `WHERE` on the upsert refuses to lower an
/// existing `schema_version`, so an older binary operating a `NewerCompatible`
/// DB does not restamp the version *down* (version-compatibility.md § 2.2 —
/// "Do not restamp down"); `min_reader_version` rides along only when the
/// version stamp does, so the breaking-change floor is never lowered either.
fn record_schema_meta(conn: &Connection) -> Result<()> {
    fauna_core::sqlite_schema_meta::record_schema_meta(
        conn,
        CURRENT_SCHEMA_VERSION,
        MIN_READER_SCHEMA_VERSION,
        super::now_epoch_secs(),
    )
    .context("record schema_meta")
}

/// Give every `segment_records` row a content-scope feed coordinate: each row
/// still sitting at the `0` sentinel gets the next free `changed_seq` **at the
/// top of its own `(scope_id, kind)` scope**, in `rowid` (append) order.
///
/// Why a backfill at all. The generalized feed's class-1 arm pages on
/// `changed_seq > cursor`, so a row left at `0` is one no walk can ever return
/// — invisible to a *dehydrating* replica, whose scope index comes from the
/// feed walk alone rather than from bulk segment adoption
/// (`account-data-plane.md` § Store logical schema). Deletes are safe either
/// way (a tombstone assigns a fresh coordinate), but "record exists" would
/// silently go missing, which is the failure this closes.
///
/// **Above the scope's current max, never renumbering.** A coordinate a replica
/// has already walked past must never be reused or lowered, or that replica's
/// frontier would skip the row; appending above the max keeps every assigned
/// value stable. The cost is that a replica which already holds the record sees
/// it as new — harmless, because applying `record-added` for a held record is
/// an idempotent index upsert.
///
/// Idempotent, and a standing self-heal rather than a one-shot: after a pass no
/// `0` remains, so a re-run is a no-op; and any row a future write path leaves
/// at `0` (a kind wired to the segment store before it is wired to the cursor)
/// is picked up at the next boot instead of staying invisible forever.
fn backfill_segment_records_changed_seq(conn: &Connection) -> Result<()> {
    // Materialised into a temp table first, deliberately: the assignment reads
    // `MAX(changed_seq)` from the very table the UPDATE writes, and SQLite makes
    // no promise about which of the two an in-flight correlated subquery sees.
    // Computing every (rowid → coordinate) pair before a single row moves takes
    // the question off the table.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS _changed_seq_backfill AS
            SELECT r.rowid AS rid,
                   (SELECT COALESCE(MAX(m.changed_seq), 0) FROM segment_records m
                     WHERE m.scope_id = r.scope_id AND m.kind = r.kind)
                   + ROW_NUMBER() OVER (
                        PARTITION BY r.scope_id, r.kind ORDER BY r.rowid) AS newseq
              FROM segment_records r
             WHERE r.changed_seq = 0;

         UPDATE segment_records
            SET changed_seq = (SELECT newseq FROM _changed_seq_backfill
                                WHERE rid = segment_records.rowid)
          WHERE rowid IN (SELECT rid FROM _changed_seq_backfill);

         DROP TABLE _changed_seq_backfill;",
    )
    .context("backfill segment_records.changed_seq")
}

/// Enqueue the `monetization.md:19` + `:126` rank fan-out rows any grant
/// missed: for every tier (each is the author's client's to mint), every
/// unexpired subscriber of the same author's tiers at or above its
/// rank who neither **readably** holds it nor has a pending request for it
/// gets a `payment_entitled` subscribe request, drained by the author's client
/// like any other (`bins/fauna-nest/src/db/subscriptions.rs::
/// enqueue_unlock_fanout` is the grant-time twin; the predicates mirror it,
/// including its boundary rules — see that doc comment for why each
/// is load-bearing).
///
/// Keyed on **readability** in lockstep with that twin: suppression needs an unexpired entitlement
/// *and* a delivered wrap, because a `subscribers` row alone is the social
/// edge, not proof anyone can read the tier. This half is what makes the fix
/// **structural rather than forward-only** — wrap-less rows already written by
/// deployed nests are healed here at the next boot, which no source-side fix
/// could reach.
///
/// Generalized 2026-07-30 with the grant-time twin:
/// before that this covered only `unlocks_post`-designated targets, so the
/// self-heal was as blind to the undesignated half of the cascade as the
/// enqueue it heals. Widening it here is what keeps "the predicates mirror it"
/// true — a narrower reconcile would silently stop being the standing net for
/// exactly the rows the fix added.
///
/// **Excludes a `hidden` target tier (`d.hidden = 0`)**: without it, every boot would re-open
/// the exact hole the grant-time twin's rule 6 closes — a `subscribers` row
/// a prior, unpatched nest wrote onto a hidden tier (or one hand-crafted
/// against `subscribe_requests` directly) would be re-enqueued forever by the
/// very sweep meant to heal missed grants, not suppress illegitimate ones.
///
/// This is the standing boot-time self-heal (`monetization.md` §
/// Implementation status (2d) obligation (iv)) for a crash between an
/// approve and its best-effort fan-out enqueue — a subscriber granted but
/// never enqueued is silently locked out of posts their subscription
/// promised them — and it is **idempotent by construction** (the NOT-EXISTS
/// guards make a re-run a no-op). Re-adding a subscriber an
/// author manually removed from an unlock tier is model-correct: their
/// unexpired qualifying subscription *is* the entitlement
/// (`monetization.md:126`); revoking it means lapsing or removing the
/// qualifying tier, not fighting the reconcile.
fn reconcile_unlock_fanout_requests(conn: &Connection) -> Result<()> {
    let now = super::now_epoch_secs();
    // Candidate (target tier, subscriber) pairs, with the entitlement half of
    // the suppression carried as `holds_unexpired`. Readability — the other
    // half, and the one SQL cannot see (`subscriptions::wrapped_subscribers`)
    // — is applied below. Ordered by target tier so each tier's blob is
    // decoded **once** for its whole subscriber run, never once per pair.
    let mut stmt = conn
        .prepare(
            "SELECT d.author_id, d.name, s.subscriber_id,
                    EXISTS (
                      SELECT 1 FROM subscribers x
                       WHERE x.author_id = d.author_id
                         AND x.subscriber_id = s.subscriber_id
                         AND x.tier_name = d.name
                         AND (x.valid_until IS NULL OR x.valid_until > ?1))
               FROM subscription_tiers d
               JOIN subscription_tiers r
                 ON r.author_id = d.author_id
                AND r.rank >= d.rank
                AND r.name <> d.name
                AND (r.unlocks_post IS NULL OR d.unlocks_post IS NULL)
               JOIN subscribers s
                 ON s.author_id = r.author_id AND s.tier_name = r.name
              WHERE d.hidden = 0
                AND (s.valid_until IS NULL OR s.valid_until > ?1)
                AND NOT EXISTS (
                     SELECT 1 FROM subscribe_requests q
                      WHERE q.author_id = d.author_id
                        AND q.subscriber_id = s.subscriber_id
                        AND q.tier_name = d.name
                        AND q.kind = 'subscribe')
              GROUP BY d.author_id, d.name, s.subscriber_id
              ORDER BY d.author_id, d.name",
        )
        .context("prepare unlock fan-out reconcile candidates")?;
    let candidates: Vec<(Vec<u8>, String, Vec<u8>, bool)> = stmt
        .query_map(rusqlite::params![now], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })
        .context("query unlock fan-out reconcile candidates")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("collect unlock fan-out reconcile candidates")?;
    drop(stmt);

    let mut wrapped_for: Option<((Vec<u8>, String), std::collections::HashSet<[u8; 32]>)> = None;
    for (author_id, name, subscriber_id, holds_unexpired) in candidates {
        let (Ok(author), Ok(subscriber)) = (
            <[u8; 32]>::try_from(author_id.as_slice()),
            <[u8; 32]>::try_from(subscriber_id.as_slice()),
        ) else {
            continue;
        };
        if holds_unexpired {
            let key = (author_id.clone(), name.clone());
            if wrapped_for.as_ref().is_none_or(|(k, _)| k != &key) {
                wrapped_for = Some((
                    key,
                    super::subscriptions::wrapped_subscribers(conn, &author, &name),
                ));
            }
            if wrapped_for
                .as_ref()
                .is_some_and(|(_, set)| set.contains(&subscriber))
            {
                continue;
            }
        }
        conn.execute(
            "INSERT INTO subscribe_requests
                 (author_id, subscriber_id, tier_name, created_at, kind,
                  payment_entitled, valid_until)
             VALUES (?1, ?2, ?3, ?4, 'subscribe', 1, NULL)
             ON CONFLICT(author_id, subscriber_id, tier_name, kind) DO NOTHING",
            rusqlite::params![author.as_slice(), subscriber.as_slice(), name, now],
        )
        .context("reconcile unlock fan-out requests")?;
    }
    Ok(())
}

/// Fill in the **hash companions** a writer left unset
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
///
/// The *hash* halves — and only those — are nest-computable: each derives from
/// its resting plaintext, so a server-side pass can supply one wherever a
/// writer stored the plaintext without it. Reserved folders are the standing
/// case: they are inserted by name alone (`db/snapshots.rs`), and are hashed
/// here on the next boot. The *sealed* halves are client-keyed and are never
/// filled in here (the nest holds no key). `sync_conflicts.path_hash` is not
/// a case: its one writer stamps it at insert and the column is `NOT NULL`
/// (the pre-backfill fill this pass once ran for it went with the
/// compat-remnant sweep).
///
/// **Idempotent by construction**: every statement is `WHERE <companion> IS
/// NULL` over a deterministic derivation, so a re-run is a no-op. Nothing is
/// ever overwritten — a populated companion is the client's or a prior run's,
/// and both are the same value.
fn reconcile_path_sealing_companions(conn: &Connection) -> Result<()> {
    // `folders.name_hash`. Reserved `__` rows are hashed too: harmless, since
    // they stay addressed by their literal routing-constant name and never seal.
    let sets: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, name FROM folders WHERE name_hash IS NULL")
            .context("read folders needing a name_hash")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .context("read folders needing a name_hash")?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for (id, name) in sets {
        conn.execute(
            "UPDATE folders SET name_hash = ?1 WHERE id = ?2",
            rusqlite::params![fauna_core::path_crypto::set_name_hash(&name).to_vec(), id],
        )
        .context("fill in folders.name_hash")?;
    }

    // `import_sessions.source_hash`.
    let sessions: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, source_descriptor FROM import_sessions \
                 WHERE source_hash IS NULL",
            )
            .context("read import_sessions needing a source_hash")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .context("read import_sessions needing a source_hash")?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for (session_id, descriptor) in sessions {
        conn.execute(
            "UPDATE import_sessions SET source_hash = ?1 WHERE session_id = ?2",
            rusqlite::params![
                fauna_core::path_crypto::import_source_hash(&descriptor).to_vec(),
                session_id
            ],
        )
        .context("fill in import_sessions.source_hash")?;
    }
    Ok(())
}

/// The folders whose names and paths rest plaintext by ratified design — the
/// SQL twin of `FolderRow::rests_plaintext_paths` (`encryption-at-rest.md`
/// § Carve-outs), as a correlated `SELECT` over `folders f` filtered by the
/// caller's `$correlation` literal. A macro so the scrub's `concat!` spells
/// the class once; `db::tests::the_scrub_s_plaintext_class_is_the_folder_row_predicate`
/// pins it against the Rust predicate over every audience.
macro_rules! plaintext_paths_folder_sql {
    ($correlation:literal) => {
        concat!(
            "SELECT 1 FROM folders f WHERE ",
            $correlation,
            " AND f.audience = 'public'"
        )
    };
}
#[cfg(all(test, feature = "test-hooks"))]
pub(crate) use plaintext_paths_folder_sql;

/// The folders whose plaintext name may NOT rest — the guard both
/// [`SCRUB_PLANES`]' `folders.name` plane and [`BLANK_SEALED_FOLDER_NAME`]
/// share, so the boot pass and the write path cannot disagree on a class: the
/// seal and the hash it is addressed by both rest, the name is no reserved `__`
/// routing constant, and the folder is not `public` (a public folder's name is
/// its URL segment — `encryption-at-rest.md` § Carve-outs).
macro_rules! folder_name_rests_sealed_sql {
    () => {
        "name_sealed IS NOT NULL AND name_hash IS NOT NULL \
         AND substr(COALESCE(name, ''), 1, 2) <> '__' AND audience IS NOT 'public'"
    };
}

/// Rest one folder's name NULL when [`folder_name_rests_sealed_sql!`] says it
/// may not rest plaintext — run by every folders writer after its write
/// (`?1` = the row id), so a sealed set never rests its name past the write
/// that sealed it.
pub(super) const BLANK_SEALED_FOLDER_NAME: &str = concat!(
    "UPDATE folders SET name = NULL WHERE id = ?1 AND name IS NOT NULL AND ",
    folder_name_rests_sealed_sql!()
);

/// The plaintext-scrub UPDATE set — every plane whose resting plaintext is
/// removable once its sealed sibling rests, run by [`run_migrations`] on
/// **every** boot (`file-sync.md` § Sealed names & paths).
///
/// Shape rules, each load-bearing:
/// - **`WHERE <sealed> IS NOT NULL` is the whole predicate** (but for the
///   plaintext-paths class exemption below). A row with no
///   sealed sibling keeps its plaintext — that is what protects the
///   machine-authored device labels no writer ever seals (the WebDAV
///   pseudo-device, the self-heal placeholder, the backup coordinator) and
///   every reserved-rail row a machine writer records sealless.
/// - **Nullable planes scrub to `NULL`; `NOT NULL` planes scrub to `''`** —
///   the ratified scrub sentinel every render seam already reads as
///   "scrubbed" and degrades to `Omit` (`label_custody`'s empty-plaintext
///   contract).
/// - **`folders.retention_policy` is deliberately ABSENT** despite having a
///   sealed sibling (S6-e): the ARMED auto-prune ruling
///   (`encryption-at-rest.md` § Carve-outs, 2026-08-01) makes the nest parse
///   its numeric knobs server-side — the plaintext the nest parses is not a
///   label, the sealed sibling is the display copy, and scrubbing it would
///   silently re-darken scheduled pruning.
/// - **The two `path` planes of a folder whose paths rest plaintext BY DESIGN
///   are exempt** — [`plaintext_paths_folder_sql!`], the SQL twin of
///   `FolderRow::rests_plaintext_paths` (a `public` audience —
///   the legacy `web` mode spelling retired 2026-09-28). Both write rails rest such a row's plaintext path on
///   purpose, and a seal can rest beside it (an owner engine still sealing
///   across a flip to public stores its envelope as sent); a public
///   follower's projection withholds that seal, so scrubbing the plaintext
///   would leave the follower a row with neither, refused as `NoSeal`
///   (`path-sealing.md` § the S9 scrub contract). The predicate reads the folder's
///   CURRENT class, so a folder flipped back to private scrubs on the next
///   boot.
/// - **`folders.name` scrubs to `NULL`** (nullable since schema 114, so any
///   number of blanked sets sit under one owner's `UNIQUE(name, actor_id)`)
///   under [`folder_name_rests_sealed_sql!`]'s guards: never a reserved `__`
///   routing constant, never a `public` folder (its name is its URL segment),
///   never a row without the `name_hash` every address resolves by. The same
///   predicate blanks one row at a write ([`BLANK_SEALED_FOLDER_NAME`]), so the
///   boot pass only ever finds what a crash between a write and its blank left.
pub(super) const SCRUB_PLANES: &[(&str, &str)] = &[
    (
        "folders.name",
        concat!(
            "UPDATE folders SET name = NULL WHERE name IS NOT NULL AND ",
            folder_name_rests_sealed_sql!()
        ),
    ),
    (
        "sync_changes.path",
        concat!(
            "UPDATE sync_changes SET path = NULL WHERE path_sealed IS NOT NULL AND path IS NOT NULL
               AND NOT EXISTS (",
            plaintext_paths_folder_sql!("f.id = sync_changes.folder_id"),
            ")"
        ),
    ),
    (
        "backup_custody.path",
        "UPDATE backup_custody SET path = NULL WHERE path_sealed IS NOT NULL AND path IS NOT NULL",
    ),
    (
        "folders.include_paths",
        "UPDATE folders SET include_paths = NULL WHERE include_paths_sealed IS NOT NULL AND include_paths IS NOT NULL",
    ),
    (
        "folders.exclude_paths",
        "UPDATE folders SET exclude_paths = NULL WHERE exclude_paths_sealed IS NOT NULL AND exclude_paths IS NOT NULL",
    ),
    (
        "sync_conflicts.details",
        "UPDATE sync_conflicts SET details = NULL WHERE details_sealed IS NOT NULL AND details IS NOT NULL",
    ),
    (
        "sync_conflicts.path",
        concat!(
            "UPDATE sync_conflicts SET path = '' WHERE path_sealed IS NOT NULL AND path <> ''
               AND NOT EXISTS (",
            plaintext_paths_folder_sql!("f.id = sync_conflicts.folder_id"),
            ")"
        ),
    ),
    (
        "sync_devices.label",
        "UPDATE sync_devices SET label = '' WHERE label_sealed IS NOT NULL AND label <> ''",
    ),
    (
        // Safe to scrub because the per-source lock is keyed on `source_hash`
        // (`idx_import_sessions_source_hash_lock`), never on the plaintext.
        "import_sessions.source_descriptor",
        "UPDATE import_sessions SET source_descriptor = '' WHERE source_sealed IS NOT NULL AND source_descriptor <> ''",
    ),
];

/// Run [`SCRUB_PLANES`], returning `(plane label, rows scrubbed)` per plane.
/// Idempotent — a clean DB pays one no-op UPDATE per plane. Sync so the boot
/// runner can call it on the migration `Connection`; the async
/// [`super::CacheDb::scrub_plaintext_where_sealed`] test observation surface
/// wraps this same function.
pub(super) fn run_scrub_plaintext(conn: &Connection) -> Result<Vec<(&'static str, usize)>> {
    let mut report = Vec::with_capacity(SCRUB_PLANES.len());
    for (plane, sql) in SCRUB_PLANES {
        let scrubbed = conn
            .execute(sql, [])
            .with_context(|| format!("scrub_plaintext_where_sealed: {plane}"))?;
        report.push((*plane, scrubbed));
    }
    Ok(report)
}

/// The additive backstop: diff every table against a throwaway genesis and
/// `ALTER … ADD COLUMN` any nullable or constant-default column this database
/// lacks (the shared [`fauna_core::sqlite_schema_meta::reconcile_added_columns`],
/// which `fauna-mls` calls for `mls.db` too). A `NOT NULL` column without a
/// default cannot be added this way and fails the boot loudly — a non-additive
/// change is a guarded step, never a reconcile (§ Genesis).
fn reconcile_added_columns(conn: &Connection) -> Result<()> {
    fauna_core::sqlite_schema_meta::reconcile_added_columns(conn, apply_genesis)
}

/// Greylist deferral state, keyed by the `(sender_domain, recipient, subnet)`
/// tuple (`smtp-server.md:160–164`, `:172`). Timestamps are **Unix seconds**
/// (greylisting is coarse-grained: 60 s / 4 h / 30 d windows), matching the
/// nest's `outbound_now()`/`now_epoch_secs()` clock so the e2e clock hook can
/// advance them. `accepted_at` is NULL until a retry passes; the periodic GC
/// prunes accepted rows past the whitelist window and un-accepted rows past the
/// retry window.
pub(super) const MIGRATIONS_GREYLIST_TUPLES: &str = "
    CREATE TABLE IF NOT EXISTS greylist_tuples (
        sender_domain  TEXT    NOT NULL,
        recipient      TEXT    NOT NULL,
        subnet         TEXT    NOT NULL,
        first_seen     INTEGER NOT NULL,
        last_attempt   INTEGER NOT NULL,
        accepted_at    INTEGER,
        PRIMARY KEY (sender_domain, recipient, subnet)
    );
    CREATE INDEX IF NOT EXISTS idx_greylist_tuples_last_attempt
        ON greylist_tuples(last_attempt);
";

/// Per-message content-scan results. SQLite mapping of the goal-doc shape
/// (`mail-content-scanning.md` § Per-message scan-result storage): UUID→BLOB
/// (the 32-byte ingest message_id), TIMESTAMP→INTEGER, NUMERIC→INTEGER
/// milli-ints (no dag-cbor floats), JSONB / TEXT[]→TEXT (JSON). `message_id`
/// is the deterministic ingest pointer, so the insert is idempotent on retry.
/// `delivered_to_actor` is NULL only for reject-at-perimeter forensic rows
/// (written via the separate report path in T3).
pub(super) const MIGRATIONS_MESSAGE_SCAN_RESULTS: &str = "
    CREATE TABLE IF NOT EXISTS message_scan_results (
        message_id             BLOB    PRIMARY KEY,
        received_at            INTEGER NOT NULL,
        direction              TEXT    NOT NULL DEFAULT 'inbound',
        clamav_verdict         TEXT    NOT NULL,
        clamav_signature       TEXT,
        rspamd_score_raw       INTEGER,
        rspamd_score_scaled    INTEGER,
        rspamd_flagged_rules   TEXT,
        rspamd_score_breakdown TEXT,
        scanned_at             INTEGER NOT NULL,
        action_taken           TEXT    NOT NULL,
        delivered_to_actor     BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_message_scan_results_received
        ON message_scan_results(received_at);
    CREATE INDEX IF NOT EXISTS idx_message_scan_results_actor
        ON message_scan_results(delivered_to_actor, received_at);
    CREATE INDEX IF NOT EXISTS idx_message_scan_results_clamav
        ON message_scan_results(clamav_verdict, received_at);
";

/// The uniform scoring-metadata bus at rest — one row per (item, factor),
/// the multi-factor generalization of the per-kind score columns
/// (`content-scoring.md` § The scoring-metadata bus; frame Q1,
/// `content-moderation-and-ranking.md` § Resolved design decisions).
/// Metadata only (score / tier / version — never content bytes); deployment-data
/// plaintext in both storage modes, like `message_scan_results`. Writing a row
/// is the `content.label-write` operation at the key-access layer (enforcement
/// ships with the capability substrate). `scorer_version` is the watermark the
/// re-score drain compares against the model-version registry. The per-kind columns
/// (`message_scan_results.*`, `segment_records` spam mirrors) are the *detail
/// record* beside these rows — richer than a row (signature, rule breakdown,
/// DMARC policy, disposition) and ratified to stay at the bus's contract phase
/// (`content-scoring.md` § The scoring-metadata bus); nothing here is derived
/// from them. `content_kind` is 'mail' today; other kinds join without schema
/// change.
pub(super) const MIGRATIONS_CONTENT_SCORES: &str = "
    CREATE TABLE IF NOT EXISTS content_scores (
        content_id     BLOB    NOT NULL,
        content_kind   TEXT    NOT NULL DEFAULT 'mail',
        factor         TEXT    NOT NULL,
        score          INTEGER NOT NULL,
        tier           INTEGER NOT NULL,
        scorer_version INTEGER NOT NULL,
        scored_at      INTEGER NOT NULL,
        actor_id       BLOB,
        PRIMARY KEY (content_id, factor)
    );
    CREATE INDEX IF NOT EXISTS idx_content_scores_actor
        ON content_scores(actor_id, scored_at);
    CREATE INDEX IF NOT EXISTS idx_content_scores_factor_version
        ON content_scores(factor, scorer_version);
";

/// The model-version registry (capability-mediated content-processing design
/// § 2.5): the deployment-wide *current* version of every transparent scorer /
/// index model, keyed by `model_kind` — a scoring-bus factor name (the
/// `content_scores.factor` / `fauna_core::scoring::ScoreEntry.factor` namespace:
/// spam / clamav / rspamd / auth_*) or a future index axis key. It is the
/// *durable target* of the re-score obligation; the *durable progress* is the
/// per-content `scorer_version` watermark on `content_scores`. The obligation is
/// the GAP (design § 2.5 step 3), derived at drain time by
/// `db::model_versions::content_scores_behind`. Seeded/reconciled from the
/// built-in `scorer_version::*` constants by `seed_builtin_model_versions` in
/// `run_migrations`. Content-free metadata (no content key — KMH rule #4/#7).
pub(super) const MIGRATIONS_MODEL_VERSIONS: &str = "
    CREATE TABLE IF NOT EXISTS model_versions (
        model_kind TEXT    NOT NULL PRIMARY KEY,
        version    INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
";

// The community-labeler publish/subscribe registry (labeler-registry design
// § 3). No user-irrecoverable data (a labeler is publisher-republishable; a
// subscription row is client-reconstructible) — but never dropped (a
// subscription row *is* the user's choice; no-user-data-loss invariant). The
// `labelers` columns beside the opaque `metadata_blob` /
// `wasm_bytes` are projections of the canonical-CBOR `AlgorithmLabeler`,
// indexed for `list`, the way `capability_grants` indexes `holder_pubkey`.
pub(super) const MIGRATIONS_LABELERS: &str = "
    CREATE TABLE IF NOT EXISTS labelers (
        labeler_id       BLOB    NOT NULL PRIMARY KEY,
        version          INTEGER NOT NULL,
        publisher_actor  BLOB    NOT NULL,
        content_kind     TEXT    NOT NULL,
        factor           TEXT    NOT NULL,
        wasm_hash        BLOB    NOT NULL,
        wasm_size        INTEGER NOT NULL,
        metadata_blob    BLOB    NOT NULL,
        wasm_bytes       BLOB    NOT NULL,
        updated_at       INTEGER NOT NULL,
        -- The authenticated router actor_id that published this row (security
        -- review F1). `publisher_actor` is the self-signed
        -- `algorithm_id` keypair — free and off-box-rotatable -- so a
        -- per-`publisher_actor` cap is evadable; the per-CALLER cap keys on
        -- this un-rotatable identity. NULL = un-attributed (counted toward no
        -- caller's cap).
        caller_actor     BLOB,
        -- The artifact kind: 'wasm' (an executable label() module) or 'list'
        -- (a dag-cbor content_id→score map; design Block A / D8).
        artifact_kind    TEXT    NOT NULL DEFAULT 'wasm',
        -- A 'text-model' artifact's tokenizer/schema `version`, read off the
        -- artifact at the publish gate (which already decodes it to validate
        -- it) so the metadata-only `list` browse can carry it -- the version
        -- itself lives inside the artifact bytes, which only `inspect`
        -- returns. 0 = not applicable ('wasm'/'list').
        artifact_version INTEGER NOT NULL DEFAULT 0
    );
    -- The List-kind id→score projection (design Block A, D11): DERIVED data,
    -- decoded from `labelers.wasm_bytes` at publish (replaced in the same txn),
    -- so it is recreatable and could be dropped/rebuilt without user-data loss.
    -- It exists so subscribe-time materialization, republish resync, and the
    -- post-arrival join are indexed lookups instead of 1 MiB blob decodes.
    CREATE TABLE IF NOT EXISTS labeler_list_entries (
        labeler_id       BLOB    NOT NULL,
        content_id       BLOB    NOT NULL,
        score            INTEGER NOT NULL,
        PRIMARY KEY (labeler_id, content_id)
    );
    CREATE INDEX IF NOT EXISTS idx_labeler_list_entries_content
        ON labeler_list_entries(content_id);
    CREATE TABLE IF NOT EXISTS labeler_subscriptions (
        owner_actor      BLOB    NOT NULL,
        labeler_id       BLOB    NOT NULL,
        grant_id         BLOB,
        subscribed_ver   INTEGER NOT NULL,
        created_at       INTEGER NOT NULL,
        PRIMARY KEY (owner_actor, labeler_id)
    );
    CREATE INDEX IF NOT EXISTS idx_labeler_subs_labeler
        ON labeler_subscriptions(labeler_id);
";

pub(super) const MIGRATIONS_MAIL_DOMAINS: &str = "
    CREATE TABLE IF NOT EXISTS mail_domains (
        domain_id              BLOB    PRIMARY KEY,
        domain_name            TEXT    NOT NULL,
        is_primary             INTEGER NOT NULL DEFAULT 0,
        added_at               INTEGER NOT NULL,
        removed_at             INTEGER,
        restored_at            INTEGER,
        dkim_selector          TEXT,
        dkim_rotation_days     INTEGER,
        dkim_algorithms        TEXT    NOT NULL DEFAULT '[\"ed25519\",\"rsa-2048\"]',
        mta_sts_mode           TEXT    NOT NULL DEFAULT 'enforce',
        mta_sts_max_age_seconds INTEGER NOT NULL DEFAULT 86400,
        mta_sts_cert_mode      TEXT    NOT NULL DEFAULT 'expand_primary',
        catch_all_actor_id     BLOB,
        role_address_overrides TEXT,
        dmarc_overrides        TEXT,
        spf_record             TEXT    NOT NULL DEFAULT 'v=spf1 mx ~all',
        dkim_selector_activated_at INTEGER,
        -- Epoch-ms when a succession ceremony or the boot reconcile last
        -- cleared `catch_all_actor_id` because it named a retired identity
        -- (row 242, succession-aftermath.md § Re-key scope). NULL when the
        -- catch-all was never set, or was last set/cleared by an admin.
        catch_all_cleared_by_succession_at INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_mail_domains_active_name
        ON mail_domains(domain_name) WHERE removed_at IS NULL;
    CREATE UNIQUE INDEX IF NOT EXISTS idx_mail_domains_active_primary
        ON mail_domains(is_primary) WHERE removed_at IS NULL AND is_primary = 1;
";

// Per-account alias storage. See `docs/goal/behavior/mail-aliases.md`
// § Storage for the full shape (column-by-column rationale + the 5
// alias kinds). Only `kind='exact'` has a production writer + reader
// today; kinds 2-5, per-alias controls, the resolver order, and
// `alias_hits` population are deferred to a later slice.
pub(super) const MIGRATIONS_MAIL_ALIASES: &str = "
    CREATE TABLE IF NOT EXISTS account_aliases (
        alias_id                 BLOB    PRIMARY KEY,
        actor_id                 BLOB    NOT NULL,
        local_domain             TEXT    NOT NULL,
        kind                     TEXT    NOT NULL,
        pattern                  TEXT    NOT NULL,
        forward_target           TEXT,
        label                    TEXT    NOT NULL DEFAULT '',
        disabled                 INTEGER NOT NULL DEFAULT 0,
        spam_threshold_override  REAL,
        rate_limit_per_hour      INTEGER,
        rate_limit_per_day       INTEGER,
        uses_remaining           INTEGER,
        expires_at               INTEGER,
        created_at               INTEGER NOT NULL,
        last_hit_at              INTEGER,
        hit_count                INTEGER NOT NULL DEFAULT 0,
        UNIQUE (local_domain, pattern, kind)
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_account_aliases_exact
        ON account_aliases(local_domain, pattern) WHERE kind = 'exact';
    CREATE INDEX IF NOT EXISTS idx_account_aliases_disposable
        ON account_aliases(local_domain, pattern) WHERE kind = 'disposable';
    -- Admin external forwarders (`mail-aliases.md` § Kind 7): the exact-key
    -- (local_domain, pattern) lookup at resolution step 2 + the exact↔forwarder
    -- collision checks. `forward_target` holds the external destination (NULL
    -- for every non-forwarder kind).
    CREATE UNIQUE INDEX IF NOT EXISTS idx_account_aliases_forwarder
        ON account_aliases(local_domain, pattern) WHERE kind = 'forwarder';
    CREATE INDEX IF NOT EXISTS idx_account_aliases_actor_kind
        ON account_aliases(actor_id, kind);

    CREATE TABLE IF NOT EXISTS alias_hits (
        hit_id            BLOB    PRIMARY KEY,
        alias_id          BLOB    NOT NULL REFERENCES account_aliases(alias_id) ON DELETE CASCADE,
        matched_address   TEXT    NOT NULL,
        sender_domain     TEXT    NOT NULL DEFAULT '',
        received_at       INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_alias_hits_alias_time
        ON alias_hits(alias_id, received_at DESC);
";

// Mailing-list storage. Shape per `docs/goal/behavior/mail-mass-mailing.md`
// § Storage. A list is a sixth `account_aliases` kind (`kind = 'list'`,
// `fauna_mail::aliases::ALIAS_KIND_LIST`); the `mail_lists` row carries the
// list metadata + the friendly-name / List-Help / List-Archive header sources
// + the per-day send/recipient counters; `mail_list_members` holds each
// subscription with its cached RFC 8058 one-click-unsubscribe token. Types
// follow the codebase mail conventions: BLOB for UUIDs (matching
// `account_aliases.alias_id`), INTEGER Unix-seconds for timestamps (matching
// `account_aliases.created_at`). The CASCADE chain `account_aliases.alias_id
// → mail_lists.list_id → mail_list_members.list_id` (§ The list as an alias
// row) deletes the list + all members when the alias row is deleted.
pub(super) const MIGRATIONS_MAIL_LISTS: &str = "
    CREATE TABLE IF NOT EXISTS mail_lists (
        list_id            BLOB    PRIMARY KEY,
        alias_id           BLOB    NOT NULL REFERENCES account_aliases(alias_id) ON DELETE CASCADE,
        owner_actor_id     BLOB    NOT NULL,
        list_friendly_name TEXT,
        description        TEXT,
        list_help_url      TEXT,
        list_archive_url   TEXT,
        recipients_per_send INTEGER,
        created_at         INTEGER NOT NULL,
        last_send_at       INTEGER,
        member_count       INTEGER NOT NULL DEFAULT 0,
        sends_today        INTEGER NOT NULL DEFAULT 0,
        recipients_today   INTEGER NOT NULL DEFAULT 0,
        -- Epoch-day bucket (`now_ms / 86_400_000`) the `sends_today` /
        -- `recipients_today` meters were last reset under. The list-send
        -- quota check (`try_consume_list_quota`) lazily zeroes the meters on
        -- the first send of a new UTC day (same lazy-rollover model as
        -- `mail_outbound_warmup_state.counter_epoch_day`) -- so no cron task
        -- is needed. Default 0 ⇒ the next send resets the meters.
        counters_day       INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_mail_lists_alias
        ON mail_lists(alias_id);
    CREATE INDEX IF NOT EXISTS idx_mail_lists_owner
        ON mail_lists(owner_actor_id);

    -- Per-account-per-day list-recipient counter (§ Per-list rate accounting
    -- → § The per-day per-account cap). Keyed on the epoch-day bucket, so a
    -- new UTC day is a fresh key with an implicit 0 count — the counter
    -- self-resets without a sweep. Old rows are harmless (a tiny retention
    -- sweep can prune them later); nothing reads a past day's row.
    CREATE TABLE IF NOT EXISTS mail_list_account_daily_counter (
        actor_id        BLOB    NOT NULL,
        day             INTEGER NOT NULL,
        recipients_sent INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (actor_id, day)
    );

    -- Deployment-wide per-day list-recipient safety valve (§ The per-day
    -- per-deployment cap). Single row per epoch-day; self-resets the same way.
    CREATE TABLE IF NOT EXISTS mail_list_deployment_daily_counter (
        day             INTEGER PRIMARY KEY,
        recipients_sent INTEGER NOT NULL DEFAULT 0
    );

    -- Per-list send history (§ Composing → `fauna.bridges.list_list_send_history`).
    -- One row per `send_list_message` fan-out. `delivered_count` is the count
    -- the nest successfully queued (per-recipient MX-delivery audit is a later
    -- track; today queued == delivered for this surface), `recipient_count`
    -- the subscribed-member snapshot at send time.
    CREATE TABLE IF NOT EXISTS mail_list_sends (
        send_id          BLOB    PRIMARY KEY,
        list_id          BLOB    NOT NULL REFERENCES mail_lists(list_id) ON DELETE CASCADE,
        owner_actor_id   BLOB    NOT NULL,
        sent_at          INTEGER NOT NULL,
        recipient_count  INTEGER NOT NULL,
        delivered_count  INTEGER NOT NULL,
        unsubscribed_during_send INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_mail_list_sends_list
        ON mail_list_sends(list_id, sent_at);

    CREATE TABLE IF NOT EXISTS mail_list_members (
        member_id         BLOB    PRIMARY KEY,
        list_id           BLOB    NOT NULL REFERENCES mail_lists(list_id) ON DELETE CASCADE,
        recipient_address TEXT    NOT NULL,
        subscribed_at     INTEGER NOT NULL,
        unsubscribed_at   INTEGER,
        one_click_unsubscribe_token TEXT,
        UNIQUE (list_id, recipient_address)
    );
    -- Subscribed-count + per-list member enumeration (filtered on the
    -- nullable `unsubscribed_at`).
    CREATE INDEX IF NOT EXISTS idx_mail_list_members_list_unsub
        ON mail_list_members(list_id, unsubscribed_at);
    -- The RFC 8058 unsubscribe-handler lookup (`one_click_unsubscribe_token
    -- = <token>`). Non-unique: the deterministic HMAC token is unique per
    -- (list, address) in practice, but a UNIQUE here would turn a 192-bit
    -- collision into an insert failure for no benefit.
    CREATE INDEX IF NOT EXISTS idx_mail_list_members_token
        ON mail_list_members(one_click_unsubscribe_token);
";

// The primary-domain-rename state machine (`docs/goal/behavior/
// mail-primary-domain-rename.md` § Data — the `mail_domain_renames` model). One
// row per in-flight or historical primary rename; the row's `state` walks the
// lifecycle (`requested` → … → `completed`/`aborted`). Ids are 16-byte BLOB
// UUIDs (matching `mail_domains.domain_id`) and `initiated_by_actor_id` is a
// 32-byte actor BLOB (matching `admin_actors`); all timestamps are INTEGER
// epoch-millis (`now_epoch_millis`), NOT the spec-prose "UUID/TIMESTAMP" — the
// storage mirrors the existing codebase conventions.
//
// `idx_mail_domain_renames_active` is a UNIQUE index on the **constant `(1)`**
// restricted to non-terminal rows — i.e. at most one row may exist with
// `state NOT IN ('completed','aborted')`, enforcing the single-active-rename
// invariant (`mail-primary-domain-rename.md` § Concurrency) at the storage
// layer. It is keyed off `state` (the source of truth) rather than a
// denormalized `active_lock` column, so it cannot drift as later slices add
// state transitions. (The spec's literal `UNIQUE(state)` does NOT enforce this —
// two rows in *different* non-terminal states would both be admitted.)
//
// Slice-1 scope: no FKs to `mail_domains` (that table declares none elsewhere in
// the schema, and the spec's `ON DELETE SET NULL` audit back-link + the
// `domain_in_rename_flight` remove-guard are a later cross-table slice). A
// `requested` row referencing a since-removed domain is fully recoverable via
// `abort`. See `mail-primary-domain-rename.md` § Implementation status today.
pub(super) const MIGRATIONS_MAIL_DOMAIN_RENAMES: &str = "
    CREATE TABLE IF NOT EXISTS mail_domain_renames (
        rename_id               BLOB PRIMARY KEY,
        old_primary_domain_id   BLOB NOT NULL,
        new_primary_domain_id   BLOB NOT NULL,
        state                   TEXT NOT NULL,
        started_at              INTEGER NOT NULL,
        grace_days              INTEGER NOT NULL,
        cert_acquired_at        INTEGER,
        new_cert_fingerprint    TEXT,
        flipped_at              INTEGER,
        grace_started_at        INTEGER,
        grace_ends_at           INTEGER,
        ready_to_complete_at    INTEGER,
        completed_at            INTEGER,
        aborted_at              INTEGER,
        abort_reason            TEXT,
        initiated_by_actor_id   BLOB NOT NULL,
        CHECK (old_primary_domain_id != new_primary_domain_id),
        CHECK (completed_at IS NULL OR aborted_at IS NULL)
    );
    -- Single-active-rename invariant: at most one non-terminal row.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_mail_domain_renames_active
        ON mail_domain_renames((1)) WHERE state NOT IN ('completed', 'aborted');
    -- The grace watcher's wakeup query (later slice).
    CREATE INDEX IF NOT EXISTS idx_mail_domain_renames_grace_ends
        ON mail_domain_renames(grace_ends_at) WHERE state = 'grace';
    -- Admin audit enumeration.
    CREATE INDEX IF NOT EXISTS idx_mail_domain_renames_actor_started
        ON mail_domain_renames(initiated_by_actor_id, started_at DESC);
";

// Single-row admin overrides for the bridge `fetch_config` policy
// sub-structs — the A3 Bucket-B write path. One table per
// `FetchConfigReply` sub-struct (spam / auth / submission / imap /
// outbound), each holding the serialized `<X>PolicyOverrides` JSON blob
// in `overrides_json` on the single legal row (`id = 1`). The read path
// treats a missing row as "use the wire-type catalog default"
// (`SpamPolicyThresholds::default()` etc. in
// `libs/fauna-protocol/src/bridge_routing.rs`); within the blob a missing
// field decodes as `None` (also "catalog default"). A JSON blob (not
// discrete columns) lets a later track grow a sub-struct's overrides
// without a migration. `mail.enabled` is intentionally NOT
// a policy override (`mail-policy-config.md` § Impl status — the s6
// flag-file + supervisor handshake is a separate lifecycle track).
// Spec: `docs/goal/behavior/mail-policy-config.md` § Policy catalog
// (rollout tracked internally).
pub(super) const MIGRATIONS_MAIL_POLICY: &str = "
    CREATE TABLE IF NOT EXISTS mail_spam_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS mail_auth_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS mail_submission_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS mail_imap_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS mail_outbound_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    -- Nest-side alias-policy knobs (NOT projected to the bridge — read by
    -- the alias resolver + alias CRUD): exact_aliases_max,
    -- reserved_local_parts, subaddressing_enabled, wildcard_prefix_enabled.
    CREATE TABLE IF NOT EXISTS mail_alias_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    -- Nest-side mass-mailing knobs (the `mail.outbound.list_*` Tier-2 ceilings,
    -- `mail-mass-mailing.md` § Per-list rate accounting). Read by the list-send
    -- quota check + the list/member CRUD validators; `MassMailingPolicy` is
    -- also projected to the bridge in `fetch_config`, but the bridge does not
    -- enforce list caps (the nest owns the `send_list_message` fan-out), so
    -- these overrides are consumed nest-side. The admin write RPC + UI land
    -- with the flat `admin-mail` page; until then the effective policy is the
    -- catalog default (`MassMailingPolicy::default()`).
    CREATE TABLE IF NOT EXISTS mail_mass_mailing_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
    -- Nest-side mailbox-export ceilings (`mail-export.md` § Quota composition:
    -- `mail.export.max_blob_bytes`, `mail.export.max_concurrent_per_user`).
    -- Read nest-side by the export session handlers; never projected to a
    -- bridge, because no bridge is party to an export (§ Cross-actor
    -- isolation). These are admin CHOICES, so they rest in nest state rather
    -- than in a config file, env var or CLI flag — the one configuration
    -- surface is the apps. The admin write RPC + UI land with the flat
    -- `admin-mail` page (`mail-policy-config.md` marks both knobs pending
    -- ui.yaml ratification); until then the effective policy is the catalog
    -- default, which is the same shape `mail_mass_mailing_policy` above has.
    CREATE TABLE IF NOT EXISTS mail_export_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
";

// Nest-OWN transport/abuse policy — single-row admin overrides for nest's
// client-facing TLS listener caps (today: the per-source-IP concurrent-
// connection cap the `serve_tls` accept loop enforces). Distinct from the
// mail-scoped `mail_*_policy` tables above: this is read nest-side by
// `serve_tls`, never projected to the bridge. The write path is the
// `fauna.transport.put_policy` Admin RPC (`db::transport_policy`). A missing
// row / missing field ⇒ the catalog default (`fauna_conn_limit::
// DEFAULT_MAX_CONNS_PER_IP`); a JSON blob lets a later track grow the policy
// (global cap / handshake + header-read timeouts) without a migration. The
// cap is client-set config per the product invariant. Spec:
// `docs/goal/architecture/transport-connection.md` § Abuse posture item (2).
pub(super) const MIGRATIONS_TRANSPORT_POLICY: &str = "
    CREATE TABLE IF NOT EXISTS transport_policy (
        id             INTEGER PRIMARY KEY CHECK (id = 1),
        overrides_json TEXT NOT NULL
    );
";

// Deployment-wide mail-enable toggle (Phase E). A settable-both-ways
// single-row singleton — distinct from the policy tables above (`mail.enabled` is a *lifecycle* state, not a
// policy override — see the MAIL_POLICY comment + `mail-policy-config.md`
// § Impl status). The admin's `fauna.bridges.set_mail_enabled(bool)` upserts
// this row both ways; `fetch_config` reads it for the bridge's `mail_enabled`
// (an unset row reads OFF — Stage-5 default-off, owned by
// `CacheDb::effective_mail_enabled`); the 60 s
// reconciliation tick treats this row as authoritative over the
// `/data/imap-enabled` flag file.
// Spec: `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
// claim + § Implementation status today (Phase E).
pub(super) const MIGRATIONS_MAIL_ENABLED: &str = "
    CREATE TABLE IF NOT EXISTS mail_enabled (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide CalDAV-enable toggle — the calendar twin of
// `MIGRATIONS_MAIL_ENABLED`. A settable-both-ways single-row singleton.
// Email (SMTP/IMAP) and CalDAV gate independently even though one MDA bridge
// serves both: CalDAV needs only the HTTPS surface, email needs the full MX
// stack. The admin's `fauna.bridges.set_caldav_enabled(bool)` upserts this row;
// `fetch_config` reads it for the bridge's `caldav_enabled`, falling back to
// `mail_enabled` when unset (the out-of-the-box default: enabling email also enables CalDAV).
// Spec: `docs/goal/behavior/caldav-server.md` § Independent enablement.
pub(super) const MIGRATIONS_CALDAV_ENABLED: &str = "
    CREATE TABLE IF NOT EXISTS caldav_enabled (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide CardDAV-enable toggle — the contacts twin of
// `MIGRATIONS_CALDAV_ENABLED`. A settable-both-ways single-row singleton.
// Mail (SMTP/IMAP), CalDAV, and CardDAV gate independently even though one MDA
// bridge serves all three: CardDAV needs only the HTTPS surface (and rides the
// SAME DAV listener as CalDAV — no separate port). The admin's
// `fauna.bridges.set_carddav_enabled(bool)` upserts this row; `fetch_config`
// reads it for the bridge's `carddav_enabled`, falling back to `mail_enabled`
// when unset (fresh real-domain deployment gets a contacts surface out of the
// box). Part of the CardDAV server design (tracked internally).
pub(super) const MIGRATIONS_CARDDAV_ENABLED: &str = "
    CREATE TABLE IF NOT EXISTS carddav_enabled (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide WebDAV-enable toggle — the files twin of
// `MIGRATIONS_CARDDAV_ENABLED`. A settable-both-ways single-row singleton.
// Mail (SMTP/IMAP), CalDAV, CardDAV, and WebDAV gate independently even though
// one MDA bridge serves all four: WebDAV needs only the HTTPS surface (and rides
// the SAME DAV listener as CalDAV/CardDAV — no separate port). The admin's
// `fauna.bridges.set_webdav_enabled(bool)` upserts this row; `fetch_config`
// reads it for the bridge's `webdav_enabled`, falling back to `mail_enabled`
// when unset (fresh real-domain deployment gets a files surface out of the box —
// harmless-on, since nothing is served until a set is flagged
// `folders.webdav_enabled`). Spec: `docs/goal/behavior/webdav-server.md`
// § Independent enablement.
pub(super) const MIGRATIONS_WEBDAV_ENABLED: &str = "
    CREATE TABLE IF NOT EXISTS webdav_enabled (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide CalDAV listener port — the admin's chosen port for the MDA's
// CalDAV listener (a settable-both-ways single-row singleton). An admin *choice*
// (a product invariant — the client UI is the one user-config surface),
// set via `fauna.bridges.set_caldav_port`; `fetch_config` reads it for the
// bridge's `caldav_port`, falling back to the hard-coded
// `bridge_routing::DEFAULT_CALDAV_PORT` (8443) when unset. The `port > 0`
// CHECK rejects port 0 (not a bindable port). Per
// `docs/goal/behavior/caldav-server.md` § Network exposure.
pub(super) const MIGRATIONS_CALDAV_PORT: &str = "
    CREATE TABLE IF NOT EXISTS caldav_port (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        port     INTEGER NOT NULL CHECK (port > 0 AND port < 65536),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide client-facing API serving port — the admin's chosen port for
// the nest's OWN client-facing HTTPS listener (the WS-RPC transport + the served
// SPA), the symmetric twin of `caldav_port`. A settable-both-ways single-row
// singleton; an admin *choice* (a product invariant — the client UI is the
// one user-config surface), set via `fauna.admin.set_serving_port`.
// Boot-resolved into the nest's listener on a router-less / direct-listener
// deployment (preferring this over the `--bind`/`listen` seed's port), falling
// back to the hard-coded `node_policy::DEFAULT_SERVING_PORT` (443) when unset.
// Inert behind the `:443` SNI router on a domain box (the router + compose
// port-map own the external port = artifact-wiring). Applies on the next nest
// (re)start — the nest cannot hot-rebind its own listener. The `port > 0` CHECK
// rejects port 0 (not a bindable port). Per
// `docs/goal/architecture/nest/common.md` § Serving ports.
pub(super) const MIGRATIONS_SERVING_PORT: &str = "
    CREATE TABLE IF NOT EXISTS serving_port (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        port     INTEGER NOT NULL CHECK (port > 0 AND port < 65536),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide "auto-enable mail for new users" policy — a settable-both-ways
// single-row singleton, sibling of `MIGRATIONS_MAIL_ENABLED`. When ON (the
// effective default — unset ⇒ ON), a freshly-registered user whose client first
// connects to a mail-enabled deployment auto-provisions its own mailbox (the
// client runs `MailSettingsMachine::enable_mail`; the nest cannot mint it — the
// MSEK is client-held, `mail-credentials.md` § MSEK lifecycle). This row is the
// admin's deployment-wide override of that default-on behavior (NOT a per-user
// control — mail is user-controlled per `admin.md` § Don't do these). The
// admin's `fauna.bridges.set_auto_enable_mail_for_new_users(bool)` upserts it;
// `setup_status` reads it (unset ⇒ ON) and surfaces it to clients, which gate
// the auto-mint on it together with `email_enabled`.
// Spec: `docs/goal/behavior/mail-policy-config.md` § Tier-2 (new-user mail
// defaults) + `docs/goal/behavior/mail-credentials.md` § Auto-enable for new
// users.
pub(super) const MIGRATIONS_MAIL_AUTO_ENABLE_NEW_USERS: &str = "
    CREATE TABLE IF NOT EXISTS mail_auto_enable_new_users (
        id       INTEGER PRIMARY KEY CHECK (id = 1),
        enabled  INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at   INTEGER NOT NULL
    );
";

// Deployment-wide apex web-content actor designation — the web analogue of the
// per-domain catch-all *mail* actor (`mail_domains.catch_all_actor_id`), but a
// nest-wide singleton (the apex is the single node domain, not a per-domain row).
// Presence of the row ⇒ an actor is designated; its absence ⇒ no apex actor (the
// apex serves the built-in nest info page). The admin's
// `fauna.web.set_apex_actor(Some/None)` upserts/deletes it; `start_server` seeds
// the live `HostResolver`'s node-domain → apex-actor mapping from it at boot.
// Spec: `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting.
pub(super) const MIGRATIONS_WEB_APEX_ACTOR: &str = "
    CREATE TABLE IF NOT EXISTS web_apex_actor (
        id        INTEGER PRIMARY KEY CHECK (id = 1),
        actor_id  BLOB    NOT NULL,
        set_at    INTEGER NOT NULL
    );
";

// Per-actor opt-in for subdomain web hosting (`<handle>.<node-domain>`). Unlike
// the nest-wide `web_apex_actor` singleton above, this is **per-actor + user-set**
// and uses presence-as-flag semantics (the row's existence ⇒ opted in; its
// absence ⇒ default OFF — privacy / user-controls-their-data). The user's own
// `fauna.web.set_subdomain_enabled(true/false)` upserts/deletes their row;
// `start_server` seeds the live `HostResolver`'s `<handle>` → actor map and the
// per-subdomain cert lifecycle reconciles its certs from `list_subdomain_enabled`.
// Spec: `docs/goal/behavior/web-content-hosting.md` § Routing (subdomain) +
// Architectural rule 8.
pub(super) const MIGRATIONS_WEB_SUBDOMAIN_ENABLED: &str = "
    CREATE TABLE IF NOT EXISTS web_subdomain_enabled (
        actor_id  BLOB PRIMARY KEY,
        set_at    INTEGER NOT NULL
    );
";

// Per-actor IMAP/CalDAV-serving opt-out (Slice 2 of the home-with-public-relay
// deployment). Distinct axis from the deployment-wide `mail_enabled` /
// `caldav_enabled` singletons above: those are admin-set + whole-nest (they gate
// whether the MDA binds its listeners at all); THIS is **user-set + per-actor**,
// and the MDA's nest-side serving gate (`require_local_mail_serving` for IMAP, the
// `BridgeMda`-path check in the CalDAV handlers) consults it per request. A row's
// absence ⇒ serving ON (the common case; only the deployment user who reads on a
// paired private nest flips their own row OFF). One row per actor, settable both
// ways via `fauna.bridges.set_mail_serving_enabled` (User-class, caller-scoped).
// Spec: `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
// § MUA reach.
pub(super) const MIGRATIONS_ACTOR_MAIL_SERVING: &str = "
    CREATE TABLE IF NOT EXISTS actor_mail_serving (
        actor_id  BLOB PRIMARY KEY,
        enabled   INTEGER NOT NULL CHECK (enabled IN (0, 1)),
        set_at    INTEGER NOT NULL
    );
";

// Mailbox-migration import sessions (`mailbox-migration.md` § Progress lives
// nest-side): one row per client-driven import from a foreign IMAP source; the
// row is the source of truth the progress UI renders from and the resume
// protocol reads. `cursors` is a JSON object {mailbox: [last_uid, uidvalidity]}
// (the per-mailbox resume cursor pair). Rows are ephemeral bookkeeping with a
// 30-day `expires_at` GC (the imported *messages* live in
// `bridge_imap_messages`; dropping a session row loses only progress display),
// lazily swept on the start/list entry points. The partial UNIQUE index is the
// multi-device per-source lock (§ Architectural rules): at most one
// running/paused session per (actor, source).
pub(super) const MIGRATIONS_IMPORT_SESSIONS: &str = "
    CREATE TABLE IF NOT EXISTS import_sessions (
        session_id         TEXT PRIMARY KEY,
        actor_id           BLOB NOT NULL,
        source_descriptor  TEXT NOT NULL,
        state              TEXT NOT NULL CHECK (state IN
            ('running','paused','errored','completed','cancelled')),
        started_at         INTEGER NOT NULL,
        last_progress_at   INTEGER NOT NULL,
        total_count        INTEGER NOT NULL DEFAULT 0,
        imported_count     INTEGER NOT NULL DEFAULT 0,
        skipped_count      INTEGER NOT NULL DEFAULT 0,
        errored_count      INTEGER NOT NULL DEFAULT 0,
        cursors            TEXT NOT NULL DEFAULT '{}',
        error_reason       TEXT NOT NULL DEFAULT '',
        expires_at         INTEGER NOT NULL,
        -- HASH COMPANION of `source_descriptor` via
        -- `fauna_core::path_crypto::import_source_hash` -- carries the partial
        -- UNIQUE multi-device per-source lock below.
        source_hash        BLOB,
        -- SEALED LABEL over `source_descriptor` -- convergent under
        -- `source_hash` because the descriptor determines its own hash.
        source_sealed      BLOB,
        -- The wizard's scope-step mailbox selection (a JSON array) so a
        -- resumed session knows which mailboxes to re-EXAMINE
        -- (mailbox-migration.md § Resume protocol).
        scope TEXT NOT NULL DEFAULT '[]',
        -- The scope step's since date. The empty string means unbounded
        -- (mailbox-migration.md § Wizard steps step 3).
        date_from TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX IF NOT EXISTS idx_import_sessions_actor
        ON import_sessions(actor_id, state);
    -- The multi-device per-source lock: at most one running or paused session
    -- per (actor, source).
    CREATE UNIQUE INDEX IF NOT EXISTS idx_import_sessions_source_hash_lock
        ON import_sessions(actor_id, source_hash)
        WHERE state IN ('running','paused');
";

pub(super) const MIGRATIONS_EXPORT_SESSIONS: &str = "
    CREATE TABLE IF NOT EXISTS export_sessions (
        session_id                 TEXT PRIMARY KEY,
        actor_id                   BLOB NOT NULL,
        format                     TEXT NOT NULL,
        scope_descriptor           BLOB NOT NULL DEFAULT x'',
        state                      TEXT NOT NULL CHECK (state IN
            ('running','paused','errored','completed','cancelled')),
        started_at                 INTEGER NOT NULL,
        last_progress_at           INTEGER NOT NULL,
        total_count                INTEGER NOT NULL DEFAULT 0,
        exported_count             INTEGER NOT NULL DEFAULT 0,
        skipped_count              INTEGER NOT NULL DEFAULT 0,
        errored_count              INTEGER NOT NULL DEFAULT 0,
        -- Resume cursor (§ Session row model's `last_processed_message_id`).
        last_processed_message_id  TEXT NOT NULL DEFAULT '',
        error_reason               TEXT NOT NULL DEFAULT '',
        -- Nest-relative, `exports/<session-id>.zip.zst.sealed` (§ Blob shape on
        -- disk pins the suffix; what rests here is framed ciphertext, never a
        -- mountable `.zip.zst`). Stored verbatim — § Reclaim turns exactly this
        -- string into an unlink. An internal handle: never surfaced to the user.
        blob_path                  TEXT NOT NULL DEFAULT '',
        blob_bytes                 INTEGER NOT NULL DEFAULT 0,
        -- CLIENT-minted, client-wrapped. Stored verbatim, never opened here.
        blob_decryption_key_wrapped_for_actor BLOB,
        expires_at                 INTEGER NOT NULL,
        next_chunk_idx INTEGER NOT NULL DEFAULT 0,
        stream_generation INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_export_sessions_actor
        ON export_sessions(actor_id, state);
    CREATE INDEX IF NOT EXISTS idx_export_sessions_expiry
        ON export_sessions(expires_at);
";

// Per-actor message-dedup index (`mailbox-migration.md` § Dedup key
// persistence): one row per (actor, dedup key) — normalized Message-ID or the
// canonical-envelope SHA-256 fallback — pointing at the owning content row
// (`message_uri` = hex message id). `envelope_key` is the canonical-envelope
// SHA-256 that confirms a hit (§ The envelope key confirms a Message-ID hit):
// an `import_message` hit skips only when it agrees; every producer stamps
// it. Plaintext (the normalized Message-ID itself and a
// SHA-256, both on the plaintext floor by analogy with the message-id indexes
// — `encryption-at-rest.md` § Plaintext floor). Populated by every
// `import_message`, `append` and inbound MTA delivery; the producers compute
// both keys where plaintext exists. NOT re-derivable nest-side (the nest can't
// read sealed bodies) — treat as irrecoverable; never drop.
pub(super) const MIGRATIONS_ACTOR_MESSAGE_DEDUP: &str = "
    CREATE TABLE IF NOT EXISTS actor_message_dedup (
        actor_id   BLOB NOT NULL,
        dedup_key  TEXT NOT NULL,
        message_uri TEXT NOT NULL,
        envelope_key TEXT NOT NULL,
        PRIMARY KEY (actor_id, dedup_key)
    );
";

pub(super) const MIGRATIONS_INVITE_REQUESTS: &str = "
    CREATE TABLE IF NOT EXISTS invite_requests (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_id       BLOB NOT NULL UNIQUE,
        handle         TEXT NOT NULL,
        message        TEXT NOT NULL DEFAULT '',
        status         TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        decided_at     INTEGER,
        decided_by     BLOB,
        denial_reason  TEXT,
        -- public-mode.md § Age at registration (absence-as-signal): the
        -- applicant's age-claim band + how it was established ('attested-ios'
        -- / 'attested-android' for a submit-time-verified claim, 'none' for
        -- declared-only). NULL = the submit carried no claim.
        age_band       TEXT,
        age_provenance TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_invite_requests_status
        ON invite_requests(status, created_at DESC);
";

/// Family safety v1 — the guardianship link + the per-ward reach-policy
/// document (`docs/goal/behavior/family-safety.md` § Wire & data shape).
/// `guardianships` is the relationship (composite PK keeps co-guardians
/// representable later; v1 admits exactly one guardian per supervised
/// account). `guardian_policies` is per supervised account, not per link,
/// and grows by additive columns; every default is the unsupervised-
/// equivalent value, so a fresh link changes nothing until the guardian
/// tightens it. Both are plaintext-floor routing state (relationship edges /
/// per-recipient routing policy) in both storage modes.
pub(super) const MIGRATIONS_FAMILY: &str = "
    CREATE TABLE IF NOT EXISTS guardianships (
        supervised_actor_id BLOB NOT NULL,
        guardian_actor_id   BLOB NOT NULL,
        created_at          INTEGER NOT NULL,
        -- family-safety.md § Screen time (the day-bucket rule): the
        -- ward's last-reported clamped UTC offset in minutes, updated by the
        -- report handlers so status can derive 'the ward's local today'
        -- without a read-side wire input. 0 = UTC;
        -- coarse ward metadata (the last_seen disclosure class). Lives on the
        -- link — so it dies with it at graduation.
        ward_utc_offset_minutes INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (supervised_actor_id, guardian_actor_id)
    );
    CREATE INDEX IF NOT EXISTS idx_guardianships_guardian
        ON guardianships(guardian_actor_id);

    CREATE TABLE IF NOT EXISTS guardian_policies (
        supervised_actor_id BLOB PRIMARY KEY,
        contact_approval    INTEGER NOT NULL DEFAULT 0,
        unknown_sender_mail TEXT    NOT NULL DEFAULT 'allow',
        federation_contact  INTEGER NOT NULL DEFAULT 1,
        feed_sources        TEXT    NOT NULL DEFAULT 'allow',
        updated_at          INTEGER NOT NULL,
        -- v1.x additive pillars (family-safety.md § Wire & data shape:96 --
        -- designed 2026-07-15). Every default is the unsupervised-equivalent
        -- value -- so a fresh link enforces nothing until the guardian
        -- tightens it.
        --
        -- Content pillar (§ Content policy): per-category render floor --
        -- closed enum inherit|collapse|block (default 'inherit' = the ward's
        -- own preferences decide). Client-enforced post-decrypt.
        content_nsfw        TEXT    NOT NULL DEFAULT 'inherit',
        content_spam        TEXT    NOT NULL DEFAULT 'inherit',
        content_phishing    TEXT    NOT NULL DEFAULT 'inherit',
        content_commercial  TEXT    NOT NULL DEFAULT 'inherit',
        -- Guardian Notify knob (§ Guardian Notify): default off. Declared here
        -- with the pillar batch; the notify report handler that writes it
        -- lands in Slice D -- so no reader touches this column yet.
        content_notify      INTEGER NOT NULL DEFAULT 0,
        -- Screen-time pillar (§ Screen time): usage window (minutes from
        -- local midnight -- wrap-capable) + daily budget. Nullable — NULL =
        -- that control is unset (the unsupervised-equivalent). Client-enforced.
        screen_window_start INTEGER,
        screen_window_end   INTEGER,
        screen_daily_minutes INTEGER,
        -- The bridge-DM gate's knob (§ The bridge-DM gate): 'allow' |
        -- 'hold' for inbound DMs from external peers the ward has never
        -- corresponded with. NOT NULL DEFAULT 'allow' — the
        -- unsupervised-equivalent -- so the default
        -- changes no ward's behaviour. Unlike the two pillars above this IS
        -- nest-enforced (a routing-floor knob -- gated at every inbound DM
        -- write path); it sits here rather than in a sub-document because it
        -- is a single closed-enum knob -- exactly like unknown_sender_mail.
        unknown_peer_dm     TEXT    NOT NULL DEFAULT 'allow',
        -- The guardian tier's controversial-class feature limits
        -- (`dynamic-features.md` § Wire & data shape). The DAG-CBOR map of
        -- stable-feature-key -> FeaturePolicy -- stored whole so an older
        -- nest round-trips a newer guardian document unchanged -- exactly as
        -- `feature_policies.document` does for the other tiers.
        --
        -- It lives HERE rather than in `feature_policies` because the
        -- guardian tier mints no kind of its own: it rides
        -- `fauna.family.policy.update` -- so its document arrives and departs
        -- with the rest of the ward's policy and must die with the
        -- guardianship -- which this table already guarantees. NULL = the
        -- guardian expressed no feature opinion -- which is the
        -- unsupervised-equivalent and is NOT the same as an authored allow.
        features_document   BLOB
    );
";

/// The transfer consent handshake's pending-proposal store
/// (`family-safety.md` § Graduation & transfer, ratified 2026-07-12). The
/// primary key IS the one-pending-per-ward invariant — a new proposal
/// replaces the row. Rows older than the 7-day window are expired lazily
/// (invisible to reads, pruned opportunistically — no background job).
/// Floor metadata: the same relationship class as `guardianships` itself.
pub(super) const MIGRATIONS_GUARDIAN_TRANSFERS: &str = "
    CREATE TABLE IF NOT EXISTS guardian_transfers (
        supervised_actor_id        BLOB PRIMARY KEY,
        proposed_guardian_actor_id BLOB NOT NULL,
        initiated_by               BLOB NOT NULL,
        created_at                 INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_guardian_transfers_proposed
        ON guardian_transfers(proposed_guardian_actor_id);
";

/// The account age band + its provenance (`family-safety.md` § The
/// account age band — D2/D5, ratified 2026-08-22). **Absence is the
/// representation of the by-construction default**: an account with no
/// guardianship link and no row is `18+`/`none` — the common case is
/// unrepresentable rather than stored — and a *supervised* account with no
/// row has no known band (`fauna.family.status` reports nothing). Rows are written only at admission (guardian-asserted
/// band on a supervised admission; verified attested claim on any admission),
/// a **minor band only ever beside a guardianship link** (write sites
/// enforce it), and the row is **deleted at graduation** — otherwise a
/// graduated ward would keep reading as a minor while the no-link rule says
/// `18+` by construction. Cascades with the account. Plaintext-floor
/// admission metadata (the same class as the guardianship edge); enforcement
/// never reads it — only admission defaults and audits do.
pub(super) const MIGRATIONS_ACCOUNT_AGE_BANDS: &str = "
    CREATE TABLE IF NOT EXISTS account_age_bands (
        actor_id    BLOB PRIMARY KEY,
        band        TEXT NOT NULL,
        provenance  TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
";

/// The ward's pending child-initiated contact asks
/// (`family-safety.md` § Child-initiated contact requests). The primary key IS
/// the one-ask-per-(ward, peer) invariant — a re-ask while pending is a quiet
/// no-op, which is also what makes the doorbell ring exactly once per created
/// ask. A pending ask cannot ride `contacts.status` (`store_knock` upserts
/// `'pending'` onto the same key, so an incoming knock would clobber it) or
/// `knocks` (a knock is the *peer's* held arrival, not the ward's intent);
/// `guardian_transfers` is the pending-intent precedent. Rows expire lazily
/// after 30 days, are capped per ward, cascade with the ward, and are dropped
/// at graduation — oversight intent, not user content.
pub(super) const MIGRATIONS_GUARDIAN_CONTACT_REQUESTS: &str = "
    CREATE TABLE IF NOT EXISTS guardian_contact_requests (
        -- AUTOINCREMENT (the knocks-table precedent) so a deleted ask's id is
        -- never reused: the doorbell dedup keys on it, and a plain rowid
        -- reused across a deny + re-ask would silently swallow the re-ring.
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        supervised_actor_id BLOB NOT NULL,
        peer_actor_id       BLOB NOT NULL,
        created_at          INTEGER NOT NULL,
        UNIQUE (supervised_actor_id, peer_actor_id)
    );
";

/// The ward's feed-source asks and the grants they become
/// (`family-safety.md` § Feed-source approvals). One table for both states —
/// `approved_at NULL` is a pending ask, set is a **single-use grant** — because
/// they are one lifecycle: the guardian's approve is an in-place transition,
/// and the redeeming gate's atomic delete is what spends the grant. A separate
/// grants table would make "approved but still pending" representable.
///
/// The key is `(ward, bridge, operation, target)`, exactly what the redeeming
/// gate passes: a grant unlocks the *one* object the guardian saw and no other
/// (`label` is display-only and deliberately outside the key). Rows cascade
/// with the ward and are dropped at graduation — oversight intent, not user
/// content; pending asks expire lazily after 30 days, grants 7 days after
/// approval.
pub(super) const MIGRATIONS_GUARDIAN_FEED_REQUESTS: &str = "
    CREATE TABLE IF NOT EXISTS guardian_feed_requests (
        -- AUTOINCREMENT for the same reason as guardian_contact_requests: the
        -- guardian's doorbell dedup keys on this id, and SQLite reuses a plain
        -- max rowid after a delete — a deny + re-ask would land on the reused
        -- id and the re-ring would be silently swallowed as a duplicate.
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        supervised_actor_id BLOB NOT NULL,
        bridge_id           TEXT NOT NULL,
        -- 'link' | 'follow' | 'feed' (fauna_core::data::FeedSourceOperation);
        -- the handler refuses anything else, so no unnameable op is stored.
        operation           TEXT NOT NULL,
        -- The follow id / feed URI; '' for a 'link', which approves connecting
        -- the bridge as a whole. Never NULL, so the UNIQUE key below always
        -- compares (SQLite treats NULLs as distinct — a nullable target would
        -- silently admit unlimited duplicate link asks).
        target              TEXT NOT NULL,
        -- Display-only, never authorizing: it rides the queue row's summary.
        label               TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        -- NULL = pending ask; set = the instant the guardian granted it.
        approved_at         INTEGER,
        UNIQUE (supervised_actor_id, bridge_id, operation, target)
    );
";

/// The ward's per-bridge known-DM-peer set (`family-safety.md` § The
/// bridge-DM gate) — the fact the gate keys on, and the *only* thing it stores.
///
/// **There is deliberately no hold state here.** A conversation is held iff the
/// knob says `hold` and the peer has no row at all: hold-ness is computed at
/// read time, so relaxing `unknown_peer_dm` releases every held conversation by
/// construction — no drainage rule, nothing strandable (§ Don't do these —
/// *"don't store bridge-DM hold state"*). A row means a *decision* was made:
/// `allow` (the ward DMed them, or the guardian approved) or `block` (the
/// guardian denied).
///
/// Peer ids are envelope-class routing metadata — the same plaintext-floor class
/// as the mail allowlist's addresses (`encryption-at-rest.md` § Plaintext floor
/// § Routing tables), never content. Rows cascade with the ward and are dropped
/// at graduation: they are oversight verdicts, not user content, so there is
/// nothing held to release.
pub(super) const MIGRATIONS_GUARDIAN_DM_PEERS: &str = "
    CREATE TABLE IF NOT EXISTS guardian_dm_peers (
        supervised_actor_id BLOB NOT NULL,
        -- The bridge the peer id is scoped to ('nostr' | 'bluesky' | ...). Part
        -- of the key: the same string means different people on two networks.
        bridge_id           TEXT NOT NULL,
        -- The external peer identity (a nostr pubkey hex, a bluesky DID, ...) —
        -- opaque to the gate, which only ever compares it.
        peer_id             TEXT NOT NULL,
        -- 'allow' | 'block' (fauna_core::data::DmPeerVerdict). A value this
        -- binary cannot name resolves to Held, never Deliver — see
        -- fauna_core::data::supervised_dm_verdict.
        verdict             TEXT NOT NULL,
        -- 'ward' (seeded by the ward's own outbound send) | 'guardian' (an
        -- explicit decide). Mirrors guardian_mail_allowlist.added_by; it is
        -- what lets the seed refuse to overwrite a guardian's block.
        added_by            TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        PRIMARY KEY (supervised_actor_id, bridge_id, peer_id)
    );
";

/// The guardian mail gate's two tables (`family-safety.md` § The mail gate,
/// § Wire & data shape). Both are plaintext-floor routing state in both
/// storage modes — envelope FROM is on the floor (`encryption-at-rest.md`
/// § Plaintext floor § Routing tables).
///
/// `guardian_mail_allowlist` is the ward's known-sender set: the addresses the
/// guardian approved plus the ones the ward has itself mailed (`added_by`
/// records which). `guardian_mail_holds` is the **envelope sidecar** for a
/// held message, NOT a hold store — the message's presence in the ward's held
/// mailbox is the hold; this row carries only the sender address the guardian's
/// queue renders and the approve path allowlists. `guardian_mail_sent_msgids`
/// is the ward's own **sent**-Message-ID set: the correlation a null-reverse-path
/// delivery-status report must match — and *consume* (`correlation_budget`) —
/// to be delivered rather than held (an address correlation was forgeable by
/// anyone who could guess one address the ward had mailed, and a durable id
/// correlation was an open channel to anyone the thread's `References:`
/// disclosed it to — § The mail gate). `guardian_mail_correlated_origins`
/// records the address-header set of each *delivered* correlated
/// report, so the outbound auto-seed declines to allowlist an address the
/// ward is merely replying to (the report's author chose it — § The mail
/// gate). All four cascade with the ward and are cleared at graduation
/// (holds are *released* to INBOX first, never dropped — `family-safety.md`
/// § Reach approvals).
///
/// The sent-Message-ID set is deliberately **not** the `outbound_mail_queue`,
/// which already stores an `original_msgid` per remote message: that table keys
/// on the sender *address*, carries forwarded and nest-generated rows the ward
/// never composed, and has its own (today: absent) retention policy. A security
/// correlation must not rest on another table's incidental lifetime, so this one
/// is per-ward, seeded at the same two outbound chokepoints as the allowlist,
/// and pruned on its own schedule.
pub(super) const MIGRATIONS_GUARDIAN_MAIL: &str = "
    CREATE TABLE IF NOT EXISTS guardian_mail_allowlist (
        supervised_actor_id BLOB NOT NULL,
        address             TEXT NOT NULL,
        added_by            TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        PRIMARY KEY (supervised_actor_id, address)
    );

    CREATE TABLE IF NOT EXISTS guardian_mail_holds (
        message_id          BLOB PRIMARY KEY,
        supervised_actor_id BLOB NOT NULL,
        sender_address      TEXT NOT NULL,
        created_at          INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_guardian_mail_holds_ward
        ON guardian_mail_holds(supervised_actor_id);

    CREATE TABLE IF NOT EXISTS guardian_mail_sent_msgids (
        supervised_actor_id BLOB NOT NULL,
        message_id          TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        -- Correlated deliveries remaining. Seeded at remote-recipient
        -- count + 2 and decremented per delivered null-path report; 0 = spent.
        -- The DEFAULT is the one-recipient budget.
        correlation_budget  INTEGER NOT NULL DEFAULT 3,
        PRIMARY KEY (supervised_actor_id, message_id)
    );
    CREATE INDEX IF NOT EXISTS idx_guardian_mail_sent_msgids_age
        ON guardian_mail_sent_msgids(supervised_actor_id, created_at);

    -- The address-header set (From/Sender/Reply-To/To/Cc) of each
    -- DELIVERED correlated null-path report, per ward. The outbound auto-seed
    -- declines to allowlist these addresses: a reply to a report whose author
    -- chose them must not bootstrap permanent access (§ The mail gate). Rows
    -- are budget-bounded at the source (each recording costs the report a
    -- correlation-budget unit) and time-bounded on the read path.
    CREATE TABLE IF NOT EXISTS guardian_mail_correlated_origins (
        supervised_actor_id BLOB NOT NULL,
        address             TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        PRIMARY KEY (supervised_actor_id, address)
    );
";

/// Guardian Notify (`family-safety.md` § Guardian Notify, v1.x): the ward's
/// conforming client reports coarse per-category enforcement counts, and the
/// nest accumulates them here — `category + count, never content, never a
/// content id`. Per (ward, day, category); the PK's `(supervised, day)`
/// prefix covers the guardian's per-day status read. Like every family side
/// table it is a no-op for an unsupervised account (the `WHERE EXISTS`
/// guardianship guard on the upsert), cascades with the ward, and is dropped at
/// graduation. `count` is a running per-day total, clamped on the way in.
///
/// `day` is the **ward's local day, nest-stamped**: `(now_epoch_secs +
/// clamped_offset·60) / 86_400`, where the offset is the report's
/// `utc_offset_minutes` clamped to ±14 h (`family-safety.md` § Screen time —
/// the day-bucket rule, ratified 2026-07-16, adopted here for one day concept
/// across both pillars). The nest still stamps the bucket from its own clock —
/// client input is limited to the clamped offset, so the key space stays
/// bounded (at most two adjacent buckets per real instant) and the doorbell
/// flood bound survives: ≤ one doorbell per (ward, day, category), ≤ 2 buckets
/// per real day even from a hostile offset-flipping client. An absent offset
/// (an older client) degrades to 0 = UTC.
pub(super) const MIGRATIONS_GUARDIAN_CONTENT_NOTICES: &str = "
    CREATE TABLE IF NOT EXISTS guardian_content_notices (
        supervised_actor_id BLOB NOT NULL,
        day                 INTEGER NOT NULL,
        category            TEXT NOT NULL,
        count               INTEGER NOT NULL,
        PRIMARY KEY (supervised_actor_id, day, category)
    );
";

/// Screen-time budget accounting (`family-safety.md` § Screen time, v1.x):
/// the ward's conforming client heartbeats coarse foreground minutes
/// (`fauna.family.usage_report`), and the nest accumulates the cross-device
/// per-day total here — the number the enforcing client locks on and both
/// roles' `status` readouts render. `day` is the ward's local day, nest-stamped
/// via the clamped report offset (the same bucket rule as
/// [`MIGRATIONS_GUARDIAN_CONTENT_NOTICES`], which documents it); `minutes` is
/// a running per-day total, delta-clamped on the way in. Like every family
/// side table it is a no-op for an unsupervised account (the `WHERE EXISTS`
/// guardianship guard on the upsert), cascades with the ward, and is dropped
/// at graduation. Coarse metadata only: minutes-per-day, the `last_seen`
/// disclosure class — never *what* was used.
pub(super) const MIGRATIONS_GUARDIAN_USAGE: &str = "
    CREATE TABLE IF NOT EXISTS guardian_usage (
        supervised_actor_id BLOB NOT NULL,
        day                 INTEGER NOT NULL,
        minutes             INTEGER NOT NULL,
        PRIMARY KEY (supervised_actor_id, day)
    );
";

/// The controversial-class feature gate's two tables (`dynamic-features.md`
/// § Wire & data shape: "a per-tier policy store … + the usage-counter table.
/// All additive; nothing user-irrecoverable (policies are re-settable; counters
/// are re-derivable bounds)").
///
/// **`feature_policies`** — what each rule-setter tier *authored*, one row per
/// (tier, subject, feature). The effective policy is never stored: it is the
/// meet, recomputed per evaluation from these rows plus tier 1's compiled-in
/// constants (`fauna_core::feature_gate::effective_policy`), so a constant
/// revision or a policy write takes effect without a rewrite pass over anything.
/// `tier` is the [`fauna_core::feature_gate::RuleTier`] discriminant; tiers 0
/// and 1 are never stored (tier 0 is the artifact, tier 1 is a Rust constant).
/// `subject_id` is the account a *self* policy binds and is **empty for the
/// nest-wide tiers** (region, admin) rather than NULL — a zero-length blob keeps
/// the primary key total, so "the admin's payments policy" has exactly one row
/// and an upsert cannot silently create a second. The guardian tier deliberately
/// has no rows here: its documents live in `guardian_policies` as an additive
/// sub-document (§ Wire & data shape — "the guardian tier mints no new kind").
/// `document` is the DAG-CBOR `FeaturePolicy`, stored whole so an older nest
/// round-trips a newer tier's document unchanged (additive-everywhere).
///
/// **`feature_usage`** — the coarse day buckets of § Usage accounting, one row
/// per (account, feature, dimension, local day). `amount` is a count for
/// `operations` / `counterparties` and a magnitude for `volume`. Buckets are
/// **append-only within a window**: nothing decrements them, which is what makes
/// a counterparty bound unrefundable (§ The quota grammar's third refinement —
/// *a counterparty, once counted, stays counted for the window's trailing span
/// regardless of any later removal*). Pruned past the largest window (30 days).
/// Counts only, never a per-item ledger, never an identity — boundary 2.
pub(super) const MIGRATIONS_FEATURE_GATE: &str = "
    CREATE TABLE IF NOT EXISTS feature_policies (
        tier       INTEGER NOT NULL,
        subject_id BLOB    NOT NULL,
        feature    TEXT    NOT NULL,
        document   BLOB    NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (tier, subject_id, feature)
    );
    CREATE TABLE IF NOT EXISTS feature_usage (
        actor_id  BLOB    NOT NULL,
        feature   TEXT    NOT NULL,
        dimension TEXT    NOT NULL,
        day       INTEGER NOT NULL,
        amount    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, feature, dimension, day)
    );
";

/// The region tier's nest state (`dynamic-features.md` § The region tier +
/// `region-blocking.md` § The region/authority plumbing — W2 slice 4).
///
/// **`nest_region`** — the deployment's *declared* region, the singleton on the
/// `caldav_enabled` shape. § Region determination is **declared, never
/// detected**: *"the admin declares the nest's region — its legal situs — in the
/// admin UI, persisted in nest state"*, and IP geolocation is never consulted.
/// Unset (no row) is the ratified fresh-install state and the reason a fresh
/// nest runs tiers 1/3/4/5 with gated features on at tier-1 constants. `id = 1`
/// keeps it a singleton the way every other deployment-wide toggle does.
///
/// **`region_sequence_floor`** — the anti-replay high-water mark, one row per
/// (region, payload kind, **authority**). Deliberately a table of its own
/// rather than a column on `region_artifacts`, because the two have different
/// lifetimes: the artifact row is the *binding* and is retired whenever the
/// binding stops (a change of situs, a withdrawal, a de-listed authority),
/// while the floor is the *replay defence* and must outlive every one of those
/// — a floor an admin action resets is not a floor.
///
/// **Keyed on the authority, not on the region code alone.** The sequence
/// counter belongs to the issuing authority, so comparing across authorities is
/// meaningless. Key *rotation* does not start a new space — the registry models
/// it as another `AuthorityKey` under the same `RegionEntry.authority_name`, so
/// the floor rightly persists across it. A curated registry revision that hands
/// the region to a *different* authority does start one, which is what keeps a
/// surviving floor from wedging the plane permanently shut against a legitimate
/// successor whose first document is at a low sequence — there is no operator
/// surface to clear a stuck floor, and by product invariant there must not be
/// one. `authority_name` is safe to key on because it comes from the curated
/// registry (compiled in today), never from the artifact an attacker delivers.
///
/// The upsert below seeds the floor from any artifact already in force. It
/// re-runs every boot and is idempotent — and it is this table's **only
/// repair** for a floor that lags a stored artifact's sequence
/// (`put_region_artifact` writes both rows in one transaction, but a lagging
/// floor can still reach disk via a crash window, or a hand-restored backup).
/// `INSERT OR IGNORE` would keep whatever the stored floor already says on a
/// key conflict — the repair declining to run in the only case it exists
/// for. `MAX` makes the seed raise a lagging floor
/// rather than leave it, on every boot, matching `put_region_artifact`'s own
/// `ON CONFLICT ... DO UPDATE SET sequence = MAX(...)` upsert. The `WHERE
/// true` is load-bearing, not decorative: SQLite's parser cannot otherwise
/// tell an `INSERT ... SELECT ... FROM t ON CONFLICT` upsert-clause apart from
/// a continuation of the `SELECT`'s own syntax and rejects it with "near DO:
/// syntax error" — any real predicate on the `SELECT` disambiguates it just as
/// well, but there is no natural one here (every artifact row seeds a floor).
///
/// **`region_artifacts`** — the last-known-good store, one row per (region,
/// payload kind). *This table is the fail posture*: § Fail posture says a
/// fetched policy *"stays in force until replaced"*, so a failed or refused
/// refresh writes nothing here and the previously accepted artifact keeps
/// binding. `sequence` is the envelope's monotonic counter and is what makes
/// that un-rollbackable — a replayed older artifact is refused against this row
/// rather than re-applied. `envelope` is the whole signed artifact, kept so the
/// acceptance stays re-checkable against a later registry revision and so the
/// transparency read can name the exact document in force. `authority_name` is
/// the name the registry gave at **acceptance** time, stored rather than looked
/// up again: the read must show whoever was verified when the document started
/// binding, and one that re-consulted the registry would go *blank* the moment
/// an authority was de-listed while its document was still in force — a bound
/// nobody can see, which is exactly boundary 4's silent gate. Retiring a
/// de-listed authority's document is the re-fold's job, and it removes the
/// bound and the name together.
///
/// **`region_refresh`** — the feature plane's refresh log, one row per payload
/// kind. `checked_at` is the last *attempt* (success or not) — diagnostic
/// only. `first_attempted_at` is the anchor for a kind that has never been
/// reached: set only on `INSERT`, never touched by the upsert's `DO UPDATE`,
/// so it stays the true first try. `reached_at` is the last time the channel
/// **answered** (an accepted or a refused artifact — a refusal is a reached
/// channel; a fetch failure is not) and is what the admin-side staleness
/// warning is computed from — deliberately separate from `accepted_at`, since a
/// nest that is fetching fine but seeing no new publications is current, not
/// stale, and separate from `checked_at`, since a worker that keeps trying and
/// failing must not read as fresh. Mirrors `region_relay_cache`'s
/// `attempted_at`/`reached_at` split below — the same rule, lifted onto the
/// feature plane (`region_tier.rs`'s `refresh_staleness`).
///
/// **`region_log_anchor`** — the last transparency-log head this nest accepted
/// inclusion evidence against (`region-blocking.md` § The transparency log,
/// *head monotonicity*): a single row, absent until the first witnessed head is
/// accepted. Absent **is** the pre-log era for a build whose compiled-in anchor
/// is `None`, which is why the row is created only by an acceptance and never
/// seeded. Kept separate from `region_artifacts` for the same reason the replay
/// floor is: retiring a document never retires the log history it was proven
/// in, and a new head must keep descending from this one.
///
/// **`region_relay_cache`** — the relay's per-(region, payload kind) cache
/// behind `fauna.region.artifact.get` (`region-blocking.md` § The content
/// plane → *How an app obtains its region's policy*): one row per pair an
/// **app has asked for**, and only those — the row *is* the demand, so a region
/// nobody declares is never fetched. `requested_at` is the first ask; the
/// envelope columns stay NULL until a verified artifact lands and are cleared
/// again if its authority leaves the registry. `attempted_at` is the last fetch
/// attempt and `reached_at` the last time the log **answered** (an accepted or a
/// refused artifact — a refusal is a reached channel); staleness reads the
/// second, so an unreachable log goes stale while the worker keeps trying.
/// Separate from `region_artifacts` because that store is single-region by
/// construction (the declared situs) while the relay serves whichever regions
/// its apps declare. **Derived and recreatable**: every row is a demand marker
/// or an envelope the next refresh refetches from the log, and the replay
/// defence is not here — it stays in `region_sequence_floor`, raised in the
/// same transaction as each acceptance, so dropping this table can never let an
/// older artifact back in.
pub(super) const MIGRATIONS_REGION_TIER: &str = "
    CREATE TABLE IF NOT EXISTS nest_region (
        id     INTEGER PRIMARY KEY CHECK (id = 1),
        region TEXT    NOT NULL,
        set_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS region_artifacts (
        region       TEXT    NOT NULL,
        payload_kind TEXT    NOT NULL,
        sequence     INTEGER NOT NULL,
        issued_at    INTEGER NOT NULL,
        key_id       TEXT    NOT NULL,
        authority_name TEXT  NOT NULL,
        envelope     BLOB    NOT NULL,
        accepted_at  INTEGER NOT NULL,
        PRIMARY KEY (region, payload_kind)
    );
    CREATE TABLE IF NOT EXISTS region_refresh (
        payload_kind       TEXT    NOT NULL PRIMARY KEY,
        checked_at         INTEGER NOT NULL,
        ok                 INTEGER NOT NULL,
        detail             TEXT,
        -- Nullable — see the table's doc comment above.
        first_attempted_at INTEGER,
        reached_at         INTEGER
    );
    CREATE TABLE IF NOT EXISTS region_sequence_floor (
        region         TEXT    NOT NULL,
        payload_kind   TEXT    NOT NULL,
        authority_name TEXT    NOT NULL,
        sequence       INTEGER NOT NULL,
        updated_at     INTEGER NOT NULL,
        PRIMARY KEY (region, payload_kind, authority_name)
    );
    CREATE TABLE IF NOT EXISTS region_log_anchor (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        head        BLOB    NOT NULL,
        accepted_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS region_relay_cache (
        region          TEXT    NOT NULL,
        payload_kind    TEXT    NOT NULL,
        requested_at    INTEGER NOT NULL,
        envelope        BLOB,
        evidence        BLOB,
        sequence        INTEGER,
        authority_name  TEXT,
        accepted_at     INTEGER,
        attempted_at    INTEGER,
        reached_at      INTEGER,
        last_error      TEXT,
        PRIMARY KEY (region, payload_kind)
    );
    INSERT INTO region_sequence_floor
        (region, payload_kind, authority_name, sequence, updated_at)
    SELECT region, payload_kind, authority_name, sequence, accepted_at
    FROM region_artifacts WHERE true
    ON CONFLICT (region, payload_kind, authority_name) DO UPDATE SET
        sequence = MAX(sequence, excluded.sequence),
        updated_at = excluded.updated_at;
";

/// The domain-expiry watch's record — what the nest last learned about its
/// **primary** domain's registration (`domains-and-tls-bootstrap.md` § Domain
/// loss → *Detection*).
///
/// Deliberately **single-row** (`CHECK (id = 1)`), matching `nest_region`: the
/// watch is deployment-scoped, not per-domain and not per-actor — the ratified
/// signal is about the one name the whole deployment's identity, mail and
/// recovery locator hang off. A secondary domain lapsing is a different (and
/// much smaller) story that this section does not tell.
///
/// `fetched_at` records the last **attempt**, whatever its outcome, which is
/// what a staleness read wants; `outcome` + `detail` carry the three-outcome
/// contract (checked / skipped-with-reason / failed-with-error) so a skip is
/// never silent in the record even though it is silent on the banner.
/// `statuses` is the RDAP status list stored **verbatim as served** — the
/// normalization for comparison happens in
/// `fauna_protocol::domain_expiry::evaluate`, so a support read shows what the
/// registry actually said rather than what we folded it to.
pub(super) const MIGRATIONS_DOMAIN_EXPIRY: &str = "
    CREATE TABLE IF NOT EXISTS domain_expiry (
        id         INTEGER PRIMARY KEY CHECK (id = 1),
        domain     TEXT    NOT NULL,
        expires_at INTEGER,
        statuses   TEXT    NOT NULL,
        fetched_at INTEGER NOT NULL,
        outcome    TEXT    NOT NULL,
        detail     TEXT
    );
";

/// R14 build step 4 — the nest as v1 escrow holder (`account-data-plane.md`
/// § The generation machinery → *The escrow doors*). One row per deposited
/// wrap; the composite key IS the idempotency contract ("idempotent per
/// (generation id, wrap hash)"), and `created_at` is the stamp the
/// holder-signed receipt carries — kept on the row so a re-deposit returns a
/// byte-identical receipt. Ciphertext opaque to the nest by construction.
pub(super) const MIGRATIONS_GENERATION_ESCROW: &str = "
    CREATE TABLE IF NOT EXISTS generation_escrow_wraps (
        actor_id      BLOB    NOT NULL,
        generation_id BLOB    NOT NULL,
        wrap_hash     BLOB    NOT NULL,
        wrap          BLOB    NOT NULL,
        created_at    INTEGER NOT NULL,
        PRIMARY KEY (actor_id, generation_id, wrap_hash)
    );
";

/// The media proxy's playback-ticket HMAC key, sealed under (key-material-
/// hierarchy.md § Audience: deployment infrastructure -> *Media playback-ticket
/// secret*). One row, `id = 0`, the `oauth_session_secret` shape: a service key
/// presented back to this nest and nothing else, so no retired generation is
/// ever kept.
pub(super) const MIGRATIONS_MEDIA_TICKET_SECRET: &str = "
    -- `secret_wrapped` is the raw 32-byte HMAC-SHA-256 key sealed under
    -- nest_kek::MEDIA_TICKET_CONTEXT, registered in nest_kek::SATELLITES so a
    -- deployment-seed rotation re-keys it rather than stranding it. Losing
    -- this row invalidates outstanding playback tickets -- a viewer re-taps --
    -- and nothing a user authored.
    CREATE TABLE IF NOT EXISTS media_ticket_secret (
        id             INTEGER PRIMARY KEY CHECK(id = 0),
        secret_wrapped BLOB NOT NULL,
        created_at     INTEGER NOT NULL
    );
";

/// The nest-held **DKIM signing keys** (`key-material-hierarchy.md` § Audience:
/// deployment infrastructure → *The oracle* → *The DKIM class is the outbound
/// spool's own door*), one per (mail domain, selector). Written and opened
/// only by `mail_dkim_key.rs`.
pub(super) const MIGRATIONS_MAIL_DKIM_KEYS: &str = "
    -- `key_wrapped` is the DKIM private key (PKCS#8 DER for Ed25519, PKCS#8
    -- PEM for RSA; `alg` says which) sealed under the nest-internal
    -- key-encryption key with nest_kek::MAIL_DKIM_CONTEXT, registered in
    -- nest_kek::SATELLITES so a deployment-seed rotation re-keys it rather
    -- than stranding a published selector. `public_dns_value` is the unsealed
    -- public half: the TXT record at <selector>._domainkey.<domain>.
    -- Deployment infrastructure, not user data: losing a row loses nothing a
    -- user authored, and the rotation path mints a successor.
    CREATE TABLE IF NOT EXISTS mail_dkim_keys (
        domain           TEXT NOT NULL,
        selector         TEXT NOT NULL,
        alg              TEXT NOT NULL,
        key_wrapped      BLOB NOT NULL,
        public_dns_value TEXT NOT NULL,
        created_at       INTEGER NOT NULL,
        -- Epoch-millis when a factory reset carried this key onto the fresh
        -- database; NULL on every key a domain signs under. A carried key is
        -- held for the door that re-registers its domain and is no selector to
        -- any reader until then (mail_dkim_key.rs, `Carried keys`).
        carried_at       INTEGER,
        PRIMARY KEY (domain, selector)
    );
";

/// A folder's **inbox segment** (`file-sync.md` § Third-party deposit
/// ingress): what third-party principals deposited into the folder, each item
/// sealed to the owner's recipient key, parked until a seat of the folder
/// adopts it into an ordinary change row. Not a folder entry: nothing here is
/// listed by `fauna.sync.files` or served.
pub(super) const MIGRATIONS_FOLDER_DEPOSIT_INBOX: &str = "
    -- `sealed` is a recipient-sealed DepositEnvelope (name, media type, bytes
    -- -- all content); the nest holds no key that opens it. `byte_length` is
    -- the sealed blob's size, the at-rest metadata every sealed kind carries.
    -- `principal_id` names the depositor (third_party_principals.principal_id)
    -- and outlives a revoke: the item is the user's once accepted. A deleted
    -- folder takes its unadopted items with it, as it takes its files.
    CREATE TABLE IF NOT EXISTS folder_deposit_inbox (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        folder_id    INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
        principal_id BLOB NOT NULL,
        sealed       BLOB NOT NULL,
        byte_length  INTEGER NOT NULL,
        received_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_folder_deposit_inbox_folder
        ON folder_deposit_inbox(folder_id, id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The three flag columns ARE the place: a roster row that omits one is
    /// refused rather than resting a default nobody chose.
    #[test]
    fn a_folder_member_row_requires_all_three_flags() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO folders (id, name, actor_id, created_at) VALUES (1, 'photos', ?1, 0)",
            rusqlite::params![vec![0xaau8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folder_members (folder_id, device_id, originates, accepts, applies_deletes)
             VALUES (1, ?1, 1, 1, 0)",
            rusqlite::params![vec![1u8; 32]],
        )
        .unwrap();
        let rejected = conn
            .execute(
                "INSERT INTO folder_members (folder_id, device_id, originates, accepts)
                 VALUES (1, ?1, 1, 1)",
                rusqlite::params![vec![2u8; 32]],
            )
            .expect_err("a row without applies_deletes was admitted");
        assert!(
            rejected.to_string().contains("NOT NULL"),
            "expected a NOT NULL violation, got: {rejected}"
        );
    }

    #[test]
    fn domain_expiry_table_created() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='domain_expiry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "domain_expiry table should exist after migrations");
    }

    #[test]
    fn bridge_absorption_tables_created() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        for table in [
            "bridge_service_users",
            "bridge_wrapped_mls_blobs",
            "bridge_mls_snapshot_blobs",
            "bridge_wrapped_submission_tokens",
            "bridge_tls_cert_blobs",
            "bridge_audit_events",
            "bridge_submission_quota",
            "segment_records",
        ] {
            let count: i32 = conn
                .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1")
                .unwrap()
                .query_row([table], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist after migration");
        }
    }

    #[test]
    fn run_migrations_seeds_builtin_model_versions() {
        // Fresh DB: every built-in factor is seeded at its source `scorer_version`
        // (the model-version registry the re-score drain compares against, § 2.5).
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        for (factor, version) in fauna_core::scoring::builtin_factor_versions() {
            let v: i64 = conn
                .query_row(
                    "SELECT version FROM model_versions WHERE model_kind = ?1",
                    [factor],
                    |r| r.get(0),
                )
                .unwrap_or_else(|e| panic!("model_versions row for {factor} missing: {e}"));
            assert_eq!(v as u32, version, "{factor} seeded at wrong version");
        }
    }

    #[test]
    fn seed_model_versions_is_monotonic_and_idempotent() {
        // A runtime bump ABOVE the built-in constant must survive a re-run of the
        // boot reconcile (monotonic — never lower a version the registry advanced
        // past the compile-time constant), and re-running is otherwise a no-op.
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "UPDATE model_versions SET version = 99 WHERE model_kind = 'clamav'",
            [],
        )
        .unwrap();
        // Re-run the whole migration path (as a second boot would).
        run_migrations(&conn).unwrap();
        let v: i64 = conn
            .query_row(
                "SELECT version FROM model_versions WHERE model_kind = 'clamav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            v, 99,
            "seed must not lower a version above the built-in constant"
        );
    }

    // ── Schema version & downgrade detection (version-compatibility.md § 2.2) ──

    /// Force the `schema_meta` row to `(schema_version, min_reader_version)`,
    /// creating the table+row if absent. Test-only helper to seed the verdict
    /// matrix without a newer binary.
    fn seed_schema_meta(conn: &Connection, v: u32, min: u32) {
        conn.execute_batch(MIGRATIONS_SCHEMA_META).unwrap();
        conn.execute(
            "INSERT INTO schema_meta (id, schema_version, min_reader_version, updated_at) \
             VALUES (1, ?1, ?2, 0) \
             ON CONFLICT(id) DO UPDATE SET schema_version=excluded.schema_version, \
                min_reader_version=excluded.min_reader_version",
            rusqlite::params![v as i64, min as i64],
        )
        .unwrap();
    }

    #[test]
    fn fresh_db_records_baseline_schema_meta() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let (v, min) = read_schema_meta(&conn).unwrap();
        assert_eq!(
            (v, min),
            (CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION)
        );
    }

    /// Rows at the `0` sentinel — where segment compaction re-inserts its
    /// survivors — each get a coordinate: distinct, per scope, in append order,
    /// so a feed walk can return them. Left at the sentinel they would be
    /// invisible to every walk forever.
    #[test]
    fn the_changed_seq_backfill_gives_sentinel_rows_distinct_coordinates() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        // Two scopes' worth of rows at the sentinel, as compaction writes them.
        for (scope, kind, tag) in [
            (1u8, "post", 1u8),
            (1, "post", 2),
            (1, "post", 3),
            (2, "post", 4),
            (1, "calendar", 5),
        ] {
            conn.execute(
                "INSERT INTO segment_records
                    (scope_id, kind, segment_id, record_cid, bucket, tombstoned, changed_seq)
                 VALUES (?1, ?2, 1, ?3, '2026-08', 0, 0)",
                rusqlite::params![&[scope; 32][..], kind, &[tag; 36][..]],
            )
            .unwrap();
        }

        backfill_segment_records_changed_seq(&conn).unwrap();

        let coords = |scope: u8, kind: &str| -> Vec<i64> {
            let mut stmt = conn
                .prepare(
                    "SELECT changed_seq FROM segment_records
                      WHERE scope_id = ?1 AND kind = ?2 ORDER BY rowid",
                )
                .unwrap();
            stmt.query_map(rusqlite::params![&[scope; 32][..], kind], |r| r.get(0))
                .unwrap()
                .collect::<Result<Vec<i64>, _>>()
                .unwrap()
        };
        // Per scope, and in append order — never a shared global counter, which
        // would make one scope's coordinates jump over another's.
        assert_eq!(coords(1, "post"), vec![1, 2, 3]);
        assert_eq!(coords(2, "post"), vec![1]);
        assert_eq!(coords(1, "calendar"), vec![1]);

        // A row written *after* the backfill keeps its own coordinate, and a
        // re-run changes nothing: the pass is idempotent, not a renumbering.
        conn.execute(
            "INSERT INTO segment_records
                (scope_id, kind, segment_id, record_cid, bucket, tombstoned, changed_seq)
             VALUES (?1, 'post', 1, ?2, '2026-08', 0, 9)",
            rusqlite::params![&[1u8; 32][..], &[9u8; 36][..]],
        )
        .unwrap();
        backfill_segment_records_changed_seq(&conn).unwrap();
        assert_eq!(coords(1, "post"), vec![1, 2, 3, 9]);
    }

    /// The self-heal half: a row that somehow arrives at the sentinel *after*
    /// others hold coordinates is placed **above** the scope's current max, so
    /// no already-walked coordinate is reused or lowered — a replica's frontier
    /// can never be made to skip a row.
    #[test]
    fn a_late_sentinel_row_lands_above_the_scopes_existing_coordinates() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        for (tag, seq) in [(1u8, 4i64), (2, 7), (3, 0)] {
            conn.execute(
                "INSERT INTO segment_records
                    (scope_id, kind, segment_id, record_cid, bucket, tombstoned, changed_seq)
                 VALUES (?1, 'post', 1, ?2, '2026-08', 0, ?3)",
                rusqlite::params![&[1u8; 32][..], &[tag; 36][..], seq],
            )
            .unwrap();
        }

        backfill_segment_records_changed_seq(&conn).unwrap();

        let assigned: i64 = conn
            .query_row(
                "SELECT changed_seq FROM segment_records WHERE record_cid = ?1",
                rusqlite::params![&[3u8; 36][..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            assigned, 8,
            "above the scope's max of 7, never reusing 1..7"
        );
    }

    /// A nest's first boot: an empty file, no `schema_meta` yet. It reads as the
    /// genesis baseline and is upgraded, never refused.
    #[test]
    fn an_empty_database_reads_as_the_genesis_baseline() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            read_schema_meta(&conn).unwrap(),
            (BASELINE_SCHEMA_VERSION, BASELINE_SCHEMA_VERSION)
        );
        assert_eq!(
            check_schema_compatibility(&conn).unwrap(),
            SchemaVerdict::UpgradeOrCurrent
        );
        check_genesis(&conn).expect("an empty file is a first boot, not a pre-genesis DB");
    }

    /// The genesis marks its database, and a marked database is its own.
    #[test]
    fn the_genesis_stamps_its_application_id_and_admits_its_own_database() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let id: i32 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, NEST_DB_APPLICATION_ID);
        check_genesis(&conn).expect("a genesis-written database is admitted");
    }

    /// The flipped "an older shape still reads" pin: a database the genesis did
    /// not write is refused before anything reads it — whether it carries the
    /// first history's stamp (the last one it ever wrote was (78, 42)), the
    /// first genesis's mark and stamp (the 79..=124 run, last at (124, 118)),
    /// or some other file format's mark.
    #[test]
    fn a_database_the_genesis_did_not_write_is_refused() {
        let pre_genesis = Connection::open_in_memory().unwrap();
        seed_schema_meta(&pre_genesis, 78, 42);
        pre_genesis
            .execute_batch("CREATE TABLE users (actor_id BLOB PRIMARY KEY);")
            .unwrap();
        let err = check_genesis(&pre_genesis).expect_err("a pre-genesis DB must be refused");
        assert_eq!(
            err.downcast_ref::<PreGenesisDatabase>(),
            Some(&PreGenesisDatabase { application_id: 0 })
        );

        let first_genesis = Connection::open_in_memory().unwrap();
        let first_genesis_id = i32::from_be_bytes(*b"NEST");
        seed_schema_meta(&first_genesis, 124, 118);
        first_genesis
            .execute_batch(&format!("PRAGMA application_id = {first_genesis_id};"))
            .unwrap();
        let err = check_genesis(&first_genesis)
            .expect_err("a database of the collapsed 79..=124 run must be refused");
        assert_eq!(
            err.downcast_ref::<PreGenesisDatabase>(),
            Some(&PreGenesisDatabase {
                application_id: first_genesis_id
            })
        );

        let foreign = Connection::open_in_memory().unwrap();
        foreign
            .execute_batch("PRAGMA application_id = 7; CREATE TABLE t (x INTEGER);")
            .unwrap();
        let err = check_genesis(&foreign).expect_err("a foreign file must be refused");
        assert_eq!(
            err.downcast_ref::<PreGenesisDatabase>(),
            Some(&PreGenesisDatabase { application_id: 7 })
        );
    }

    /// The genesis rows land once: four tiers — the backup tier storage-only —
    /// and the deployment secrets, which a second boot keeps rather than
    /// re-mints (a re-mint would invalidate every SRS address and unsubscribe
    /// link in flight, and re-key the nest).
    #[test]
    fn the_genesis_rows_are_seeded_once() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let backup: (i64, i64) = conn
            .query_row(
                "SELECT max_inbox_bytes, max_feeds FROM tiers WHERE name = 'backup'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            backup,
            (0, 0),
            "the backup tier holds no inbox and no feeds"
        );
        let tiers: i64 = conn
            .query_row("SELECT COUNT(*) FROM tiers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tiers, 4);

        let secrets = |conn: &Connection| -> (Vec<u8>, Vec<u8>, Vec<u8>) {
            (
                conn.query_row("SELECT secret FROM mail_srs_secrets", [], |r| r.get(0))
                    .unwrap(),
                conn.query_row(
                    "SELECT secret FROM mail_list_unsubscribe_secrets",
                    [],
                    |r| r.get(0),
                )
                .unwrap(),
                conn.query_row("SELECT secret_key FROM nest_keypair", [], |r| r.get(0))
                    .unwrap(),
            )
        };
        let first = secrets(&conn);
        run_migrations(&conn).unwrap();
        assert_eq!(
            secrets(&conn),
            first,
            "a second boot must keep every secret"
        );
    }

    /// A reserved folder is inserted by name alone; the next boot gives it the
    /// `name_hash` companion it is addressed by — the standing input that keeps
    /// [`reconcile_path_sealing_companions`] in the runner.
    #[test]
    fn a_reserved_folder_gains_its_name_hash_on_the_next_boot() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO folders (name, actor_id, created_at, high_cadence)
             VALUES ('__config', ?1, 0, 0)",
            rusqlite::params![vec![0xaau8; 32]],
        )
        .unwrap();
        run_migrations(&conn).unwrap();
        let hash: Vec<u8> = conn
            .query_row(
                "SELECT name_hash FROM folders WHERE name = '__config'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            hash,
            fauna_core::path_crypto::set_name_hash("__config").to_vec()
        );
    }

    #[test]
    fn verdict_matrix() {
        let bin = CURRENT_SCHEMA_VERSION;
        // db_v ≤ bin → UpgradeOrCurrent.
        let conn = Connection::open_in_memory().unwrap();
        seed_schema_meta(&conn, bin, bin);
        assert_eq!(
            check_schema_compatibility(&conn).unwrap(),
            SchemaVerdict::UpgradeOrCurrent
        );
        // db_v > bin but db_min ≤ bin → NewerCompatible (additive-only newer DB).
        let conn = Connection::open_in_memory().unwrap();
        seed_schema_meta(&conn, bin + 1, bin);
        assert_eq!(
            check_schema_compatibility(&conn).unwrap(),
            SchemaVerdict::NewerCompatible
        );
        // db_v > bin and db_min > bin → Incompatible (breaking change predated).
        let conn = Connection::open_in_memory().unwrap();
        seed_schema_meta(&conn, bin + 3, bin + 1);
        assert_eq!(
            check_schema_compatibility(&conn).unwrap(),
            SchemaVerdict::Incompatible {
                db_v: bin + 3,
                db_min: bin + 1,
                bin_v: bin,
            }
        );
    }

    #[test]
    fn record_does_not_restamp_version_down() {
        // An older binary operating a NewerCompatible DB must NOT lower the
        // recorded schema_version (or the min_reader floor) — § 2.2 "Do not
        // restamp down".
        let conn = Connection::open_in_memory().unwrap();
        seed_schema_meta(&conn, CURRENT_SCHEMA_VERSION + 5, CURRENT_SCHEMA_VERSION);
        record_schema_meta(&conn).unwrap();
        let (v, min) = read_schema_meta(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION + 5, "version must not drop");
        assert_eq!(min, CURRENT_SCHEMA_VERSION, "min_reader must not drop");
    }

    #[test]
    fn run_migrations_is_idempotent_for_schema_meta() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Exactly one row, still at the baseline.
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            read_schema_meta(&conn).unwrap(),
            (CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION)
        );
    }

    /// A binary of a retired schema number must read a genesis database as
    /// `Incompatible` and boot degraded — never run its own migration runner
    /// over it (version-compatibility.md § 2.2, the genesis paragraph). Its
    /// rule is `classify_schema` at its own version, and every retired number
    /// sits at or below `RETIRED_LINEAGE_MAX_SCHEMA_VERSION`.
    #[test]
    fn a_pre_genesis_binary_reads_a_genesis_database_as_incompatible() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let (db_v, db_min) = read_schema_meta(&conn).unwrap();
        for bin_v in [RETIRED_LINEAGE_MAX_SCHEMA_VERSION, 79, 78, 42, 1] {
            assert_eq!(
                classify_schema(db_v, db_min, bin_v),
                SchemaVerdict::Incompatible {
                    db_v,
                    db_min,
                    bin_v
                },
                "a binary at schema {bin_v} must boot degraded on a genesis database"
            );
        }
        // The genesis binary itself reads its own stamp as current.
        assert_eq!(
            check_schema_compatibility(&conn).unwrap(),
            SchemaVerdict::UpgradeOrCurrent
        );
    }

    /// `segment_records` and the indexes its lookup paths use exist.
    #[test]
    fn cache_schema_creates_segment_records_table() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        // Table exists.
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='segment_records'",
                [],
                |r| r.get(0),
            )
            .expect("count table");
        assert_eq!(table_count, 1, "segment_records table must exist");

        // Index on (record_cid, kind) exists (the actor-by-record-cid lookup path).
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segment_records_record_cid'",
                [],
                |r| r.get(0),
            )
            .expect("count idx");
        assert_eq!(idx_count, 1, "record_cid index must exist");

        // Index on (scope_id, kind, bucket) exists (the per-scope scan path).
        let idx_scope_kind_bucket: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segment_records_scope_kind_bucket'",
                [],
                |r| r.get(0),
            )
            .expect("count scope_kind_bucket idx");
        assert_eq!(
            idx_scope_kind_bucket, 1,
            "scope_kind_bucket index must exist"
        );
    }

    /// The primary-domain-rename state machine schema
    /// (`mail-primary-domain-rename.md` § Data): `run_migrations` creates the
    /// `mail_domain_renames` table + its three indexes, and the migration const
    /// is idempotent (all `CREATE ... IF NOT EXISTS`).
    #[test]
    fn run_migrations_creates_mail_domain_renames() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mail_domain_renames'",
                [],
                |r| r.get(0),
            )
            .expect("count table");
        assert_eq!(table_count, 1, "mail_domain_renames table must exist");

        for idx in [
            "idx_mail_domain_renames_active",
            "idx_mail_domain_renames_grace_ends",
            "idx_mail_domain_renames_actor_started",
        ] {
            let idx_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [idx],
                    |r| r.get(0),
                )
                .unwrap_or_else(|e| panic!("count index {idx}: {e}"));
            assert_eq!(idx_count, 1, "index {idx} must exist");
        }

        // Re-applying the rename migration alone is a no-op (idempotent).
        conn.execute_batch(MIGRATIONS_MAIL_DOMAIN_RENAMES)
            .expect("re-applying the rename migration must be idempotent");
    }

    /// Capability-grant storage (design § Phase 2 Step 2 § 2.4): `run_migrations`
    /// creates the `capability_grants` table + its holder index, and the const
    /// is idempotent (all `CREATE ... IF NOT EXISTS`).
    #[test]
    fn run_migrations_creates_capability_grants() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='capability_grants'",
                [],
                |r| r.get(0),
            )
            .expect("count table");
        assert_eq!(table_count, 1, "capability_grants table must exist");

        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_capability_grants_holder'",
                [],
                |r| r.get(0),
            )
            .expect("count index");
        assert_eq!(idx_count, 1, "idx_capability_grants_holder must exist");

        // Re-applying the migration alone is a no-op (idempotent).
        conn.execute_batch(MIGRATIONS_CAPABILITY_GRANTS)
            .expect("re-applying the capability-grants migration must be idempotent");
    }

    /// ATProto full-PDS F1 batch (`atproto-pds-full.md` § Nest state schema):
    /// `run_migrations` creates all seven tables + the session-by-credential
    /// index, the kill-switch default is ON, and the const is idempotent.
    #[test]
    fn run_migrations_creates_atproto_pds_tables() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        for table in [
            "atproto_app_credentials",
            "atproto_sessions",
            "atproto_oauth_grants",
            "atproto_native_records",
            "atproto_preferences",
            "atproto_blobs",
            "atproto_account_settings",
            "atproto_session_secret_blobs",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .expect("count table");
            assert_eq!(count, 1, "{table} must exist");
        }

        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_atproto_sessions_credential'",
                [],
                |r| r.get(0),
            )
            .expect("count index");
        assert_eq!(idx_count, 1, "idx_atproto_sessions_credential must exist");

        // The external-apps kill-switch defaults ON (atproto-pds-full.md § F1
        // detail — the login plane is inert-by-construction without credentials;
        // the switch is opt-out defense-in-depth, so a fresh row allows).
        conn.execute(
            "INSERT INTO atproto_account_settings (actor_id, updated_at) VALUES (?1, 0)",
            rusqlite::params![vec![7u8; 32]],
        )
        .expect("insert settings row");
        let enabled: i64 = conn
            .query_row(
                "SELECT external_apps_enabled FROM atproto_account_settings",
                [],
                |r| r.get(0),
            )
            .expect("read flag");
        assert_eq!(enabled, 1, "external_apps_enabled must default ON");

        // Re-applying the migration alone is a no-op (idempotent).
        conn.execute_batch(MIGRATIONS_ATPROTO_PDS)
            .expect("re-applying the atproto-pds migration must be idempotent");
    }

    /// S4-A: the integration level is nest state (`ui/atproto.md` § State & data
    /// shape) — an additive column defaulting to the ratified OFF posture, and
    /// `atproto_identities.status` admits the layer-2 `deactivated` value.
    #[test]
    fn run_migrations_adds_integration_level_and_deactivated_status() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        let col = column_defs(&conn, "atproto_account_settings")
            .expect("settings columns")
            .into_iter()
            .find(|c| c.name == "integration_level")
            .expect("integration_level column must exist");
        assert!(col.notnull, "level must be NOT NULL");
        assert_eq!(
            col.dflt.as_deref(),
            Some("'off'"),
            "default OFF is the ratified consent posture"
        );

        // A sparse row (the kill-switch path inserts without a level) reads OFF.
        conn.execute(
            "INSERT INTO atproto_account_settings (actor_id, updated_at) VALUES (?1, 0)",
            rusqlite::params![vec![9u8; 32]],
        )
        .expect("insert sparse settings row");
        let level: String = conn
            .query_row(
                "SELECT integration_level FROM atproto_account_settings WHERE actor_id = ?1",
                rusqlite::params![vec![9u8; 32]],
                |r| r.get(0),
            )
            .expect("read level");
        assert_eq!(level, "off");

        // Every ratified level value is storable.
        for good in ["off", "linked", "hosted_visible", "hosted_full"] {
            conn.execute(
                "UPDATE atproto_account_settings SET integration_level = ?1 WHERE actor_id = ?2",
                rusqlite::params![good, vec![9u8; 32]],
            )
            .unwrap_or_else(|e| panic!("level {good} must be accepted: {e}"));
        }

        // Deliberately NO column-level CHECK: `reconcile_added_columns` rebuilds
        // a missing column from `pragma_table_info`, which carries type/NOT
        // NULL/DEFAULT but *not* CHECK — so a CHECK here would exist on a fresh
        // database and be silently absent on every upgraded one, i.e. a test
        // that passes in-memory while production runs unguarded. Validity is
        // enforced where it is uniform: `IntegrationLevel::from_wire` at the
        // handler, the column's only writer. This asserts the parity that the
        // absent CHECK buys — a database whose table predates the column gets
        // the identical column from the reconciler.
        let upgraded = Connection::open_in_memory().expect("open upgrade target");
        upgraded
            .execute_batch(
                "CREATE TABLE atproto_account_settings (
                     actor_id              BLOB PRIMARY KEY,
                     external_apps_enabled INTEGER NOT NULL DEFAULT 1,
                     updated_at            INTEGER NOT NULL
                 );",
            )
            .expect("build the table without the column");
        run_migrations(&upgraded).expect("reconcile the column onto it");
        let upgraded_col = column_defs(&upgraded, "atproto_account_settings")
            .expect("upgraded columns")
            .into_iter()
            .find(|c| c.name == "integration_level")
            .expect("the upgrade path must materialise integration_level");
        assert_eq!(
            (
                upgraded_col.ty.to_uppercase(),
                upgraded_col.notnull,
                upgraded_col.dflt
            ),
            (col.ty.to_uppercase(), col.notnull, col.dflt),
            "fresh and upgraded databases must agree on the level column"
        );

        // Layer-2 deactivation retains the identity row under a third status,
        // and S5 slice 5's delete sweep under a fourth.
        conn.execute(
            "INSERT INTO atproto_identities (actor_id, method, status, created_at, updated_at)
             VALUES (?1, 'plc', 'deactivated', 0, 0)",
            rusqlite::params![vec![10u8; 32]],
        )
        .expect("'deactivated' must be an admissible identity status");
        conn.execute(
            "INSERT INTO atproto_identities (actor_id, method, status, created_at, updated_at)
             VALUES (?1, 'plc', 'deleted', 0, 0)",
            rusqlite::params![vec![11u8; 32]],
        )
        .expect("'deleted' must be an admissible identity status");
    }

    /// Community-labeler registry (labeler-registry design § 3): `run_migrations`
    /// creates the `labelers` + `labeler_subscriptions` tables + the subscription
    /// index, and the const is idempotent (all `CREATE ... IF NOT EXISTS`).
    #[test]
    fn run_migrations_creates_labeler_registry() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        for table in ["labelers", "labeler_subscriptions"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .expect("count table");
            assert_eq!(count, 1, "{table} table must exist");
        }

        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_labeler_subs_labeler'",
                [],
                |r| r.get(0),
            )
            .expect("count index");
        assert_eq!(idx_count, 1, "idx_labeler_subs_labeler must exist");

        // The authenticated-caller quota column exists (the un-rotatable identity the per-caller cap keys on).
        let has_caller_actor: bool = column_defs(&conn, "labelers")
            .expect("labelers columns")
            .iter()
            .any(|c| c.name == "caller_actor");
        assert!(has_caller_actor, "labelers.caller_actor column must exist");

        // Re-applying the migration alone is a no-op (idempotent).
        conn.execute_batch(MIGRATIONS_LABELERS)
            .expect("re-applying the labeler-registry migration must be idempotent");
    }

    /// A fresh DB's `bridge_service_users.role` CHECK admits the additive
    /// `'content-processor'` holder role (design § 2.4) and still rejects an
    /// unknown role.
    #[test]
    fn run_migrations_bridge_service_users_role_admits_content_processor() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        // The widened CHECK accepts 'content-processor'.
        conn.execute(
            "INSERT INTO bridge_service_users \
             (ed25519_pubkey, role, bridge_id, status, created_at) \
             VALUES (?1, 'content-processor', 'cp-1', 'pending', 1)",
            rusqlite::params![vec![9u8; 32]],
        )
        .expect("content-processor role must be admitted");

        // …and the other roles still work…
        conn.execute(
            "INSERT INTO bridge_service_users \
             (ed25519_pubkey, role, bridge_id, status, created_at) \
             VALUES (?1, 'mta', 'mta-1', 'approved', 1)",
            rusqlite::params![vec![10u8; 32]],
        )
        .expect("mta role must still be admitted");

        // …but an unknown role is still rejected by the CHECK.
        let bogus = conn.execute(
            "INSERT INTO bridge_service_users \
             (ed25519_pubkey, role, bridge_id, status, created_at) \
             VALUES (?1, 'nonsense', 'x-1', 'pending', 1)",
            rusqlite::params![vec![11u8; 32]],
        );
        assert!(bogus.is_err(), "unknown role must fail the CHECK");
    }

    /// A fresh DB's `bridge_service_users.role` CHECK admits the additive
    /// `'atproto.pds'` role (the out-of-process ATProto PDS host bridge,
    /// `atproto-pds-bridge.md`) alongside the existing roles, and still rejects an
    /// unknown role.
    #[test]
    fn run_migrations_bridge_service_users_role_admits_atproto_pds() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        run_migrations(&conn).expect("apply schema");

        for (i, (role, id)) in [
            ("atproto.pds", "atproto-1"),
            ("mta", "mta-1"),
            ("mda", "mda-1"),
            ("content-processor", "cp-1"),
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO bridge_service_users \
                 (ed25519_pubkey, role, bridge_id, status, created_at) \
                 VALUES (?1, ?2, ?3, 'pending', 1)",
                rusqlite::params![vec![(i + 1) as u8; 32], role, id],
            )
            .unwrap_or_else(|e| panic!("role {role} must be admitted: {e}"));
        }

        let bogus = conn.execute(
            "INSERT INTO bridge_service_users \
             (ed25519_pubkey, role, bridge_id, status, created_at) \
             VALUES (?1, 'atproto', 'x-1', 'pending', 1)",
            rusqlite::params![vec![7u8; 32]],
        );
        assert!(
            bogus.is_err(),
            "the bare 'atproto' role (not 'atproto.pds') must fail the CHECK"
        );
    }

    #[test]
    fn snapshots_table_has_message_kind_columns() {
        let conn = Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("apply");
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(snapshots)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        for required in ["message_kind", "message_manifest", "placement_manifest"] {
            assert!(
                cols.contains(&required.to_string()),
                "missing column {required}"
            );
        }
    }

    #[test]
    fn restore_history_table_exists() {
        let conn = Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("apply");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='restore_history'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_restore_history_actor'",
                [],
                |r| r.get(0),
            )
            .expect("idx count");
        assert_eq!(idx_count, 1, "restore_history actor index must exist");
    }

    #[test]
    fn bridge_restore_divergence_table_exists() {
        let conn = Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("apply");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bridge_restore_divergence'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "bridge_restore_divergence table must exist");
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_restore_divergence_snapshot_actor'",
                [],
                |r| r.get(0),
            )
            .expect("idx count");
        assert_eq!(
            idx_count, 1,
            "bridge_restore_divergence snapshot/actor index must exist"
        );
    }

    /// Every boot re-runs the whole genesis, so a second run over a fresh
    /// database must be a clean no-op.
    #[test]
    fn run_migrations_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("first run");
        run_migrations(&conn).expect("second run must be idempotent");
    }

    /// Sorted column names of `table` in `conn`.
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        cols.sort();
        cols
    }

    /// Every line of `table`'s stored `CREATE TABLE` text that carries a comma
    /// AFTER a `--` comment marker — the lines the comma trap (the module
    /// doc's § No commas) forbids. Read from the connection under test, so
    /// the schema text scanned is the one whose `DROP COLUMN` just failed. The
    /// guard below prints these in its panic: the trap's `incomplete input`
    /// names the column SQLite failed on — usually a LATER column than the one
    /// whose comment carries the comma — so a reader sent to "an earlier
    /// column's comment" still had a whole `CREATE TABLE` to search; this
    /// narrows it to the lines that can actually be the offender.
    fn comma_bearing_comment_lines(conn: &Connection, table: &str) -> Vec<String> {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .unwrap_or_default();
        sql.lines()
            .enumerate()
            .filter(|(_, line)| {
                line.find("--")
                    .map(|i| line[i..].contains(','))
                    .unwrap_or(false)
            })
            .map(|(i, line)| format!("line {}: {}", i + 1, line.trim()))
            .collect()
    }

    /// The additive backstop's guard, over **every** managed table: model a
    /// deployment created at an older schema by dropping
    /// every additive (nullable or constant-default, droppable) column from a
    /// freshly-migrated database, then assert `run_migrations` reconciles each
    /// table back to its fresh column set. This is the class guard for the
    /// outage — a column added to a `CREATE TABLE` block without a
    /// paired `ALTER` is now auto-reconciled, so the omission can no longer brick
    /// an existing `/data`. Without `reconcile_added_columns` this is RED for the
    /// many columns that have no hand-written `ALTER` catch-up.
    #[test]
    fn reconciler_restores_dropped_additive_columns_for_every_table() {
        let reference = Connection::open_in_memory().unwrap();
        run_migrations(&reference).unwrap();

        // One deployment at the current schema, then drop every additive
        // (nullable or constant-default, droppable) column across every table —
        // standing in for many columns introduced across many later versions,
        // all to be restored in a single reconcile pass. `apply_genesis` (no
        // reconcile) builds the fresh starting point.
        let old = Connection::open_in_memory().unwrap();
        apply_genesis(&old).unwrap();

        let mut dropped_by_table: Vec<(String, Vec<String>)> = Vec::new();
        for table in managed_tables(&reference).unwrap() {
            let mut dropped = Vec::new();
            for col in column_defs(&reference, &table).unwrap() {
                let additive = !col.notnull || col.dflt.is_some();
                if !additive {
                    continue;
                }
                // DROP COLUMN refuses PK / indexed / FK-referenced / otherwise
                // constrained columns; skip those — not the forgotten-ALTER class.
                // A comma inside the column's own inline schema comment is a
                // THIRD, unintended reason DROP COLUMN can fail (`incomplete
                // input` — the module doc's § No commas) and must not be swallowed the
                // same way: it silently un-covers the column instead of
                // structurally refusing to drop it.
                let drop = format!("ALTER TABLE \"{table}\" DROP COLUMN \"{}\"", col.name);
                match old.execute_batch(&drop) {
                    Ok(()) => dropped.push(col.name),
                    Err(e) if e.to_string().contains("incomplete input") => panic!(
                        "`{table}`.`{}` cannot be DROPped: {e} — this is the comma-in-\
                         inline-comment trap (migrations.rs § No commas), not a structural \
                         refusal; the stray comma may be in THIS column's `--` comment \
                         or in an EARLIER column's comment within the same CREATE TABLE \
                         (a comma there misaligns the boundary SQLite computes for every \
                         later column too) — reword whichever one carries it, comma-free. \
                         Comma-bearing `--` comment lines in `{table}`'s stored schema \
                         text (the offender is among these):\n  {}",
                        col.name,
                        comma_bearing_comment_lines(&old, &table).join("\n  "),
                    ),
                    Err(_) => {}
                }
            }
            if !dropped.is_empty() {
                dropped_by_table.push((table, dropped));
            }
        }
        assert!(
            dropped_by_table.len() >= 5,
            "expected many tables to have droppable additive columns; only {} did — the guard \
             may have gone vacuous",
            dropped_by_table.len(),
        );

        // A single reconcile pass must restore every dropped column on every
        // table, with its type / NOT NULL / default intact. Calling
        // `reconcile_added_columns` directly (rather than `run_migrations`) keeps
        // this a focused unit test of the backstop and avoids re-running data
        // migrations against the deliberately-perturbed schema.
        reconcile_added_columns(&old).unwrap();

        for (table, dropped) in &dropped_by_table {
            let mut got = column_defs(&old, table).unwrap();
            let mut want = column_defs(&reference, table).unwrap();
            got.sort_by(|a, b| a.name.cmp(&b.name));
            want.sort_by(|a, b| a.name.cmp(&b.name));
            assert_eq!(
                got, want,
                "table `{table}` must reach its fresh column set after reconcile \
                 (dropped then expected restored: {dropped:?})",
            );
        }
    }

    /// The standing self-heal (`monetization.md` (2d) obligation (iv)):
    /// a subscriber granted whose approve crashed before its fan-out enqueue
    /// gets the `payment_entitled` request enqueued at boot — and a re-run enqueues
    /// nothing twice (idempotent by construction, so the sweep can stand as
    /// the every-boot self-heal).
    #[test]
    fn unlock_fanout_reconcile_enqueues_missed_grants_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn).unwrap();
        let author = vec![7u8; 32];
        let sub = vec![8u8; 32];
        conn.execute(
            "INSERT INTO subscription_tiers (author_id, name, rank, created_at, unlocks_post)
             VALUES (?1, 'post-unlock-a', 1, 0, '11'), (?1, 'gold', 2, 0, NULL)",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subscribers (author_id, subscriber_id, tier_name, approved_at)
             VALUES (?1, ?2, 'gold', 0)",
            rusqlite::params![author.as_slice(), sub.as_slice()],
        )
        .unwrap();

        reconcile_unlock_fanout_requests(&conn).unwrap();
        reconcile_unlock_fanout_requests(&conn).unwrap();

        let (count, entitled): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), MAX(payment_entitled) FROM subscribe_requests
                  WHERE tier_name = 'post-unlock-a' AND subscriber_id = ?1",
                rusqlite::params![sub.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "the missed grant is enqueued exactly once across re-runs"
        );
        assert_eq!(entitled, 1, "the row rides the payment_entitled drain lane");
    }

    /// The **structural** half: a
    /// wrap-less `subscribers` row already on disk is healed at the next boot.
    ///
    /// The sweep must not suppress on the row's mere existence: a nest that
    /// enrolled someone into a keyless tier without minting for them could
    /// otherwise never recover — no source-side fix reaches rows that are
    /// already on disk. Here `silver` has no `current_key_blobs` row at all, so
    /// no wrap exists and the enqueue must fire despite the enrolment.
    #[test]
    fn unlock_fanout_reconcile_heals_an_enrolled_but_unreadable_row() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn).unwrap();
        let author = vec![7u8; 32];
        let sub = vec![9u8; 32];
        conn.execute(
            "INSERT INTO subscription_tiers (author_id, name, rank, created_at, unlocks_post)
             VALUES (?1, 'silver', 1, 0, NULL), (?1, 'gold', 2, 0, NULL)",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        // The retired nest-side cascade's residue shape: enrolled in BOTH,
        // wrapped in neither.
        conn.execute(
            "INSERT INTO subscribers (author_id, subscriber_id, tier_name, approved_at)
             VALUES (?1, ?2, 'gold', 0), (?1, ?2, 'silver', 0)",
            rusqlite::params![author.as_slice(), sub.as_slice()],
        )
        .unwrap();

        reconcile_unlock_fanout_requests(&conn).unwrap();
        // Still idempotent — the pending-request guard, not the roster row, is
        // what keeps a re-run from duplicating.
        reconcile_unlock_fanout_requests(&conn).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name = 'silver' AND subscriber_id = ?1",
                rusqlite::params![sub.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "an enrolled-but-unreadable row must be healed exactly once — the \
             subscribers row is the social edge, not proof of a delivered wrap"
        );
    }

    /// The negative half of the same rule, and the granularity it must have:
    /// readability is **per subscriber**, not per tier.
    ///
    /// One tier, one blob, two enrolled subscribers — the blob wraps only the
    /// first. The wrapped one must be left alone (or the readability key
    /// degenerates into "enqueue everything every boot", a standing storm since
    /// each drained row costs the author's client a whole blob re-mint), and
    /// the unwrapped one must still be healed. A guard that asks the cheaper
    /// question — "does this tier have a blob at all?" — passes the first
    /// assertion and silently strands the second, which is the original defect
    /// wearing a different hat.
    #[test]
    fn unlock_fanout_reconcile_reads_the_blob_per_subscriber_not_per_tier() {
        use fauna_core::data::Timestamp;
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        use fauna_core::identity::{ActorId, ActorKeypair};
        use fauna_core::subscription::types::{KemSuiteId, KeyBlob, KeyBlobEntry};

        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn).unwrap();
        let author = [7u8; 32];
        let sub = [9u8; 32];
        let unwrapped = [11u8; 32];
        conn.execute(
            "INSERT INTO subscription_tiers (author_id, name, rank, created_at, unlocks_post)
             VALUES (?1, 'silver', 1, 0, NULL), (?1, 'gold', 2, 0, NULL)",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subscribers (author_id, subscriber_id, tier_name, approved_at)
             VALUES (?1, ?2, 'gold', 0), (?1, ?2, 'silver', 0),
                    (?1, ?3, 'gold', 0), (?1, ?3, 'silver', 0)",
            rusqlite::params![author.as_slice(), sub.as_slice(), unwrapped.as_slice()],
        )
        .unwrap();

        // The wrap the healing pin's fixture lacks: `silver`'s current blob
        // carries an entry for `sub` — and deliberately not for `unwrapped`.
        let signer = ActorKeypair::from_secret([0x42u8; 32]);
        let blob = KeyBlob {
            author: ActorId(author),
            tier: "silver".into(),
            rotated_at: Timestamp(1_700_000_000_000_000),
            entries: vec![KeyBlobEntry {
                subscriber: ActorId(sub),
                encrypted_key: vec![1, 2, 3, 4],
                suite: KemSuiteId::Classical,
            }],
            signer: [0u8; 32],
            key_commitment: [0x6b; 32],
        };
        let (canon, env) = sign_envelope(&signer, &blob).unwrap();
        let stored = canonical_encode(&EmbedAsBytes::from_signed(canon, env)).unwrap();
        conn.execute(
            "INSERT INTO current_key_blobs
                 (author_id, tier_name, key_version, blob_hash, blob_data, created_at)
             VALUES (?1, 'silver', 1, x'00', ?2, 0)",
            rusqlite::params![author.as_slice(), stored],
        )
        .unwrap();

        reconcile_unlock_fanout_requests(&conn).unwrap();

        let wrapped_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name = 'silver' AND subscriber_id = ?1",
                rusqlite::params![sub.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            wrapped_rows, 0,
            "a subscriber the blob already wraps is readable — re-enqueueing \
             them would cost the author's client a blob re-mint every boot"
        );

        let unwrapped_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name = 'silver' AND subscriber_id = ?1",
                rusqlite::params![unwrapped.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            unwrapped_rows, 1,
            "the tier having *a* blob says nothing about THIS subscriber — an \
             enrolled reader missing from the roster must still be healed"
        );
    }

    /// The sweep's boundary predicates. A lapsed qualifying subscription and a
    /// qualifying rank below the target's (the free rank-0 follow included)
    /// enqueue nothing. A *designated* qualifying tier (a buyer of post B) is
    /// the case that got corrected: it must not reach post A
    /// (designated→designated — one cheap post unlocking every sold post), but
    /// it MUST reach the undesignated rank-0 `followers` tier, because
    /// `monetization.md:126` ratifies that buyers of a sold post also become
    /// followers. Before the fix, this test asserted zero rows here, which pinned
    /// the defect rather than the contract.
    #[test]
    fn unlock_fanout_reconcile_respects_expiry_rank_and_designation_bounds() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn).unwrap();
        let author = vec![7u8; 32];
        let (lapsed, follower, buyer) = (vec![1u8; 32], vec![2u8; 32], vec![3u8; 32]);
        conn.execute(
            "INSERT INTO subscription_tiers (author_id, name, rank, created_at, unlocks_post)
             VALUES (?1, 'post-unlock-a', 1, 0, '11'),
                    (?1, 'post-unlock-b', 1, 0, '22'),
                    (?1, 'gold', 2, 0, NULL),
                    (?1, 'followers', 0, 0, NULL)",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subscribers (author_id, subscriber_id, tier_name, approved_at, valid_until)
             VALUES (?1, ?2, 'gold', 0, 1),
                    (?1, ?3, 'followers', 0, NULL),
                    (?1, ?4, 'post-unlock-b', 0, NULL)",
            rusqlite::params![
                author.as_slice(),
                lapsed.as_slice(),
                follower.as_slice(),
                buyer.as_slice()
            ],
        )
        .unwrap();

        reconcile_unlock_fanout_requests(&conn).unwrap();
        // Idempotent by construction — a second pass must add nothing.
        reconcile_unlock_fanout_requests(&conn).unwrap();

        // Nothing designated is ever reached: post A is the designated→
        // designated edge from the post-B buyer, and neither the lapsed gold
        // holder nor the rank-0 follower qualifies for it.
        let designated: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name IN ('post-unlock-a', 'post-unlock-b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            designated, 0,
            "lapsed / rank-0 / designated→designated must enqueue no designated tier"
        );

        // The one row the contract requires: the post-B buyer becomes a
        // follower (rank-0, undesignated, client-mintable).
        let rows: Vec<(String, Vec<u8>)> = conn
            .prepare("SELECT tier_name, subscriber_id FROM subscribe_requests")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("followers".to_string(), buyer.clone())],
            "the sold-post buyer is enrolled as a follower exactly once (monetization.md:126)"
        );
    }

    /// Every boot re-runs this sweep, so
    /// if it ever selected a hidden target tier the hole arm A closes at
    /// grant-time would silently reopen on every restart. `backstage` sits at
    /// the SAME rank as the positive control `silver` — the fixture proves
    /// the exclusion is keyed on `hidden`, not on rank or reachability, by
    /// showing the sweep still fires for an ordinary tier at that rank.
    #[test]
    fn unlock_fanout_reconcile_never_enqueues_a_hidden_target_tier() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn).unwrap();
        let author = vec![7u8; 32];
        let sub = vec![8u8; 32];
        conn.execute(
            "INSERT INTO subscription_tiers
                 (author_id, name, rank, created_at, unlocks_post, hidden)
             VALUES (?1, 'silver', 1, 0, NULL, 0),
                    (?1, 'backstage', 1, 0, NULL, 1),
                    (?1, 'gold', 2, 0, NULL, 0)",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subscribers (author_id, subscriber_id, tier_name, approved_at)
             VALUES (?1, ?2, 'gold', 0)",
            rusqlite::params![author.as_slice(), sub.as_slice()],
        )
        .unwrap();

        reconcile_unlock_fanout_requests(&conn).unwrap();

        let silver_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name = 'silver' AND subscriber_id = ?1",
                rusqlite::params![sub.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            silver_count, 1,
            "the positive control at the same rank must still be enqueued — \
             proves the sweep genuinely reaches rank 1, not that it enqueues nothing at all"
        );

        let hidden_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE tier_name = 'backstage' AND subscriber_id = ?1",
                rusqlite::params![sub.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            hidden_count, 0,
            "a hidden tier must never be enqueued by the boot-time self-heal — \
             ruling 4, not offered and not subscribable"
        );
    }

    /// `segment_records` is keyed by the 36-byte `record_cid` (and indexed on it).
    #[test]
    fn segment_records_record_cid_is_the_key_column() {
        let conn = Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("apply");

        let cols: Vec<String> = column_defs(&conn, "segment_records")
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(
            cols.contains(&"record_cid".to_string()),
            "segment_records must have record_cid column; got: {cols:?}"
        );

        let has_cid_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segment_records_record_cid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_cid_idx, 1, "idx_segment_records_record_cid must exist");
    }

    /// `segment_records` carries the nullable `seq` column — the conversation
    /// per-channel sequence.
    #[test]
    fn segment_records_has_seq_column() {
        let conn = Connection::open_in_memory().expect("open");
        run_migrations(&conn).expect("apply");

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(segment_records)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        assert!(
            cols.contains(&"seq".to_string()),
            "segment_records must have seq column; got: {cols:?}"
        );
    }

    /// Every sealed-label and hash-companion column (`file-sync.md` § Sealed
    /// names & paths), by table.
    const PATH_SEALING_COLUMNS: &[(&str, &[&str])] = &[
        ("sync_changes", &["path_sealed"]),
        ("sync_devices", &["label_sealed"]),
        ("backup_custody", &["path_sealed"]),
        ("snapshot_files", &["path_hash", "path_sealed"]),
        ("snapshots", &["tag_hashes", "tags_sealed"]),
        (
            "sync_conflicts",
            &["path_hash", "path_sealed", "details_sealed"],
        ),
        (
            "folders",
            &[
                "name_hash",
                "name_sealed",
                "include_paths_sealed",
                "exclude_paths_sealed",
                "retention_policy_sealed",
            ],
        ),
        ("share_tokens", &["filename_sealed"]),
        ("import_sessions", &["source_hash", "source_sealed"]),
    ];

    fn index_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    #[test]
    fn fresh_db_carries_every_path_sealing_column_and_index() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        for (table, columns) in PATH_SEALING_COLUMNS {
            let have = column_names(&conn, table);
            for col in *columns {
                assert!(
                    have.contains(&col.to_string()),
                    "fresh DB must carry {table}.{col}; has {have:?}"
                );
            }
        }
        for idx in [
            "idx_snapshot_files_path_hash",
            "idx_folders_name_hash",
            "idx_import_sessions_source_hash_lock",
        ] {
            assert!(index_exists(&conn, idx), "fresh DB must carry {idx}");
        }
    }

    /// A migrated database with one scope folder, ready to take class-2 rows.
    fn scoped_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO folders (id, name, actor_id, created_at) VALUES (1, 'scope', ?1, 0)",
            rusqlite::params![vec![0xaau8; 32]],
        )
        .unwrap();
        conn
    }

    /// One class-2 row, shaped exactly as `record_account_state_entry`'s INSERT
    /// writes it: `item` is the item key (a distinct one is a *different* item
    /// at the same coordinate).
    fn insert_state_entry(
        conn: &Connection,
        item: u8,
        writer: u8,
        writer_seq: i64,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO sync_changes
                (actor_id, path_hash, size_bytes, change_type, created_at,
                 folder_id, item_class, origin_writer, origin_seq, entry_sealed)
             VALUES (?1, ?2, 0, 'state-put', 0, 1, 'state-entry', ?3, ?4, ?5)",
            rusqlite::params![
                vec![0xaau8; 32],
                vec![item; 32],
                vec![writer; 32],
                writer_seq,
                vec![0u8; 8],
            ],
        )
    }

    /// The structural twin of the `SeqReused` refusal
    /// (`account-data-plane.md` § Store logical schema — row uniqueness is per
    /// `(scope, writer, seq)`; `account-replica-posture.md` refinement 11): a
    /// second row at a coordinate the scope already holds is refused by the
    /// SCHEMA, not only by the probe the class-2 write path runs ahead of its
    /// INSERT. Without this pin a `CREATE UNIQUE INDEX` that quietly lost its
    /// `UNIQUE` — or a write path that grew a second INSERT skipping the probe
    /// — reads identical in review, in the logs, and on a green suite.
    #[test]
    fn a_reused_writer_coordinate_is_refused_at_the_schema() {
        let conn = scoped_db();
        insert_state_entry(&conn, 0x11, 0xbb, 7).expect("the first row at a fresh coordinate");
        let refused = insert_state_entry(&conn, 0x22, 0xbb, 7)
            .expect_err("the schema accepted a second row at a held coordinate");
        assert!(
            refused.to_string().contains("UNIQUE"),
            "expected a UNIQUE violation, got: {refused}"
        );
        // A different writer, and the same writer's next seq, are ordinary.
        insert_state_entry(&conn, 0x22, 0xcc, 7).expect("another writer at the same seq");
        insert_state_entry(&conn, 0x22, 0xbb, 8).expect("the same writer's next seq");

        assert!(index_exists(
            &conn,
            "idx_sync_changes_writer_coordinate_unique"
        ));
    }

    /// Every ordinary file row carries a NULL `origin_writer` / `origin_seq`,
    /// and SQLite holds NULLs distinct in a UNIQUE index — so the coordinate
    /// constraint must not narrow the file plane, whose whole history is many
    /// rows per `(folder, path)`.
    #[test]
    fn file_rows_without_a_writer_coordinate_are_untouched_by_the_unique_index() {
        let conn = scoped_db();
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO sync_changes
                    (actor_id, path_hash, size_bytes, change_type, created_at, folder_id)
                 VALUES (?1, ?2, 0, 'create', 0, 1)",
                rusqlite::params![vec![0xaau8; 32], vec![0x33u8; 32]],
            )
            .expect("an ordinary file row must not collide on a NULL coordinate");
        }
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_changes WHERE origin_writer IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 3);
    }
}
