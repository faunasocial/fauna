//! SQL table definitions for ActivityPub bridge state.

/// SQL to create ActivityPub tables — the bridge's genesis at the current
/// shape. The nest applies it with its additive column reconciler
/// (`fauna_nest::bridge_schema::apply_genesis`), so a new nullable or
/// defaulted column is added here and nowhere else.
pub const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS ap_accounts (
        actor_id          TEXT PRIMARY KEY,
        username          TEXT NOT NULL UNIQUE,
        actor_url         TEXT NOT NULL UNIQUE,
        encrypted_privkey BLOB NOT NULL,
        public_key_pem    TEXT NOT NULL,
        enabled           INTEGER NOT NULL DEFAULT 1,
        backfill          INTEGER NOT NULL DEFAULT 0,
        auto_accept_follows INTEGER NOT NULL DEFAULT 1,
        default_visibility TEXT NOT NULL DEFAULT 'public',
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL
    );

    -- The nest-level instance actor: ONE row (`id = 1` is enforced, not a
    -- convention), holding the RSA keypair that signs server-context outbound
    -- requests — the remote-actor fetch, which has no per-user actor to sign
    -- as. Custody is the per-account rule verbatim: the private key rests
    -- encrypted under the nest identity key (`activitypub/key_crypto.rs`).
    -- Never dropped or re-minted: a remote caches this key against our actor
    -- URL, so re-minting would invalidate every peer's cached copy.
    CREATE TABLE IF NOT EXISTS ap_instance_actor (
        id                INTEGER PRIMARY KEY CHECK (id = 1),
        encrypted_privkey BLOB NOT NULL,
        public_key_pem    TEXT NOT NULL,
        created_at        INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS ap_remote_actors (
        uri              TEXT PRIMARY KEY,
        inbox            TEXT NOT NULL,
        shared_inbox     TEXT,
        public_key_pem   TEXT NOT NULL,
        preferred_username TEXT,
        display_name     TEXT,
        avatar_url       TEXT,
        banner_url       TEXT,
        summary          TEXT,
        last_fetched     INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS ap_follows (
        id               INTEGER PRIMARY KEY,
        local_actor_id   TEXT NOT NULL,
        remote_actor_uri TEXT NOT NULL,
        direction        TEXT NOT NULL,
        state            TEXT NOT NULL DEFAULT 'pending',
        follow_activity_id TEXT,
        created_at       INTEGER NOT NULL,
        -- The user's own nickname for an outbound follow (`bridges.md`
        -- § Follows: an external ID plus an optional nickname). NULL when none
        -- was given, and always NULL on an inbound row.
        petname          TEXT,
        UNIQUE(local_actor_id, remote_actor_uri, direction)
    );
    CREATE INDEX IF NOT EXISTS idx_ap_follows_local ON ap_follows(local_actor_id);
    CREATE INDEX IF NOT EXISTS idx_ap_follows_remote ON ap_follows(remote_actor_uri);

    CREATE TABLE IF NOT EXISTS ap_post_map (
        fauna_post_id    TEXT NOT NULL,
        ap_url           TEXT NOT NULL,
        actor_id         TEXT NOT NULL,
        created_at       INTEGER NOT NULL,
        tombstoned       INTEGER NOT NULL DEFAULT 0,
        remote_actor_uri TEXT,
        PRIMARY KEY (fauna_post_id, ap_url)
    );
    CREATE INDEX IF NOT EXISTS idx_ap_post_map_url ON ap_post_map(ap_url);

    -- The uniform per-bridge Search-corpus policy (`content-index.md` § Bridge
    -- content in the Search corpus: 'indexing rides each bridge's nest-transit
    -- point, in lockstep — nobody runs an indexer'). Writing is Rust-side at
    -- the inbound transit points, for two reasons SQL cannot cover: the key is
    -- blake3(content_type:natural_id), and only **inbound, foreign-authored**
    -- Notes may index (an outbound push writes an `ap_post_map` row too, and
    -- its body is already in `content_fts` as the Fauna post). Removal is a
    -- trigger for the same reason the nostr store uses one — the corpus must
    -- leave in lockstep with the store row on *every* path, present and
    -- future, rather than the one path `handle_delete` happens to take today.
    --
    -- AP's removal is a **tombstone** (an UPDATE), not a DELETE: inbound
    -- `Delete` and the `Update{Note}` edit path both flip `tombstoned`, and no
    -- production path deletes the row. Both shapes get a trigger so neither can
    -- drift. A pushed local note never indexed, so its tombstone finds nothing
    -- — a clean no-op. `bridge_index_map` (a `bins/fauna-nest` table, like
    -- `content_fts`/`content_fts_map`) is what makes the blake3 key reachable
    -- from SQL; these triggers are why this DDL is nest-run only.
    CREATE TRIGGER IF NOT EXISTS ap_post_map_bridge_search_au
    AFTER UPDATE OF tombstoned ON ap_post_map
    WHEN new.tombstoned = 1 AND old.tombstoned = 0
    BEGIN
        DELETE FROM content_fts WHERE rowid IN (
            SELECT m.fts_rowid FROM content_fts_map m
            JOIN bridge_index_map b ON b.content_id = m.content_id
            WHERE b.content_type = 'bridge.activitypub' AND b.natural_id = old.ap_url);
        DELETE FROM content_fts_map WHERE content_id IN (
            SELECT content_id FROM bridge_index_map
            WHERE content_type = 'bridge.activitypub' AND natural_id = old.ap_url);
        DELETE FROM bridge_index_map
            WHERE content_type = 'bridge.activitypub' AND natural_id = old.ap_url;
    END;

    CREATE TRIGGER IF NOT EXISTS ap_post_map_bridge_search_ad
    AFTER DELETE ON ap_post_map
    BEGIN
        DELETE FROM content_fts WHERE rowid IN (
            SELECT m.fts_rowid FROM content_fts_map m
            JOIN bridge_index_map b ON b.content_id = m.content_id
            WHERE b.content_type = 'bridge.activitypub' AND b.natural_id = old.ap_url);
        DELETE FROM content_fts_map WHERE content_id IN (
            SELECT content_id FROM bridge_index_map
            WHERE content_type = 'bridge.activitypub' AND natural_id = old.ap_url);
        DELETE FROM bridge_index_map
            WHERE content_type = 'bridge.activitypub' AND natural_id = old.ap_url;
    END;

    CREATE TABLE IF NOT EXISTS ap_delivery_queue (
        id               INTEGER PRIMARY KEY,
        activity_json    TEXT NOT NULL,
        target_inbox     TEXT NOT NULL,
        created_at       INTEGER NOT NULL,
        attempts         INTEGER NOT NULL DEFAULT 0,
        next_retry_at    INTEGER NOT NULL,
        status           TEXT NOT NULL DEFAULT 'pending'
    );
    CREATE INDEX IF NOT EXISTS idx_ap_delivery_pending ON ap_delivery_queue(status, next_retry_at);

    CREATE TABLE IF NOT EXISTS ap_dead_inboxes (
        inbox_url        TEXT PRIMARY KEY,
        first_failure_at INTEGER NOT NULL,
        last_failure_at  INTEGER NOT NULL,
        failure_count    INTEGER NOT NULL DEFAULT 1,
        last_success_at  INTEGER
    );

    -- The oracle's ActivityPub class (TP11 -- `key-material-hierarchy.md`
    -- § Audience: deployment infrastructure → *The oracle*, the
    -- `activitypub.http_sig` row). One row per (account, third-party
    -- principal) that has had a request signed: the audit counters and the
    -- rate ceiling's fixed window (`fauna_core::identity_op::RateWindow`).
    -- NOT the authority -- every request re-resolves the principal row, its
    -- declared peer set and the owner's live `identity.op` grant. Deleted
    -- with the principal, the account's federation, and at succession.
    CREATE TABLE IF NOT EXISTS ap_oracle_principals (
        actor_id      TEXT NOT NULL,
        principal_id  BLOB NOT NULL,
        last_used_at  INTEGER,
        use_count     INTEGER NOT NULL DEFAULT 0,
        window_start  INTEGER NOT NULL DEFAULT 0,
        window_count  INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (actor_id, principal_id)
    );

    -- The oracle's per-operation record (the audit split): one row per
    -- signature the custodian made for a principal -- the class and its
    -- detail (`<method> <host>`). Bounded to the newest
    -- `activitypub::oracle::OPS_KEPT_PER_PRINCIPAL` per principal; the grant
    -- lifecycle itself is the client-signed `GrantEvent` log.
    CREATE TABLE IF NOT EXISTS ap_oracle_ops (
        id           INTEGER PRIMARY KEY,
        actor_id     TEXT NOT NULL,
        principal_id BLOB NOT NULL,
        at           INTEGER NOT NULL,
        class        TEXT NOT NULL,
        detail       TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_ap_oracle_ops_principal
        ON ap_oracle_ops(actor_id, principal_id, id);
";
