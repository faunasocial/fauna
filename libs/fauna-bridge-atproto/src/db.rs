//! Database schema and queries for the Bluesky bridge.
//!
//! Defines table creation SQL and query functions. Connection management
//! is handled by the node — this module operates on a provided connection.

/// SQL statements to create Bluesky bridge tables — the bridge's genesis at
/// the current shape. The nest applies it with its additive column reconciler
/// (`fauna_nest::bridge_schema::apply_genesis`), so a new nullable or
/// defaulted column is added here and nowhere else.
pub const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS bluesky_accounts (
    actor_id        TEXT PRIMARY KEY,
    bluesky_did     TEXT NOT NULL,
    bluesky_handle  TEXT NOT NULL,
    access_token    BLOB NOT NULL,
    refresh_token   BLOB NOT NULL,
    dpop_key        BLOB NOT NULL,
    token_expires   INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    write_through   INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS bluesky_posts (
    fauna_post_id TEXT PRIMARY KEY,
    at_uri          TEXT NOT NULL UNIQUE,
    author_did      TEXT NOT NULL,
    content_json    BLOB NOT NULL,
    interacted      INTEGER DEFAULT 0,
    cached_at       INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    cid             TEXT
);

CREATE TABLE IF NOT EXISTS bluesky_interactions (
    actor_id        TEXT NOT NULL,
    fauna_post_id TEXT NOT NULL,
    interaction_type TEXT NOT NULL,
    record_uri      TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (actor_id, fauna_post_id, interaction_type)
);

CREATE TABLE IF NOT EXISTS bluesky_oauth_states (
    key         TEXT PRIMARY KEY,
    data        BLOB NOT NULL,
    expires_at  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS bluesky_sessions (
    key         TEXT PRIMARY KEY,
    data        BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS bluesky_saved_feeds (
    actor_id     TEXT NOT NULL,
    feed_uri     TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description  TEXT,
    avatar       TEXT,
    saved_at     INTEGER NOT NULL,
    PRIMARY KEY (actor_id, feed_uri)
);
"#;
