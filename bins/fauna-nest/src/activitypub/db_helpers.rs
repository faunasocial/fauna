//! Nest-side database operations for ActivityPub bridge state.
//!
//! All functions take a `&rusqlite::Connection` (obtained from `CacheDb::conn()`)
//! and follow the same synchronous pattern as `nostr::db`.

use anyhow::Result;
use rusqlite::Connection;

fn now_secs() -> i64 {
    crate::db::now_epoch_secs()
}

// ── Row types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ApAccount {
    pub actor_id: String,
    pub username: String,
    pub actor_url: String,
    pub encrypted_privkey: Vec<u8>,
    pub public_key_pem: String,
    pub enabled: bool,
    pub backfill: bool,
    pub auto_accept_follows: bool,
    pub default_visibility: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Settings that can be updated via the bridge management API.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ApSettings {
    pub enabled: Option<bool>,
    pub backfill: Option<bool>,
    pub auto_accept_follows: Option<bool>,
    pub default_visibility: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteActor {
    pub uri: String,
    pub inbox: String,
    pub shared_inbox: Option<String>,
    pub public_key_pem: String,
    pub preferred_username: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub banner_url: Option<String>,
    pub summary: Option<String>,
    pub last_fetched: i64,
}

#[derive(Debug, Clone)]
pub struct ApFollowRecord {
    pub id: i64,
    pub local_actor_id: String,
    pub remote_actor_uri: String,
    pub direction: String,
    pub state: String,
    pub follow_activity_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct DeliveryJob {
    pub id: i64,
    pub activity_json: String,
    pub target_inbox: String,
    pub created_at: i64,
    pub attempts: i64,
    pub next_retry_at: i64,
    pub status: String,
}

// ── Account CRUD ────────────────────────────────────────────────

pub fn get_account(conn: &Connection, actor_id: &str) -> Result<Option<ApAccount>> {
    let mut stmt = conn.prepare(
        "SELECT actor_id, username, actor_url, encrypted_privkey, public_key_pem,
                enabled, backfill, auto_accept_follows, default_visibility,
                created_at, updated_at
         FROM ap_accounts WHERE actor_id = ?1",
    )?;
    let row = stmt.query_row([actor_id], row_to_account);
    match row {
        Ok(acct) => Ok(Some(acct)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_account_by_username(conn: &Connection, username: &str) -> Result<Option<ApAccount>> {
    let mut stmt = conn.prepare(
        "SELECT actor_id, username, actor_url, encrypted_privkey, public_key_pem,
                enabled, backfill, auto_accept_follows, default_visibility,
                created_at, updated_at
         FROM ap_accounts WHERE username = ?1",
    )?;
    let row = stmt.query_row([username], row_to_account);
    match row {
        Ok(acct) => Ok(Some(acct)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count of accounts federating right now — the `usage.users.total` a NodeInfo
/// document reports.
///
/// Deliberately **not** the nest's total user count: AP federation is per-actor
/// opt-in (`ap_accounts.enabled`), so a nest's non-federating users are not
/// users of this fediverse server and reporting them would leak nest population
/// to every crawler that reads NodeInfo. A shipped-but-unenabled nest reports
/// `0`, matching the rest of the dark-ship surface (WebFinger 404s the same
/// accounts).
pub fn count_enabled_accounts(conn: &Connection) -> Result<u64> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ap_accounts WHERE enabled = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(total.max(0) as u64)
}

pub fn create_account(
    conn: &Connection,
    actor_id: &str,
    username: &str,
    actor_url: &str,
    encrypted_privkey: &[u8],
    public_key_pem: &str,
) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT OR REPLACE INTO ap_accounts
         (actor_id, username, actor_url, encrypted_privkey, public_key_pem, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![
            actor_id,
            username,
            actor_url,
            encrypted_privkey,
            public_key_pem,
            now
        ],
    )?;
    Ok(())
}

pub fn delete_account(conn: &Connection, actor_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ap_follows WHERE local_actor_id = ?1",
        [actor_id],
    )?;
    conn.execute("DELETE FROM ap_accounts WHERE actor_id = ?1", [actor_id])?;
    Ok(())
}

pub fn update_settings(conn: &Connection, actor_id: &str, settings: &ApSettings) -> Result<()> {
    let now = now_secs();
    if let Some(v) = settings.enabled {
        conn.execute(
            "UPDATE ap_accounts SET enabled = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.backfill {
        conn.execute(
            "UPDATE ap_accounts SET backfill = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.auto_accept_follows {
        conn.execute(
            "UPDATE ap_accounts SET auto_accept_follows = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(ref v) = settings.default_visibility {
        conn.execute(
            "UPDATE ap_accounts SET default_visibility = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v, now, actor_id],
        )?;
    }
    Ok(())
}

// ── Instance actor (nest-level signing identity) ─────────────────

/// Read the nest's instance-actor keypair as `(encrypted_privkey, public_pem)`,
/// or `None` if it has not been minted yet. Custody + minting live in
/// `activitypub::instance_actor`; this is only the storage half.
pub fn get_instance_actor(conn: &Connection) -> Result<Option<(Vec<u8>, String)>> {
    let mut stmt = conn
        .prepare("SELECT encrypted_privkey, public_key_pem FROM ap_instance_actor WHERE id = 1")?;
    match stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Store the instance-actor keypair unless one already exists.
///
/// `INSERT OR IGNORE` against the single-row primary key is what makes minting
/// idempotent without a lock: two racing callers each generate a keypair, the
/// second insert is a silent no-op, and both then read back the one key that
/// won. A caller MUST read back rather than trust what it generated — the
/// published document and the signing key have to be the same pair.
pub fn insert_instance_actor_if_absent(
    conn: &Connection,
    encrypted_privkey: &[u8],
    public_key_pem: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO ap_instance_actor
             (id, encrypted_privkey, public_key_pem, created_at)
         VALUES (1, ?1, ?2, ?3)",
        rusqlite::params![encrypted_privkey, public_key_pem, now_secs()],
    )?;
    Ok(())
}

// ── Remote actor cache ──────────────────────────────────────────

pub fn get_remote_actor(conn: &Connection, uri: &str) -> Result<Option<RemoteActor>> {
    let mut stmt = conn.prepare(
        "SELECT uri, inbox, shared_inbox, public_key_pem, preferred_username,
                display_name, avatar_url, banner_url, summary, last_fetched
         FROM ap_remote_actors WHERE uri = ?1",
    )?;
    let row = stmt.query_row([uri], row_to_remote_actor);
    match row {
        Ok(a) => Ok(Some(a)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn upsert_remote_actor(conn: &Connection, actor: &RemoteActor) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO ap_remote_actors
         (uri, inbox, shared_inbox, public_key_pem, preferred_username,
          display_name, avatar_url, banner_url, summary, last_fetched)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            actor.uri,
            actor.inbox,
            actor.shared_inbox,
            actor.public_key_pem,
            actor.preferred_username,
            actor.display_name,
            actor.avatar_url,
            actor.banner_url,
            actor.summary,
            actor.last_fetched,
        ],
    )?;
    // The bridged-author transit point (`bridges.md` § Unified feed ingestion
    // → *Bridged authors*): every cache write — the inbox's fetch-and-cache,
    // `Update{Person}`, the push and interact resolves — refreshes the face
    // the feed serves for this actor's synthetic id. One site, every caller.
    crate::db::bridge_authors::upsert(conn, &bridge_author_of(actor))?;
    Ok(())
}

/// The `bridge_authors` row a cached remote actor projects: the synthetic id
/// over the actor URI, `@preferredUsername@host` as the handle (the spelling
/// the `Create`-push's `Mention` already names), `name` as the display name,
/// the `icon` URL behind the shared media proxy. Stamped with the write
/// instant — an actor document carries no timestamp of its own.
pub fn bridge_author_of(actor: &RemoteActor) -> crate::db::bridge_authors::BridgeAuthor {
    use fauna_bridge_activitypub::identity::synthetic_actor_id;
    let host = url::Url::parse(&actor.uri)
        .ok()
        .and_then(|u| u.host_str().map(String::from));
    let handle = match (actor.preferred_username.as_deref(), host) {
        (Some(user), Some(host)) if !user.trim().is_empty() => {
            Some(format!("@{}@{host}", user.trim()))
        }
        _ => None,
    };
    crate::db::bridge_authors::BridgeAuthor {
        actor_id: synthetic_actor_id(&actor.uri).0,
        bridge: crate::db::bridge_authors::BRIDGE_ACTIVITYPUB.into(),
        external_id: actor.uri.clone(),
        handle,
        display_name: actor.display_name.clone(),
        avatar_url: actor
            .avatar_url
            .as_deref()
            .and_then(fauna_core::data::shared_media_proxy_url),
        updated_at: crate::db::now_epoch_micros(),
    }
}

// ── Follow CRUD ─────────────────────────────────────────────────

pub fn create_follow(
    conn: &Connection,
    local_actor_id: &str,
    remote_actor_uri: &str,
    direction: &str,
    follow_activity_id: Option<&str>,
) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT OR IGNORE INTO ap_follows
         (local_actor_id, remote_actor_uri, direction, state, follow_activity_id, created_at)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
        rusqlite::params![
            local_actor_id,
            remote_actor_uri,
            direction,
            follow_activity_id,
            now
        ],
    )?;
    Ok(())
}

pub fn accept_follow(
    conn: &Connection,
    local_actor_id: &str,
    remote_actor_uri: &str,
    direction: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE ap_follows SET state = 'accepted'
         WHERE local_actor_id = ?1 AND remote_actor_uri = ?2 AND direction = ?3",
        rusqlite::params![local_actor_id, remote_actor_uri, direction],
    )?;
    Ok(())
}

pub fn delete_follow(
    conn: &Connection,
    local_actor_id: &str,
    remote_actor_uri: &str,
    direction: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM ap_follows
         WHERE local_actor_id = ?1 AND remote_actor_uri = ?2 AND direction = ?3",
        rusqlite::params![local_actor_id, remote_actor_uri, direction],
    )?;
    Ok(())
}

/// List remote actors that follow the local actor (inbound follows).
pub fn list_followers(conn: &Connection, local_actor_id: &str) -> Result<Vec<ApFollowRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, local_actor_id, remote_actor_uri, direction, state, follow_activity_id, created_at
         FROM ap_follows WHERE local_actor_id = ?1 AND direction = 'inbound'
         ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([local_actor_id], row_to_follow)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The follow requests waiting on `local_actor_id`: its inbound rows still
/// `pending`, newest first. A request IS that row — it holds the requester and
/// the `Follow` activity id the answer must name (`activitypub.md` § Follow
/// requests) — so there is no second table to read.
pub fn list_pending_inbound_follows(
    conn: &Connection,
    local_actor_id: &str,
) -> Result<Vec<ApFollowRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, local_actor_id, remote_actor_uri, direction, state, follow_activity_id, created_at
         FROM ap_follows
         WHERE local_actor_id = ?1 AND direction = 'inbound' AND state = 'pending'
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([local_actor_id], row_to_follow)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The one waiting request `remote_actor_uri` has with `local_actor_id`, or
/// `None` when there is none — never asked, withdrawn, or already answered.
pub fn get_pending_inbound_follow(
    conn: &Connection,
    local_actor_id: &str,
    remote_actor_uri: &str,
) -> Result<Option<ApFollowRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, local_actor_id, remote_actor_uri, direction, state, follow_activity_id, created_at
         FROM ap_follows
         WHERE local_actor_id = ?1 AND remote_actor_uri = ?2
           AND direction = 'inbound' AND state = 'pending'",
    )?;
    match stmt.query_row([local_actor_id, remote_actor_uri], row_to_follow) {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// True when any **enabled** local AP account follows `remote_actor_uri` — an
/// outbound `ap_follows` row in any state (the local user's Follow request is
/// itself the opt-in act, so `pending` counts; the remote's Accept only
/// confirms it). The inbound-Create relationship gate keys on this.
pub fn any_enabled_account_follows(conn: &Connection, remote_actor_uri: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM ap_follows f
            JOIN ap_accounts a ON a.actor_id = f.local_actor_id
            WHERE f.remote_actor_uri = ?1
              AND f.direction = 'outbound'
              AND a.enabled = 1
         )",
        [remote_actor_uri],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// True when the mapped object at `ap_url` is an **enabled** local AP
/// account's own post — the ownership arm of the inbound reaction gate:
/// engagement on a local account's own federated posts is what enabling
/// federation subscribes it to (Mastodon-parity — anyone may favorite/boost
/// a public post), while a reaction on an ingested remote object needs the
/// follow arm. Local map rows carry the account's `actor_id` (the
/// Create-push writes them); ingested rows carry a synthetic actor hex that
/// joins to no account.
pub fn enabled_account_owns_ap_url(conn: &Connection, ap_url: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM ap_post_map m
            JOIN ap_accounts a ON a.actor_id = m.actor_id
            WHERE m.ap_url = ?1
              AND m.tombstoned = 0
              AND a.enabled = 1
         )",
        [ap_url],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// True when `ap_url` maps to a **local account's own** post — any local
/// account, enabled or not, and tombstoned rows included.
///
/// The guard for the one AP path that *destroys* a content projection (inbound
/// `Delete`). Deliberately NOT [`enabled_account_owns_ap_url`], whose two extra
/// clauses are exactly wrong here: `enabled = 1` would stop protecting a user's
/// posts the moment they switched federation off, and `tombstoned = 0` would
/// stop protecting a post on the retry of a delete that already tombstoned its
/// map row. That gate answers "may this reaction be minted"; this one answers
/// "is this ours to destroy", and a destructive verb fails closed.
///
/// The distinction is structural, per [`enabled_account_owns_ap_url`]'s note:
/// local map rows carry the account's `actor_id` (the Create-push writes them),
/// ingested rows carry a synthetic actor hex that joins to no account.
pub fn is_local_account_post(conn: &Connection, ap_url: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM ap_post_map m
            JOIN ap_accounts a ON a.actor_id = m.actor_id
            WHERE m.ap_url = ?1
         )",
        [ap_url],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// List remote actors that the local actor follows (outbound follows).
pub fn list_outbound_follows(
    conn: &Connection,
    local_actor_id: &str,
) -> Result<Vec<ApFollowRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, local_actor_id, remote_actor_uri, direction, state, follow_activity_id, created_at
         FROM ap_follows WHERE local_actor_id = ?1 AND direction = 'outbound'
         ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([local_actor_id], row_to_follow)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── Post map ────────────────────────────────────────────────────

/// `remote_actor_uri` is the AP actor the object belongs to (the Note's
/// `attributedTo` / the activity's `actor`) for inbound rows — it lets the
/// outbound interact path resolve the actor's real inbox from
/// `ap_remote_actors` instead of URL-guessing. `None` for local-post rows
/// (the Create-push).
pub fn insert_post_map(
    conn: &Connection,
    fauna_post_id: &str,
    ap_url: &str,
    actor_id: &str,
    remote_actor_uri: Option<&str>,
) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT OR IGNORE INTO ap_post_map
         (fauna_post_id, ap_url, actor_id, created_at, remote_actor_uri)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![fauna_post_id, ap_url, actor_id, now, remote_actor_uri],
    )?;
    Ok(())
}

pub fn get_ap_url_for_post(conn: &Connection, fauna_post_id: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT ap_url FROM ap_post_map WHERE fauna_post_id = ?1 AND tombstoned = 0")?;
    let row = stmt.query_row([fauna_post_id], |row| row.get::<_, String>(0));
    match row {
        Ok(url) => Ok(Some(url)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The interact path's target lookup: the AP object URL plus, when the
/// inbound ingest recorded it, the owning remote actor's URI (for real
/// inbox resolution via `ap_remote_actors`; `None` on local rows).
pub fn get_ap_target_for_post(
    conn: &Connection,
    fauna_post_id: &str,
) -> Result<Option<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT ap_url, remote_actor_uri FROM ap_post_map
         WHERE fauna_post_id = ?1 AND tombstoned = 0",
    )?;
    let row = stmt.query_row([fauna_post_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    });
    match row {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A live `ap_post_map` row as a reply/quote target: the object URL, the
/// remote owner's URI (inbound rows) and the local actor the row belongs to
/// (a `Create`-push row's author — the self-thread case, whose
/// `remote_actor_uri` is `None`).
pub struct ApReferenceTarget {
    pub ap_url: String,
    pub remote_actor_uri: Option<String>,
    pub actor_id: String,
}

pub fn get_ap_reference_target(
    conn: &Connection,
    fauna_post_id: &str,
) -> Result<Option<ApReferenceTarget>> {
    let mut stmt = conn.prepare(
        "SELECT ap_url, remote_actor_uri, actor_id FROM ap_post_map
         WHERE fauna_post_id = ?1 AND tombstoned = 0",
    )?;
    let row = stmt.query_row([fauna_post_id], |row| {
        Ok(ApReferenceTarget {
            ap_url: row.get(0)?,
            remote_actor_uri: row.get(1)?,
            actor_id: row.get(2)?,
        })
    });
    match row {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_post_id_for_ap_url(conn: &Connection, ap_url: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT fauna_post_id FROM ap_post_map WHERE ap_url = ?1 AND tombstoned = 0")?;
    let row = stmt.query_row([ap_url], |row| row.get::<_, String>(0));
    match row {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The local note URL a `Create`-push recorded for this post + author —
/// tombstoned rows INCLUDED, deliberately: this is the Delete leg's
/// pushed-witness lookup, and a delete retry (`AlreadyGone`) must still find
/// the URL of a Delete a first attempt failed to deliver.
pub fn get_local_note_url(
    conn: &Connection,
    fauna_post_id: &str,
    actor_id: &str,
) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT ap_url FROM ap_post_map WHERE fauna_post_id = ?1 AND actor_id = ?2")?;
    let row = stmt.query_row([fauna_post_id, actor_id], |row| row.get::<_, String>(0));
    match row {
        Ok(url) => Ok(Some(url)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn tombstone_post_map(conn: &Connection, fauna_post_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE ap_post_map SET tombstoned = 1 WHERE fauna_post_id = ?1",
        [fauna_post_id],
    )?;
    Ok(())
}

// ── Delivery queue ──────────────────────────────────────────────

pub fn enqueue_delivery(conn: &Connection, activity_json: &str, target_inbox: &str) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT INTO ap_delivery_queue
         (activity_json, target_inbox, created_at, next_retry_at)
         VALUES (?1, ?2, ?3, ?3)",
        rusqlite::params![activity_json, target_inbox, now],
    )?;
    Ok(())
}

pub fn get_pending_deliveries(conn: &Connection, limit: u32) -> Result<Vec<DeliveryJob>> {
    let now = now_secs();
    let mut stmt = conn.prepare(
        "SELECT id, activity_json, target_inbox, created_at, attempts, next_retry_at, status
         FROM ap_delivery_queue
         WHERE status = 'pending' AND next_retry_at <= ?1
         ORDER BY next_retry_at ASC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![now, limit], row_to_delivery_job)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_delivery_done(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE ap_delivery_queue SET status = 'done' WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

pub fn mark_delivery_retry(conn: &Connection, id: i64) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "UPDATE ap_delivery_queue
         SET attempts = attempts + 1,
             next_retry_at = ?1 + (60 * (1 << MIN(attempts, 10)))
         WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

pub fn mark_delivery_failed(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE ap_delivery_queue SET status = 'failed' WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

// ── Dead inbox tracking ─────────────────────────────────────────

/// Record a delivery failure for an inbox. Creates or updates the dead_inboxes entry.
pub fn record_inbox_failure(conn: &Connection, inbox_url: &str) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT INTO ap_dead_inboxes (inbox_url, first_failure_at, last_failure_at, failure_count)
         VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(inbox_url) DO UPDATE SET
             last_failure_at = ?3,
             failure_count = failure_count + 1",
        rusqlite::params![inbox_url, now, now],
    )?;
    Ok(())
}

/// Record a successful delivery — clears the dead inbox record.
pub fn record_inbox_success(conn: &Connection, inbox_url: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ap_dead_inboxes WHERE inbox_url = ?1",
        [inbox_url],
    )?;
    Ok(())
}

/// Check if an inbox is considered dead (failing for > 7 days).
pub fn is_inbox_dead(conn: &Connection, inbox_url: &str) -> Result<bool> {
    let now = now_secs();
    let dead_threshold = 7 * 86_400; // 7 days
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ap_dead_inboxes
         WHERE inbox_url = ?1 AND (?2 - first_failure_at) > ?3",
        rusqlite::params![inbox_url, now, dead_threshold],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Remove follows to remote actors whose inboxes have been dead for > 7 days.
/// Returns the number of stale follows removed.
pub fn cleanup_stale_follows(conn: &Connection) -> Result<usize> {
    let now = now_secs();
    let dead_threshold = 7 * 86_400;

    // Find remote actor URIs with dead inboxes by joining through ap_remote_actors.
    let removed = conn.execute(
        "DELETE FROM ap_follows WHERE direction = 'inbound' AND remote_actor_uri IN (
            SELECT ra.uri FROM ap_remote_actors ra
            INNER JOIN ap_dead_inboxes di ON (di.inbox_url = ra.inbox OR di.inbox_url = ra.shared_inbox)
            WHERE (?1 - di.first_failure_at) > ?2
        )",
        rusqlite::params![now, dead_threshold],
    )?;
    Ok(removed)
}

/// Clean up old completed/failed delivery queue entries (older than 7 days).
pub fn cleanup_old_deliveries(conn: &Connection) -> Result<usize> {
    let cutoff = now_secs() - 7 * 86_400;
    let removed = conn.execute(
        "DELETE FROM ap_delivery_queue WHERE status IN ('done', 'failed') AND created_at < ?1",
        [cutoff],
    )?;
    Ok(removed)
}

// ── Row mappers ─────────────────────────────────────────────────

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApAccount> {
    Ok(ApAccount {
        actor_id: row.get(0)?,
        username: row.get(1)?,
        actor_url: row.get(2)?,
        encrypted_privkey: row.get(3)?,
        public_key_pem: row.get(4)?,
        enabled: row.get::<_, i32>(5)? != 0,
        backfill: row.get::<_, i32>(6)? != 0,
        auto_accept_follows: row.get::<_, i32>(7)? != 0,
        default_visibility: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_remote_actor(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteActor> {
    Ok(RemoteActor {
        uri: row.get(0)?,
        inbox: row.get(1)?,
        shared_inbox: row.get(2)?,
        public_key_pem: row.get(3)?,
        preferred_username: row.get(4)?,
        display_name: row.get(5)?,
        avatar_url: row.get(6)?,
        banner_url: row.get(7)?,
        summary: row.get(8)?,
        last_fetched: row.get(9)?,
    })
}

fn row_to_follow(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApFollowRecord> {
    Ok(ApFollowRecord {
        id: row.get(0)?,
        local_actor_id: row.get(1)?,
        remote_actor_uri: row.get(2)?,
        direction: row.get(3)?,
        state: row.get(4)?,
        follow_activity_id: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn row_to_delivery_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryJob> {
    Ok(DeliveryJob {
        id: row.get(0)?,
        activity_json: row.get(1)?,
        target_inbox: row.get(2)?,
        created_at: row.get(3)?,
        attempts: row.get(4)?,
        next_retry_at: row.get(5)?,
        status: row.get(6)?,
    })
}

// ── Outbox reads ────────────────────────────────────────────────────────────
//
// The outbox is an UNAUTHENTICATED, world-readable surface, so all three
// reads below apply the shared off-box servability predicate
// (`crate::db::public_servability::PUBLIC_POST_SERVABLE`): they exclude gated
// (monetized/paywalled) posts, everything the moderation flags withhold, and
// any post with no `content_meta` row at all.
//
// A gated post lives in the public `post/%` projection like any other — its
// `schema` is derived from the body variant alone (`db::post_body_schema`) and
// never consults `Post::gated`. Exclusion is therefore read-side and
// per-consumer, keyed on `content_meta.gated_tier`: feeds, trends and signals
// each carry their own filter, nostr refuses in `public_post_from_payload`,
// and the trending plane hard-refuses. These helpers are the outbox's
// share of that contract, kept together so the filter has ONE owner rather
// than being duplicated inline at call sites that can drift apart — which is
// exactly what had happened by 2026-07-29: this filter and the ATProto
// projection's copy of it both still read `gated_tier` alone, while the feed
// read had honoured the moderation flags since 2026-07-05.
//
// (What would leak is the teaser, not the body — a gated post's `body` is the
// public preview and the full content is ciphertext at `gated.encrypted_ref`.
// But federating the teaser as an ordinary free Note strips the paywall
// context the web render carries, so the outbox refuses it outright.)

/// Number of publicly-servable posts by `author` — the outbox's `totalItems`.
///
/// Applies the identical [`crate::db::public_servability::PUBLIC_POST_SERVABLE`]
/// predicate as [`public_outbox_page`]; a divergence here would make
/// `totalItems` disagree with the page it counts.
pub fn public_outbox_count(conn: &Connection, author: &[u8]) -> Result<u64> {
    let n: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM content c
                 LEFT JOIN content_meta cm ON cm.content_id = c.id
                 WHERE c.author = ?1 AND {servable}",
                servable = crate::db::public_servability::PUBLIC_POST_SERVABLE.as_str(),
            ),
            [author],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(n as u64)
}

/// One page of publicly-servable post ids by `author`, newest first.
///
/// Servability is the shared off-box predicate
/// ([`crate::db::public_servability::PUBLIC_POST_SERVABLE`]) — the outbox
/// publishes to the fediverse, so it withholds everything the ATProto
/// projection and the feed read withhold.
pub fn public_outbox_page(
    conn: &Connection,
    author: &[u8],
    limit: i64,
    offset: i64,
) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT c.id FROM content c
         LEFT JOIN content_meta cm ON cm.content_id = c.id
         WHERE c.author = ?1 AND {servable}
         ORDER BY c.created_at DESC
         LIMIT ?2 OFFSET ?3",
        servable = crate::db::public_servability::PUBLIC_POST_SERVABLE.as_str(),
    ))?;
    let rows = stmt.query_map(rusqlite::params![author, limit, offset], |row| {
        row.get::<_, Vec<u8>>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// True iff `post_id` is a publicly-servable post by `author` — the same
/// [`crate::db::public_servability::PUBLIC_POST_SERVABLE`] predicate
/// `public_outbox_page` applies, narrowed to a single id for the
/// note-dereference route.
pub fn public_note_exists(conn: &Connection, author: &[u8], post_id: &[u8]) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM content c
                 LEFT JOIN content_meta cm ON cm.content_id = c.id
                 WHERE c.id = ?1 AND c.author = ?2 AND {servable}",
                servable = crate::db::public_servability::PUBLIC_POST_SERVABLE.as_str(),
            ),
            rusqlite::params![post_id, author],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(n > 0)
}

/// Resolve the delivery inbox for an interaction on a mapped remote object:
/// the owning actor's cached inbox (personal preferred, shared fallback)
/// when the map row recorded the actor URI, else the URL-shape heuristic
/// below (an uncached actor, or a local row with no recorded URI). Shared by
/// the interact arms and the `Create`-push's referenced-author delivery
/// (`activitypub.md` § Reply and quote) — one copy.
pub fn resolve_delivery_inbox(
    conn: &rusqlite::Connection,
    remote_actor_uri: Option<&str>,
    ap_url: &str,
) -> String {
    if let Some(uri) = remote_actor_uri
        && let Ok(Some(actor)) = get_remote_actor(conn, uri)
    {
        if !actor.inbox.is_empty() {
            return actor.inbox;
        }
        if let Some(shared) = actor.shared_inbox.filter(|s| !s.is_empty()) {
            return shared;
        }
    }
    derive_inbox_from_url(ap_url)
}

/// Derive the likely inbox URL from a remote object URL.
///
/// Heuristic fallback for map rows with no recorded `remote_actor_uri`:
/// strip the path and append `/inbox`. Works for most Mastodon-like servers.
fn derive_inbox_from_url(ap_url: &str) -> String {
    // Try to find the actor segment: https://server/users/name/statuses/123
    // -> inbox: https://server/users/name/inbox
    if let Some(idx) = ap_url.find("/statuses/") {
        return format!("{}/inbox", &ap_url[..idx]);
    }
    if let Some(idx) = ap_url.find("/notes/") {
        return format!("{}/inbox", &ap_url[..idx]);
    }
    // Fallback: shared inbox at origin
    if let Some(idx) = ap_url.find("://")
        && let Some(slash) = ap_url[idx + 3..].find('/')
    {
        return format!("{}/inbox", &ap_url[..idx + 3 + slash]);
    }
    format!("{}/inbox", ap_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map row that recorded the owning actor URI resolves the actor's
    /// CACHED real inbox — the URL-shape heuristic is only the
    /// fallback (no recorded URI, or the actor is not cached).
    #[test]
    fn resolve_delivery_inbox_prefers_cached_actor() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
            .unwrap();
        let uri = "https://pleroma.example/users/bob";
        upsert_remote_actor(
            &conn,
            &RemoteActor {
                uri: uri.into(),
                // Deliberately NOT derivable from the object URL's shape.
                inbox: "https://pleroma.example/custom/inbox-path".into(),
                shared_inbox: None,
                public_key_pem: "pem".into(),
                preferred_username: None,
                display_name: None,
                avatar_url: None,
                banner_url: None,
                summary: None,
                last_fetched: 1,
            },
        )
        .unwrap();

        // Recorded + cached → the real inbox.
        assert_eq!(
            resolve_delivery_inbox(&conn, Some(uri), "https://pleroma.example/objects/123"),
            "https://pleroma.example/custom/inbox-path"
        );
        // No recorded URI → heuristic.
        assert_eq!(
            resolve_delivery_inbox(
                &conn,
                None,
                "https://mastodon.social/users/alice/statuses/1"
            ),
            "https://mastodon.social/users/alice/inbox"
        );
        // Recorded but not cached → heuristic fallback.
        assert_eq!(
            resolve_delivery_inbox(
                &conn,
                Some("https://uncached.example/users/x"),
                "https://misskey.io/notes/abc"
            ),
            "https://misskey.io/inbox"
        );
    }

    #[test]
    fn derive_inbox_mastodon_status() {
        let url = "https://mastodon.social/users/alice/statuses/12345";
        assert_eq!(
            derive_inbox_from_url(url),
            "https://mastodon.social/users/alice/inbox"
        );
    }

    #[test]
    fn derive_inbox_misskey_note() {
        let url = "https://misskey.io/notes/abc123";
        assert_eq!(derive_inbox_from_url(url), "https://misskey.io/inbox");
    }

    #[test]
    fn derive_inbox_fallback() {
        let url = "https://example.com/some/random/path";
        assert_eq!(derive_inbox_from_url(url), "https://example.com/inbox");
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrations::run_migrations(&conn).unwrap();
        conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
            .unwrap();
        conn
    }

    /// Seed a `post/text` row for `author`, optionally paywalled at `tier`.
    fn seed_post(
        conn: &Connection,
        id: [u8; 32],
        author: [u8; 32],
        created_at: i64,
        tier: Option<&str>,
    ) {
        seed_post_row(conn, id, author, created_at);
        conn.execute(
            "INSERT INTO content_meta (content_id, gated_tier) VALUES (?1, ?2)",
            rusqlite::params![id.as_slice(), tier],
        )
        .unwrap();
    }

    /// Seed a post carrying one of the moderation flags the outbox withholds.
    fn seed_flagged_post(
        conn: &Connection,
        id: [u8; 32],
        author: [u8; 32],
        created_at: i64,
        column: &str,
        value: &dyn rusqlite::ToSql,
    ) {
        seed_post_row(conn, id, author, created_at);
        conn.execute(
            &format!("INSERT INTO content_meta (content_id, {column}) VALUES (?1, ?2)"),
            rusqlite::params![id.as_slice(), value],
        )
        .unwrap();
    }

    fn seed_post_row(conn: &Connection, id: [u8; 32], author: [u8; 32], created_at: i64) {
        conn.execute(
            "INSERT INTO content (id, author, schema, created_at, payload)
             VALUES (?1, ?2, 'post/text', ?3, x'')",
            rusqlite::params![id.as_slice(), author.as_slice(), created_at],
        )
        .unwrap();
    }

    /// The single-note dereference gate applies the same servability filter
    /// as the outbox: right author, `post/%` schema, not gated. Anything else
    /// resolves to "does not exist" — this is a world-readable surface.
    #[test]
    fn public_note_exists_applies_outbox_filter() {
        let conn = test_conn();
        let author = [7u8; 32];
        let other = [8u8; 32];
        let public = [0xA1u8; 32];
        let gated = [0xB1u8; 32];
        seed_post(&conn, public, author, 1_000_000, None);
        seed_post(&conn, gated, author, 2_000_000, Some("premium"));

        assert!(public_note_exists(&conn, author.as_slice(), public.as_slice()).unwrap());
        assert!(
            !public_note_exists(&conn, author.as_slice(), gated.as_slice()).unwrap(),
            "a gated post is not dereferenceable"
        );
        assert!(
            !public_note_exists(&conn, other.as_slice(), public.as_slice()).unwrap(),
            "an author mismatch is not dereferenceable"
        );
        assert!(
            !public_note_exists(&conn, author.as_slice(), [0xC1u8; 32].as_slice()).unwrap(),
            "an absent post is not dereferenceable"
        );
    }

    /// The outbox never serves a gated (paywalled) post — neither in the page
    /// nor in `totalItems`. It is an unauthenticated world-readable surface,
    /// and a gated post sits in the `post/%` projection right next to public
    /// ones, so only the `content_meta.gated_tier` filter separates them.
    #[test]
    fn public_outbox_reads_exclude_gated_posts() {
        let conn = test_conn();
        let author = [7u8; 32];
        let public = [0xA1u8; 32];
        let gated = [0xB1u8; 32];
        seed_post(&conn, public, author, 1_000_000, None);
        seed_post(&conn, gated, author, 2_000_000, Some("premium")); // newer

        assert_eq!(
            public_outbox_count(&conn, author.as_slice()).unwrap(),
            1,
            "totalItems counts only the public post"
        );
        assert_eq!(
            public_outbox_page(&conn, author.as_slice(), 20, 0).unwrap(),
            vec![public.to_vec()],
            "the gated post is excluded despite being newer"
        );
    }

    /// A post with **no** `content_meta` row is NOT servable — absence must
    /// never read as permission.
    ///
    /// **This inverts the assertion that stood here until 2026-07-29**, whose
    /// stated rationale was that an INNER JOIN "would silently empty every
    /// outbox whose posts predate their meta row". That era does not exist:
    /// `content_meta` is base schema (`db/schema.rs`, `CREATE TABLE IF NOT
    /// EXISTS`), not a later migration, so no post ever predated the table.
    /// The genuinely reachable source is the opposite one — all four
    /// post-write sites swallow the `write_post_index` error (`let _ =`), and
    /// that call is what carries `gated_tier` into the index. So the old
    /// reading published a **gated** post as a free public Note whenever its
    /// index write failed.
    ///
    /// The decisive argument is coherence with the read the moderation design
    /// already cites: the feed read INNER-joins `content_meta`
    /// (`db/feeds.rs`), so such a post is invisible in its own author's feed
    /// while this world-readable surface federated it. Requiring the row makes
    /// the outbox *consistent* with the feed, not stricter than it.
    #[test]
    fn public_outbox_reads_exclude_posts_without_a_meta_row() {
        let conn = test_conn();
        let author = [7u8; 32];
        let bare = [0xC1u8; 32];
        seed_post_row(&conn, bare, author, 1000);

        assert_eq!(
            public_outbox_count(&conn, author.as_slice()).unwrap(),
            0,
            "a half-indexed post is not counted"
        );
        assert!(
            public_outbox_page(&conn, author.as_slice(), 20, 0)
                .unwrap()
                .is_empty(),
            "a half-indexed post is not federated"
        );
        assert!(
            !public_note_exists(&conn, author.as_slice(), bare.as_slice()).unwrap(),
            "and it is not dereferenceable either"
        );
    }

    /// The outbox is a public federation surface, so a legally compelled
    /// takedown — and the two discretionary withholding flags — must stop it,
    /// exactly as they stop the feed read (`moderation.md` § Legal takedown).
    #[test]
    fn public_outbox_reads_exclude_every_moderation_flag() {
        let conn = test_conn();
        let author = [7u8; 32];
        let public = [0xA1u8; 32];
        seed_post(&conn, public, author, 1_000_000, None);
        seed_flagged_post(
            &conn,
            [0xB2u8; 32],
            author,
            2_000_000,
            "legal_takedown_ref",
            &"court-order-1",
        );
        seed_flagged_post(&conn, [0xB3u8; 32], author, 3_000_000, "quarantined", &1i64);
        seed_flagged_post(&conn, [0xB4u8; 32], author, 4_000_000, "suppressed", &1i64);

        assert_eq!(
            public_outbox_count(&conn, author.as_slice()).unwrap(),
            1,
            "totalItems counts only the servable post"
        );
        assert_eq!(
            public_outbox_page(&conn, author.as_slice(), 20, 0).unwrap(),
            vec![public.to_vec()],
            "withheld posts are excluded despite being newer"
        );
        assert!(
            !public_note_exists(&conn, author.as_slice(), [0xB2u8; 32].as_slice()).unwrap(),
            "a taken-down post is not dereferenceable"
        );
    }

    /// Ruling 1 (`archive-import.md` § Compatibility → *Slice-3 rulings*): the
    /// outbox never federates an archive-imported post — served on Fauna,
    /// never re-broadcast off-box — while the native post beside it (same
    /// author) is federated as before. The source token lives on
    /// `content.source` (not `content_meta`, which has no `source` column),
    /// so the imported row is seeded there directly.
    #[test]
    fn public_outbox_reads_exclude_archive_imported_posts() {
        let conn = test_conn();
        let author = [7u8; 32];
        let imported_id = [0xA1u8; 32];
        let native_id = [0xA2u8; 32];
        conn.execute(
            "INSERT INTO content (id, author, schema, created_at, payload, source)
             VALUES (?1, ?2, 'post/text', ?3, x'', 'facebook')",
            rusqlite::params![imported_id.as_slice(), author.as_slice(), 1_000_000i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO content_meta (content_id) VALUES (?1)",
            rusqlite::params![imported_id.as_slice()],
        )
        .unwrap();
        seed_post(&conn, native_id, author, 2_000_000, None);

        assert_eq!(public_outbox_count(&conn, &author).unwrap(), 1);
        assert_eq!(
            public_outbox_page(&conn, &author, 10, 0).unwrap(),
            vec![native_id.to_vec()]
        );
        assert!(!public_note_exists(&conn, &author, &imported_id).unwrap());
        assert!(public_note_exists(&conn, &author, &native_id).unwrap());
    }

    #[test]
    fn create_and_get_account() {
        let conn = test_conn();
        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "-----BEGIN PUBLIC KEY-----\nMIIBI...",
        )
        .unwrap();
        let acct = get_account(&conn, "actor1").unwrap().unwrap();
        assert_eq!(acct.username, "alice");
        assert_eq!(acct.actor_url, "https://example.com/users/alice");
        assert!(acct.enabled);
        assert!(acct.auto_accept_follows);
        assert_eq!(acct.default_visibility, "public");
    }

    #[test]
    fn get_account_by_username_works() {
        let conn = test_conn();
        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "pem",
        )
        .unwrap();
        let acct = get_account_by_username(&conn, "alice").unwrap().unwrap();
        assert_eq!(acct.actor_id, "actor1");
        assert!(get_account_by_username(&conn, "bob").unwrap().is_none());
    }

    #[test]
    fn any_enabled_account_follows_requires_enabled_and_outbound() {
        let conn = test_conn();
        let remote = "https://remote.example/users/mallory";
        assert!(!any_enabled_account_follows(&conn, remote).unwrap());

        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "pem",
        )
        .unwrap();

        // An inbound follow (mallory follows alice) is not an opt-in.
        create_follow(&conn, "actor1", remote, "inbound", None).unwrap();
        assert!(!any_enabled_account_follows(&conn, remote).unwrap());

        // An outbound follow is — even while still pending (the local user's
        // Follow request is itself the opt-in act).
        create_follow(&conn, "actor1", remote, "outbound", None).unwrap();
        assert!(any_enabled_account_follows(&conn, remote).unwrap());

        // ...but not from a disabled account.
        conn.execute("UPDATE ap_accounts SET enabled = 0", [])
            .unwrap();
        assert!(!any_enabled_account_follows(&conn, remote).unwrap());
    }

    #[test]
    fn enabled_account_owns_ap_url_requires_local_enabled_owner() {
        let conn = test_conn();
        let local_url = "https://example.com/ap/users/alice/notes/aa";
        let remote_url = "https://remote.example/notes/bb";

        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "pem",
        )
        .unwrap();
        // A local (Create-push-shaped) row: actor_id = the account's.
        insert_post_map(&conn, "post-local", local_url, "actor1", None).unwrap();
        // An ingested remote row: synthetic actor hex, joins to no account.
        insert_post_map(
            &conn,
            "post-remote",
            remote_url,
            "eeee",
            Some("https://remote.example/users/eve"),
        )
        .unwrap();

        assert!(enabled_account_owns_ap_url(&conn, local_url).unwrap());
        assert!(!enabled_account_owns_ap_url(&conn, remote_url).unwrap());
        assert!(!enabled_account_owns_ap_url(&conn, "https://nowhere.example/x").unwrap());

        // A tombstoned (deleted) local post is no longer an opt-in surface.
        tombstone_post_map(&conn, "post-local").unwrap();
        assert!(!enabled_account_owns_ap_url(&conn, local_url).unwrap());
        conn.execute("UPDATE ap_post_map SET tombstoned = 0", [])
            .unwrap();

        // Neither is a disabled account's post.
        conn.execute("UPDATE ap_accounts SET enabled = 0", [])
            .unwrap();
        assert!(!enabled_account_owns_ap_url(&conn, local_url).unwrap());
    }

    #[test]
    fn delete_account_removes_follows() {
        let conn = test_conn();
        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "pem",
        )
        .unwrap();
        create_follow(
            &conn,
            "actor1",
            "https://remote.example/users/bob",
            "outbound",
            None,
        )
        .unwrap();
        delete_account(&conn, "actor1").unwrap();
        assert!(get_account(&conn, "actor1").unwrap().is_none());
        let follows = list_outbound_follows(&conn, "actor1").unwrap();
        assert!(follows.is_empty());
    }

    #[test]
    fn update_settings_partial() {
        let conn = test_conn();
        create_account(
            &conn,
            "actor1",
            "alice",
            "https://example.com/users/alice",
            b"secret",
            "pem",
        )
        .unwrap();
        update_settings(
            &conn,
            "actor1",
            &ApSettings {
                enabled: Some(false),
                default_visibility: Some("unlisted".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let acct = get_account(&conn, "actor1").unwrap().unwrap();
        assert!(!acct.enabled);
        assert_eq!(acct.default_visibility, "unlisted");
        // Unchanged fields
        assert!(acct.auto_accept_follows);
    }

    #[test]
    fn remote_actor_upsert_and_get() {
        let conn = test_conn();
        let actor = RemoteActor {
            uri: "https://remote.example/users/bob".into(),
            inbox: "https://remote.example/users/bob/inbox".into(),
            shared_inbox: Some("https://remote.example/inbox".into()),
            public_key_pem: "pem-data".into(),
            preferred_username: Some("bob".into()),
            display_name: Some("Bob".into()),
            avatar_url: None,
            banner_url: None,
            summary: None,
            last_fetched: 1_700_000_000,
        };
        upsert_remote_actor(&conn, &actor).unwrap();
        let got = get_remote_actor(&conn, "https://remote.example/users/bob")
            .unwrap()
            .unwrap();
        assert_eq!(got.inbox, "https://remote.example/users/bob/inbox");
        assert_eq!(got.display_name, Some("Bob".into()));

        assert!(
            get_remote_actor(&conn, "https://missing.example")
                .unwrap()
                .is_none()
        );
    }

    /// The bridged-author transit point: caching an actor projects its face
    /// under the synthetic id the inbox rests its posts under, with the
    /// fediverse handle spelled `@user@host` and the icon behind the shared
    /// proxy; a re-fetch with a new name refreshes it.
    #[test]
    fn upsert_remote_actor_projects_the_bridged_author_face() {
        use fauna_bridge_activitypub::identity::synthetic_actor_id;
        let conn = test_conn();
        let mut actor = RemoteActor {
            uri: "https://remote.example/users/bob".into(),
            inbox: "https://remote.example/users/bob/inbox".into(),
            shared_inbox: None,
            public_key_pem: "pem-data".into(),
            preferred_username: Some("bob".into()),
            display_name: Some("Bob".into()),
            avatar_url: Some("https://remote.example/media/bob.png".into()),
            banner_url: None,
            summary: None,
            last_fetched: 1_700_000_000,
        };
        upsert_remote_actor(&conn, &actor).unwrap();
        let id = synthetic_actor_id("https://remote.example/users/bob").0;
        let face = crate::db::bridge_authors::get(&conn, &id)
            .unwrap()
            .expect("the cache write projects the face")
            .display();
        assert_eq!(face.handle.as_deref(), Some("@bob@remote.example"));
        assert_eq!(face.display_name.as_deref(), Some("Bob"));
        assert_eq!(
            face.avatar_url.as_deref(),
            Some("/api/v1/media/proxy?url=https%3A%2F%2Fremote.example%2Fmedia%2Fbob.png")
        );

        actor.display_name = Some("Robert".into());
        actor.avatar_url = None;
        upsert_remote_actor(&conn, &actor).unwrap();
        let face = crate::db::bridge_authors::get(&conn, &id)
            .unwrap()
            .unwrap()
            .display();
        assert_eq!(face.display_name.as_deref(), Some("Robert"));
        assert!(
            face.avatar_url.is_none(),
            "a re-fetch replaces the whole face"
        );

        // No preferredUsername → no handle (never a bare `@@host`).
        let nameless = RemoteActor {
            uri: "https://remote.example/users/anon".into(),
            preferred_username: None,
            display_name: Some("Anon".into()),
            ..actor.clone()
        };
        assert!(bridge_author_of(&nameless).handle.is_none());
    }

    #[test]
    fn follow_lifecycle() {
        let conn = test_conn();
        create_follow(
            &conn,
            "actor1",
            "https://remote.example/users/bob",
            "outbound",
            Some("https://example.com/activities/1"),
        )
        .unwrap();
        let follows = list_outbound_follows(&conn, "actor1").unwrap();
        assert_eq!(follows.len(), 1);
        assert_eq!(follows[0].state, "pending");

        accept_follow(
            &conn,
            "actor1",
            "https://remote.example/users/bob",
            "outbound",
        )
        .unwrap();
        let follows = list_outbound_follows(&conn, "actor1").unwrap();
        assert_eq!(follows[0].state, "accepted");

        delete_follow(
            &conn,
            "actor1",
            "https://remote.example/users/bob",
            "outbound",
        )
        .unwrap();
        let follows = list_outbound_follows(&conn, "actor1").unwrap();
        assert!(follows.is_empty());
    }

    #[test]
    fn inbound_follows_listed_separately() {
        let conn = test_conn();
        create_follow(
            &conn,
            "actor1",
            "https://remote.example/users/bob",
            "inbound",
            None,
        )
        .unwrap();
        create_follow(
            &conn,
            "actor1",
            "https://remote.example/users/carol",
            "outbound",
            None,
        )
        .unwrap();

        let followers = list_followers(&conn, "actor1").unwrap();
        assert_eq!(followers.len(), 1);
        assert_eq!(
            followers[0].remote_actor_uri,
            "https://remote.example/users/bob"
        );

        let following = list_outbound_follows(&conn, "actor1").unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(
            following[0].remote_actor_uri,
            "https://remote.example/users/carol"
        );
    }

    #[test]
    fn post_map_roundtrip() {
        let conn = test_conn();
        insert_post_map(
            &conn,
            "fauna_post_1",
            "https://example.com/posts/1",
            "actor1",
            Some("https://example.com/users/bob"),
        )
        .unwrap();

        assert_eq!(
            get_ap_url_for_post(&conn, "fauna_post_1")
                .unwrap()
                .as_deref(),
            Some("https://example.com/posts/1"),
        );
        assert_eq!(
            get_post_id_for_ap_url(&conn, "https://example.com/posts/1")
                .unwrap()
                .as_deref(),
            Some("fauna_post_1"),
        );
        // The interact-path target lookup carries the recorded actor URI.
        let (url, actor) = get_ap_target_for_post(&conn, "fauna_post_1")
            .unwrap()
            .unwrap();
        assert_eq!(url, "https://example.com/posts/1");
        assert_eq!(actor.as_deref(), Some("https://example.com/users/bob"));

        assert!(get_ap_url_for_post(&conn, "missing").unwrap().is_none());
        assert!(
            get_post_id_for_ap_url(&conn, "https://missing")
                .unwrap()
                .is_none()
        );
    }

    /// A local-post row (no recorded actor URI) still round-trips with `None` —
    /// the interact path falls back to the URL heuristic for these.
    #[test]
    fn post_map_without_actor_uri() {
        let conn = test_conn();
        insert_post_map(
            &conn,
            "fauna_post_2",
            "https://example.com/posts/2",
            "actor1",
            None,
        )
        .unwrap();
        let (_, actor) = get_ap_target_for_post(&conn, "fauna_post_2")
            .unwrap()
            .unwrap();
        assert!(actor.is_none());
    }

    #[test]
    fn tombstone_hides_post_map() {
        let conn = test_conn();
        insert_post_map(
            &conn,
            "fauna_post_1",
            "https://example.com/posts/1",
            "actor1",
            None,
        )
        .unwrap();
        tombstone_post_map(&conn, "fauna_post_1").unwrap();
        assert!(
            get_ap_url_for_post(&conn, "fauna_post_1")
                .unwrap()
                .is_none()
        );
        assert!(
            get_post_id_for_ap_url(&conn, "https://example.com/posts/1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn delivery_queue_lifecycle() {
        let conn = test_conn();
        enqueue_delivery(
            &conn,
            r#"{"type":"Create"}"#,
            "https://remote.example/inbox",
        )
        .unwrap();

        let pending = get_pending_deliveries(&conn, 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, "pending");
        assert_eq!(pending[0].attempts, 0);
        let job_id = pending[0].id;

        mark_delivery_done(&conn, job_id).unwrap();
        let pending = get_pending_deliveries(&conn, 10).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn delivery_retry_increments_attempts() {
        let conn = test_conn();
        enqueue_delivery(
            &conn,
            r#"{"type":"Create"}"#,
            "https://remote.example/inbox",
        )
        .unwrap();
        let pending = get_pending_deliveries(&conn, 10).unwrap();
        let job_id = pending[0].id;

        mark_delivery_retry(&conn, job_id).unwrap();
        // The job should have incremented attempts and pushed next_retry_at into the future.
        let row: (i64, i64) = conn
            .query_row(
                "SELECT attempts, next_retry_at FROM ap_delivery_queue WHERE id = ?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, 1);
        // next_retry_at should be in the future (at least now + 60 seconds)
        assert!(row.1 > now_secs());
    }

    #[test]
    fn delivery_failed_excludes_from_pending() {
        let conn = test_conn();
        enqueue_delivery(
            &conn,
            r#"{"type":"Delete"}"#,
            "https://remote.example/inbox",
        )
        .unwrap();
        let pending = get_pending_deliveries(&conn, 10).unwrap();
        let job_id = pending[0].id;

        mark_delivery_failed(&conn, job_id).unwrap();
        let pending = get_pending_deliveries(&conn, 10).unwrap();
        assert!(pending.is_empty());
    }
}
