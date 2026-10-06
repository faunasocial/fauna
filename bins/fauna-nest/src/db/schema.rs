//! Unified schema definitions for fauna-nest storage.
//!
//! Content tables: `content`, `content_links`, `content_meta`, `content_fts`
//! These replace the old fragmented tables (posts, inbox, bridge_messages,
//! group_messages, channel_messages, post_index, post_tags, inbox_labels,
//! inbox_fts, search_index, search_index_map).

/// Core content table — every piece of content is a row here.
///
/// **`created_at` is epoch MICROSECONDS, for every writer without exception.**
/// That is the unit `fauna_core::data::Timestamp` declares, the unit
/// `ui/feed.md` § State & data shape → *The read model* names for the
/// chronological feed ordering, and the unit `SearchRequest`'s `before`/`after`
/// window cursors are declared in — so a row stored in seconds or milliseconds
/// compares as ~1970, sorts to the bottom of every mixed listing, and falls out
/// of every client-supplied time window. Three writers once disagreed (the
/// nostr sweep stored seconds; `db/inbox.rs` and the since-retired group store
/// stored milliseconds), and this comment is what stops the next writer guessing. No
/// boot pass may fix a wrong unit after the fact by magnitude: the column's
/// past is deliberately unbounded, so a legitimate 1970-1973 instant (a
/// backdated post, an archive import) is indistinguishable from a seconds value
/// (`ui/feed.md` § The read model). Store microseconds.
///
/// Use `db::now_epoch_micros()`, never `now_epoch_millis()`, for this
/// column. NB the sibling tables keep their own conventions on purpose:
/// `content_links.created_at` is milliseconds and `content.expires_at` is
/// seconds (single-writer, NIP-40 — `db/content.rs::list_expired_by_source`).
pub const SCHEMA_CONTENT: &str = "
    CREATE TABLE IF NOT EXISTS content (
        id          BLOB PRIMARY KEY,
        schema      TEXT NOT NULL,
        author      BLOB NOT NULL,
        created_at  INTEGER NOT NULL,
        payload     BLOB NOT NULL,
        expires_at  INTEGER,
        source      TEXT NOT NULL DEFAULT 'fauna',
        blob_hash   BLOB,
        -- The nest a discovery-fetched post was indexed FROM (`resolve_nest_
        -- from_post_index`, `get_post_references`).
        -- NULL for every post this nest itself hosts (created here, bridged
        -- in, or archive-imported) — its presence, not `source`, is what
        -- marks a `content` row as a remote federation stub this nest does
        -- not own (`interact_routes.rs`'s door refuses on it regardless of
        -- token). `source` on such a row instead carries the peer's real
        -- advertised protocol token (feed.md § PostSummary — `classify_
        -- sources()`), which this column's split-out makes possible: before
        -- it existed, the discovery poller had to overload `source` with the
        -- fetch URL itself to keep referral-chain resolution working.
        origin_nest_url TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_content_schema ON content(schema, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_content_author ON content(author, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_content_source ON content(source, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_content_expires ON content(expires_at) WHERE expires_at IS NOT NULL;
";

/// Typed relationship edges between content, actors, and external entities.
pub const SCHEMA_CONTENT_LINKS: &str = "
    CREATE TABLE IF NOT EXISTS content_links (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        link_type   TEXT NOT NULL,
        source_id   BLOB,
        target_id   BLOB,
        actor_id    BLOB,
        status      TEXT,
        metadata    BLOB,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        modseq BIGINT DEFAULT 1
    );
    CREATE INDEX IF NOT EXISTS idx_links_source ON content_links(source_id, link_type);
    CREATE INDEX IF NOT EXISTS idx_links_target ON content_links(target_id, link_type);
    CREATE INDEX IF NOT EXISTS idx_links_actor  ON content_links(actor_id, link_type, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_links_type   ON content_links(link_type, created_at DESC);
";

/// Uniqueness constraints for semantically-unique link types.
pub const SCHEMA_CONTENT_LINKS_UNIQUE: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_attendee
        ON content_links(source_id, actor_id) WHERE link_type = 'attendee';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_calendar_member
        ON content_links(source_id, target_id) WHERE link_type = 'calendar_member';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_co_host
        ON content_links(source_id, actor_id) WHERE link_type = 'co_host';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_delivery
        ON content_links(source_id, actor_id) WHERE link_type = 'delivery';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_event_group
        ON content_links(source_id, target_id) WHERE link_type = 'event_group';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_reminder
        ON content_links(source_id, actor_id) WHERE link_type = 'reminder';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_tag
        ON content_links(source_id, status) WHERE link_type = 'tag';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_label
        ON content_links(source_id, actor_id, status) WHERE link_type = 'label';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_channel_seq
        ON content_links(source_id, target_id) WHERE link_type = 'channel_seq';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_bridge_delivery
        ON content_links(source_id, actor_id) WHERE link_type = 'bridge_delivery';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_deleted
        ON content_links(source_id, actor_id) WHERE link_type = 'deleted';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_recurrence_exception
        ON content_links(source_id, target_id) WHERE link_type = 'recurrence_exception';
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_quote
        ON content_links(source_id, target_id) WHERE link_type = 'quote';
";

/// Computed metadata for feed ranking. Only content that participates in
/// feeds needs a row here (social posts, social events — not DMs or
/// private calendar events).
pub const SCHEMA_CONTENT_META: &str = "
    CREATE TABLE IF NOT EXISTS content_meta (
        content_id  BLOB PRIMARY KEY,
        score       REAL NOT NULL DEFAULT 0,
        has_media   INTEGER NOT NULL DEFAULT 0,
        is_reply    INTEGER NOT NULL DEFAULT 0,
        quote_count INTEGER NOT NULL DEFAULT 0,
        gated_tier  TEXT,
        gated_room  BLOB,
        quarantined INTEGER NOT NULL DEFAULT 0,
        suppressed INTEGER NOT NULL DEFAULT 0,
        legal_takedown_ref TEXT,
        like_count INTEGER NOT NULL DEFAULT 0,
        reply_count INTEGER NOT NULL DEFAULT 0,
        repost_count INTEGER NOT NULL DEFAULT 0,
        preview    TEXT
    );
";
// `preview` (nullable, additive 2026-09-29): the list-card text — the first
// 500 characters of `Post::body_text()`, exactly the string a native post's
// FTS row indexes (`ui/feed.md` § The read model → *The list-card preview*).
// Written by `write_post_index`, the one funnel every feed-visible post passes
// (native, all three bridges, the segment restore), and read by the three feed
// queries in place of a `content_fts` lookup — which a bridged post, whose
// corpus row is keyed by its natural id, could never satisfy. Derived: a row
// written before the column is filled once from its FTS row by
// `migrations::backfill_content_meta_preview`. NULL = no text to show (a
// payload-less discovery stub, a takedown tombstone).
// `gated_tier` (nullable, additive 2026-07-13): the tier name of a
// gated-to-tier post (`Post.gated.tier` — a plaintext-floor attribute,
// `ui/feed.md` § Encryption at rest), projected at index time so `query_feed`
// serves the wire `FeedPostItem.gated_tier` → the apps' `gated-post-badge`
// without reading the body. NULL = public post.
// `gated_room` (nullable, additive 2026-09-13): the 32-byte channel id of the
// room a **room-restricted** post addresses (`KeyAccess::Room.group_id`, the
// same plaintext floor — `ui/feed.md` § Encryption at rest → *Plaintext floor
// for a stored post*), projected beside the tier for the same reason: a
// member's card names the room, and the list must know which room without a
// per-post body decode. NULL = not a room post.
// NB: the sibling interaction counters `like_count` / `reply_count` /
// `repost_count` were added pre-policy via the explicit ALTER
// migration in `migrations.rs` (the s5 `content_meta` count-column block).
// `quote_count` follows the current canonical pattern — declared here in the
// `CREATE TABLE` block so `reconcile_added_columns` ALTERs it onto existing
// `/data` automatically (additive, constant-default; nest/common.md § Database,
// "the CREATE TABLE blocks stay the single source of truth"). No hand ALTER.

/// Unified full-text search across all content types.
/// Replaces both search_index/search_index_map and inbox_fts.
pub const SCHEMA_CONTENT_FTS: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
        title, body, author_name, tags,
        schema UNINDEXED,
        tokenize='porter unicode61'
    );

    CREATE TABLE IF NOT EXISTS content_fts_map (
        content_id BLOB PRIMARY KEY,
        fts_rowid  INTEGER NOT NULL,
        -- Indexing time epoch MICROSECONDS — the same unit as
        -- `content.created_at` -- mandatory rather than tidy: the search
        -- window compares `COALESCE(c.created_at / m.created_at)` (`db/fts.rs`)
        -- so the two columns must share a unit or the COALESCE silently mixes
        -- scales. It carries two jobs: the
        -- recency order the per-bridge newest-N cap evicts on
        -- (content-index.md § Bridge content in the Search corpus) -- and a
        -- fallback `created_at` for
        -- rows with no `content` row to join (bridge-corpus rows are keyed by
        -- blake3(content_type:natural_id) not by a content id) so the Search
        -- page's before/after window and ordering stay meaningful for them.
        -- `0` reads as unknown-and-therefore-oldest.
        created_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_content_fts_map_created
        ON content_fts_map(created_at DESC);
";

/// The two per-bridge Search-corpus choices, one row per (actor, bridge) —
/// ONE shared table rather than a column on each provider's own table, because
/// the controls are registry-supplied and uniform across every content bridge
/// (`../../../../docs/goal/behavior/content-index.md` § Bridge content in the
/// Search corpus). An absent row means the actor is on the defaults; the
/// multi-actor union rule lives in `db/bridge_search.rs`.
pub const SCHEMA_BRIDGE_SEARCH_POLICY: &str = "
    CREATE TABLE IF NOT EXISTS bridge_search_policy (
        actor_id       TEXT NOT NULL,
        bridge_id      TEXT NOT NULL,
        show_in_search INTEGER NOT NULL,
        post_limit     INTEGER NOT NULL,
        PRIMARY KEY (actor_id, bridge_id)
    );
";

/// Natural-id ↔ `content_fts` key linkage for bridge-corpus rows.
///
/// A bridge row's FTS key is `blake3(content_type:natural_id)`, which SQL
/// cannot compute — so a trigger on the bridge's own store table could never
/// find the indexed row to delete. This table is what makes the removal arms
/// STRUCTURAL rather than a hook every future deletion path must remember to
/// call (the same discipline `nostr_event_fts` gets from its two triggers:
/// "the store is the indexer, by construction"). Written at index time,
/// deleted by the same trigger that deletes the FTS row.
///
/// `post_id` is the resting post's `content.id` when the transit point rests
/// one (the AP inbox, the nostr sweep, Bluesky feed ingest) — the link the
/// feed's text filters read a bridged post's text through (`feed.md` § The
/// read model → *The list-card preview* → Corollary; `db/feeds.rs::
/// body_match_ids`). NULL for the nostr relay-store plane, which indexes
/// events that hold no post row. No index: the filters reach it by
/// `content_id`, never by `post_id`.
pub const SCHEMA_BRIDGE_INDEX_MAP: &str = "
    CREATE TABLE IF NOT EXISTS bridge_index_map (
        content_type TEXT NOT NULL,
        natural_id   TEXT NOT NULL,
        content_id   BLOB NOT NULL,
        post_id      BLOB,
        PRIMARY KEY (content_type, natural_id)
    );
    CREATE INDEX IF NOT EXISTS idx_bridge_index_map_content
        ON bridge_index_map(content_id);
";

/// The face of a synthetic (bridged) author — one row per synthetic
/// `ActorId`, written by each content bridge at its own transit point and read
/// by the feed handlers after every local page query (`bridges.md` § Unified
/// feed ingestion → *Bridged authors*, ruled 2026-09-26; module
/// `db/bridge_authors.rs`).
///
/// **Derived, recreatable, not user data**: every row is re-derivable from the
/// bridge's next transit (an ActivityPub actor re-fetch or `Update{Person}`, a
/// followed nostr author's replaceable kind 0 re-arriving, the next Bluesky
/// poll's `author` view), so dropping it loses nothing a user cannot get back
/// (`principles.md` § No user-data loss). `updated_at` is epoch micros; the
/// upsert keeps the newest (`bridge_authors::upsert`).
pub const SCHEMA_BRIDGE_AUTHORS: &str = "
    CREATE TABLE IF NOT EXISTS bridge_authors (
        actor_id     BLOB PRIMARY KEY,
        bridge       TEXT NOT NULL,
        external_id  TEXT NOT NULL,
        handle       TEXT,
        display_name TEXT,
        avatar_url   TEXT,
        updated_at   INTEGER NOT NULL
    );
";

/// Nest pairing records — which private nests are paired with this public nest.
pub const SCHEMA_NEST_PAIRINGS: &str = "
    CREATE TABLE IF NOT EXISTS nest_pairings (
        actor_id        BLOB NOT NULL,
        private_nest_id BLOB NOT NULL,
        capabilities    TEXT NOT NULL,
        expires_at      INTEGER,
        created_at      INTEGER NOT NULL,
        nest_url TEXT,
        label TEXT,
        PRIMARY KEY (actor_id, private_nest_id)
    );
";

/// Outbox queue for forwarding posts from private to public nest.
///
/// `author_id` is the queued post's or tombstone's author, stamped by the
/// producer — the table's only person column, and what lets an account deletion
/// find its own queued forwards without decoding every `payload`.
///
/// `last_error` is the worker's most recent failure to send this row —
/// the *why* the author's app shows beside "N posts waiting to reach your
/// relay" (`private-mode.md` § Post Forwarding → the queue is the user's to
/// see). Nullable: NULL until a send has failed; a successful send deletes the
/// row, so nothing ever clears it in place. It is stored bounded and
/// control-stripped (`db/pairing.rs::bounded_failure_reason`).
///
/// `last_attempt_at` is when that failure was recorded, in microseconds — the
/// anchor of the re-arm's throttle (`outbox_retry_now_for_author`), which makes
/// an entry due no sooner than a fixed gap after its last attempt. NULL until a
/// send has failed.
pub const SCHEMA_OUTBOX: &str = "
    CREATE TABLE IF NOT EXISTS outbox (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        payload         BLOB NOT NULL,
        entry_type      TEXT NOT NULL,
        created_at      INTEGER NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_retry      INTEGER NOT NULL DEFAULT 0,
        author_id       BLOB NOT NULL,
        last_error      TEXT,
        last_attempt_at INTEGER
    );
";

/// Encrypted namespace entries for paired nest sync.
pub const SCHEMA_NAMESPACE_ENTRIES: &str = "
    CREATE TABLE IF NOT EXISTS namespace_entries (
        namespace   BLOB NOT NULL,
        entry_id    BLOB NOT NULL,
        seq         INTEGER NOT NULL,
        ciphertext  BLOB NOT NULL,
        actor_sig   BLOB NOT NULL,
        source      TEXT NOT NULL DEFAULT 'local',
        updated_at  INTEGER NOT NULL,
        PRIMARY KEY (namespace, entry_id, source)
    );
    CREATE INDEX IF NOT EXISTS idx_namespace_seq ON namespace_entries(namespace, seq);
";

/// Delivery receipts for video CDN tracking — records when a nest serves
/// content to another nest.
pub const SCHEMA_DELIVERY_RECEIPTS: &str = "
    CREATE TABLE IF NOT EXISTS delivery_receipts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        content_hash    BLOB NOT NULL,
        server_nest     BLOB(32) NOT NULL,
        requesting_nest BLOB(32) NOT NULL,
        bytes_served    INTEGER NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_delivery_receipts_content
        ON delivery_receipts(content_hash);
";

/// Cache sources for video segments — records which remote nests have
/// served a given content hash, enabling cache-on-fetch lookups.
pub const SCHEMA_VIDEO_CACHE_SOURCES: &str = "
    CREATE TABLE IF NOT EXISTS video_cache_sources (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        content_hash    BLOB NOT NULL,
        nest_url        TEXT NOT NULL,
        bytes_served    INTEGER NOT NULL DEFAULT 0,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_video_cache_sources_hash
        ON video_cache_sources(content_hash);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_video_cache_sources_unique
        ON video_cache_sources(content_hash, nest_url);
";

/// Engagement events — likes, reposts, replies, views, etc.
pub const SCHEMA_ENGAGEMENT_EVENTS: &str = "
    CREATE TABLE IF NOT EXISTS engagement_events (
        event_id        BLOB PRIMARY KEY,
        content_id      BLOB NOT NULL,
        actor_id        BLOB,
        event_type      TEXT NOT NULL,
        event_data      BLOB,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_engagement_content
        ON engagement_events(content_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_engagement_actor
        ON engagement_events(actor_id, created_at DESC);
";

/// Web hosting file store — uploaded source files per actor.
///
/// `blob_hash` is the sync change's **manifest hash**, not the file body: the
/// serve path walks manifest → chunks → blob store (web-content-hosting.md
/// § Content model).
pub const SCHEMA_WEB_FILES: &str = "
    CREATE TABLE IF NOT EXISTS web_files (
        actor_id        BLOB NOT NULL,
        path            TEXT NOT NULL,
        blob_hash       BLOB NOT NULL,
        content_type    TEXT NOT NULL,
        updated_at      INTEGER NOT NULL,
        -- The `web`-mode folder this file was synced from (FK `folders.id`).
        -- Serve-side this resolves the set NAME (the `content.read{folder:set}`
        -- grant-scope qualifier) and its `web_paywall_tier` (the fail-closed
        -- gate). NULL only for rows written before this column existed.
        -- Additive nullable, declared here so reconcile_added_columns ALTERs it
        -- onto existing DBs (NO hand ALTER; nest/common.md § Database).
        folder_id     INTEGER,
        -- The M2 content-key generation this file's chunks are sealed under
        -- (monetization.md § Pillar 2, the folder half). NULL = plaintext
        -- chunks (an unpaywalled web set — the default). Non-NULL = sealed: the
        -- row serves ONLY through the paywall token gate, and never as
        -- ciphertext. Transcribed from the sync change's `content_key_version`.
        -- Additive nullable (same reconcile rule as above).
        content_key_version INTEGER,
        PRIMARY KEY (actor_id, path)
    );
";

/// Web hosting rendered output — post-processed files per actor.
pub const SCHEMA_WEB_RENDERED: &str = "
    CREATE TABLE IF NOT EXISTS web_rendered (
        actor_id        BLOB NOT NULL,
        path            TEXT NOT NULL,
        blob_hash       BLOB NOT NULL,
        content_type    TEXT NOT NULL,
        updated_at      INTEGER NOT NULL,
        PRIMARY KEY (actor_id, path)
    );
";

/// Web hosting custom domain registrations.
pub const SCHEMA_WEB_DOMAINS: &str = "
    CREATE TABLE IF NOT EXISTS web_domains (
        actor_id        BLOB NOT NULL,
        domain          TEXT NOT NULL UNIQUE,
        verify_token    TEXT NOT NULL,
        status          TEXT NOT NULL DEFAULT 'pending',
        created_at      INTEGER NOT NULL,
        verified_at     INTEGER,
        PRIMARY KEY (actor_id, domain)
    );
";

/// Uniqueness constraint ensuring each actor has at most one 'web_published' link.
pub const SCHEMA_WEB_PUBLISHED_UNIQUE: &str = "
    CREATE UNIQUE INDEX IF NOT EXISTS idx_links_unique_web_published
        ON content_links(actor_id, status) WHERE link_type = 'web_published';
";

/// Per-nest engagement statistics and trust scoring.
pub const SCHEMA_NEST_TRUST: &str = "

    CREATE TABLE IF NOT EXISTS nest_engagement_stats (
        nest_id         BLOB NOT NULL,
        event_type      TEXT NOT NULL,
        event_count     INTEGER NOT NULL DEFAULT 0,
        last_seen       INTEGER NOT NULL,
        PRIMARY KEY (nest_id, event_type)
    );
    CREATE INDEX IF NOT EXISTS idx_nest_stats_nest ON nest_engagement_stats(nest_id);

    CREATE TABLE IF NOT EXISTS nest_trust (
        nest_id         BLOB PRIMARY KEY,
        trust_score     REAL NOT NULL DEFAULT 1.0,
        reason          TEXT,
        updated_at      INTEGER NOT NULL
    );
";

/// All unified schemas in order. Called once at startup.
pub fn apply_unified_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(SCHEMA_CONTENT)?;
    conn.execute_batch(SCHEMA_CONTENT_LINKS)?;
    conn.execute_batch(SCHEMA_CONTENT_LINKS_UNIQUE)?;
    conn.execute_batch(SCHEMA_CONTENT_META)?;
    conn.execute_batch(SCHEMA_CONTENT_FTS)?;
    conn.execute_batch(SCHEMA_BRIDGE_SEARCH_POLICY)?;
    conn.execute_batch(SCHEMA_BRIDGE_INDEX_MAP)?;
    conn.execute_batch(SCHEMA_BRIDGE_AUTHORS)?;
    conn.execute_batch(SCHEMA_NEST_PAIRINGS)?;
    conn.execute_batch(SCHEMA_OUTBOX)?;
    conn.execute_batch(SCHEMA_NAMESPACE_ENTRIES)?;
    conn.execute_batch(SCHEMA_DELIVERY_RECEIPTS)?;
    conn.execute_batch(SCHEMA_VIDEO_CACHE_SOURCES)?;
    conn.execute_batch(SCHEMA_ENGAGEMENT_EVENTS)?;
    conn.execute_batch(SCHEMA_NEST_TRUST)?;
    conn.execute_batch(SCHEMA_WEB_FILES)?;
    conn.execute_batch(SCHEMA_WEB_RENDERED)?;
    conn.execute_batch(SCHEMA_WEB_DOMAINS)?;
    conn.execute_batch(SCHEMA_WEB_PUBLISHED_UNIQUE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn unified_schema_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        apply_unified_schema(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"content".to_string()));
        assert!(tables.contains(&"content_links".to_string()));
        assert!(tables.contains(&"content_meta".to_string()));
    }

    #[test]
    fn unified_schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_unified_schema(&conn).unwrap();
        apply_unified_schema(&conn).unwrap();
    }

    #[test]
    fn unified_schema_creates_web_tables() {
        let conn = Connection::open_in_memory().unwrap();
        apply_unified_schema(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"web_files".to_string()));
        assert!(tables.contains(&"web_rendered".to_string()));
        assert!(tables.contains(&"web_domains".to_string()));
    }
}
