//! Database query helpers for Bluesky account management.

use anyhow::Result;
use fauna_bridge_atproto::oauth::{BlueskyAgent, restore_agent};
use fauna_bridge_atproto::types::BlueskyFeedGenerator;
use rusqlite::{Connection, OptionalExtension};

use crate::api_error::ApiError;
use crate::routes::AppState;

/// A linked Bluesky account record from the database.
pub struct LinkedAccount {
    pub bluesky_did: String,
    pub bluesky_handle: String,
}

/// `actor_hex` decoded, if and only if it is the canonical
/// `bluesky_accounts.actor_id` spelling: lowercase hex of the 32-byte actor id
/// (`db::actor_tables::ActorKey::Hex`).
///
/// Every route looks a link up by `hex::encode` of the authenticated actor, so
/// a row under any other spelling — uppercase, the wrong length, not hex at all
/// — names no actor a route can reach. Decoded here rather than with SQL
/// `unhex()`, which would pin the nest to SQLite ≥ 3.41.
fn canonical_actor(actor_hex: &str) -> Option<[u8; 32]> {
    let actor: [u8; 32] = hex::decode(actor_hex).ok()?.try_into().ok()?;
    (hex::encode(actor) == actor_hex).then_some(actor)
}

/// Whether the consume-side OAuth poller may run for `actor_hex` — **D7's
/// predicate** (`atproto-pds-full.md` § D7), evaluated on the actor rather than
/// on how a row spells it.
///
/// Both must hold: `actor_hex` is the canonical spelling ([`canonical_actor`]),
/// and the actor it decodes to holds no active nest-hosted ATProto identity.
/// The hosted check compares the decoded bytes against `atproto_identities`'
/// BLOB key, so no spelling of a hosted actor can fail it open — a string
/// comparison against `lower(hex(actor_id))` did exactly that for any row not
/// already spelled canonically. A non-canonical id is refused outright rather
/// than normalized: it names no reachable actor, and the only way one reaches
/// the table is through the OAuth callback's attacker-controlled `state`.
pub fn consume_side_poll_allowed(conn: &Connection, actor_hex: &str) -> Result<bool> {
    let Some(actor) = canonical_actor(actor_hex) else {
        return Ok(false);
    };
    let hosted: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM atproto_identities
                         WHERE status = 'active' AND actor_id = ?1)",
        rusqlite::params![&actor[..]],
        |row| row.get(0),
    )?;
    Ok(!hosted)
}

/// Every actor this nest may act for on the **consume side** — one row per
/// `bluesky_accounts` link that passes [`consume_side_poll_allowed`]. Returned
/// as the lowercase hex actor ids that table keys on.
///
/// **This is D7's gate** (`atproto-pds-full.md` § D7, and the binding
/// instruction in its § Implementation status: *the session that wires
/// per-user polling implements D7's gate in the same change, at the start
/// site*). An account whose ATProto backing is nest-hosted reads its Bluesky
/// activity through service-auth proxying, so a consume-side OAuth poller must
/// never run for it — not even to fail. The one-backing rule already makes the
/// two backings mutually exclusive per user, but nothing at rest enforces it,
/// so the exclusion is written here rather than assumed. `notif_sync::poll_bluesky_notifications`
/// re-checks the same predicate before it starts.
///
/// A row under a non-canonical spelling is never enumerated.
/// [`upsert_linked_account`] refuses to write one, and no migration rewrites
/// any that predate that refusal: a legitimate link was only ever written as
/// `hex::encode`, so such a row can only have come from a forged callback
/// `state`, names no actor any route reaches, and is inert here. Nothing a user
/// cannot recreate is at stake either way — the row is a re-linkable pointer to
/// an OAuth session the session store keys by DID.
///
/// An unreadable `actor_id` (not TEXT) is skipped for the same reason: it names
/// no actor in this table's spelling. A failure evaluating the predicate fails
/// the whole enumeration — the caller polls nobody rather than guess.
///
/// Excluding an unlinked actor needs no clause: they have no row to enumerate.
pub fn list_consume_side_linked_actors(conn: &Connection) -> Result<Vec<String>> {
    let linked: Vec<String> = {
        let mut stmt = conn.prepare("SELECT actor_id FROM bluesky_accounts ORDER BY actor_id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut actors = Vec::with_capacity(linked.len());
    for actor_hex in linked {
        if consume_side_poll_allowed(conn, &actor_hex)? {
            actors.push(actor_hex);
        }
    }
    Ok(actors)
}

/// Look up a linked Bluesky account for a Fauna actor.
pub fn get_linked_account(conn: &Connection, actor_hex: &str) -> Result<Option<LinkedAccount>> {
    match conn.query_row(
        "SELECT bluesky_did, bluesky_handle FROM bluesky_accounts WHERE actor_id = ?1",
        rusqlite::params![actor_hex],
        |row| {
            Ok(LinkedAccount {
                bluesky_did: row.get(0)?,
                bluesky_handle: row.get(1)?,
            })
        },
    ) {
        Ok(account) => Ok(Some(account)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Store or update a linked Bluesky account.
///
/// Refuses any `actor_hex` that is not the canonical spelling
/// ([`canonical_actor`]). The production caller, the OAuth callback, splits
/// the id out of a `state` it calls fully attacker-controlled, so this is the
/// one place every write passes through — the check lives here rather than at
/// a caller, so no row can ever key on a spelling no route reaches.
pub fn upsert_linked_account(
    conn: &Connection,
    actor_hex: &str,
    did: &str,
    handle: &str,
) -> Result<()> {
    if canonical_actor(actor_hex).is_none() {
        anyhow::bail!("refusing a non-canonical actor id for a Bluesky link: {actor_hex:?}");
    }
    let now = fauna_core::data::Timestamp::now_secs();
    conn.execute(
        "INSERT OR REPLACE INTO bluesky_accounts \
         (actor_id, bluesky_did, bluesky_handle, access_token, refresh_token, dpop_key, \
          token_expires, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        rusqlite::params![
            actor_hex,
            did,
            handle,
            b"" as &[u8], // tokens managed by session store
            b"" as &[u8],
            b"" as &[u8],
            0i64,
            now,
        ],
    )?;
    Ok(())
}

/// Remove a linked Bluesky account for a Fauna actor.
pub fn delete_linked_account(conn: &Connection, actor_hex: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM bluesky_accounts WHERE actor_id = ?1",
        rusqlite::params![actor_hex],
    )?;
    Ok(())
}

/// Store a Bluesky interaction record URI for undo operations.
pub fn store_interaction(
    conn: &Connection,
    actor_hex: &str,
    post_uri: &str,
    interaction_type: &str,
    record_uri: &str,
) -> Result<()> {
    let now = fauna_core::data::Timestamp::now_secs();
    conn.execute(
        "INSERT OR REPLACE INTO bluesky_interactions \
         (actor_id, fauna_post_id, interaction_type, record_uri, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![actor_hex, post_uri, interaction_type, record_uri, now],
    )?;
    Ok(())
}

/// Remove a stored interaction.
pub fn remove_interaction(conn: &Connection, actor_hex: &str, record_uri: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM bluesky_interactions WHERE actor_id = ?1 AND record_uri = ?2",
        rusqlite::params![actor_hex, record_uri],
    )?;
    Ok(())
}

/// Look up the record URI of a previously-stored interaction. Used by the
/// unified `unlike` / `unrepost` flow to translate (post_uri, interaction_type)
/// back to the AT-proto record that needs deleting.
pub fn get_interaction(
    conn: &Connection,
    actor_hex: &str,
    post_uri: &str,
    interaction_type: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT record_uri FROM bluesky_interactions \
             WHERE actor_id = ?1 AND fauna_post_id = ?2 AND interaction_type = ?3",
            rusqlite::params![actor_hex, post_uri, interaction_type],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Get saved feeds for a Fauna actor.
pub fn get_saved_feeds(conn: &Connection, actor_hex: &str) -> Result<Vec<BlueskyFeedGenerator>> {
    let mut stmt = conn.prepare(
        "SELECT feed_uri, display_name, description, avatar \
         FROM bluesky_saved_feeds WHERE actor_id = ?1 ORDER BY saved_at DESC",
    )?;
    let feeds = stmt
        .query_map(rusqlite::params![actor_hex], |row| {
            Ok(BlueskyFeedGenerator {
                uri: row.get(0)?,
                did: String::new(),
                display_name: row.get(1)?,
                description: row.get(2)?,
                avatar: row.get(3)?,
                like_count: 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(feeds)
}

/// Save a feed for a Fauna actor.
pub fn save_feed(
    conn: &Connection,
    actor_hex: &str,
    uri: &str,
    display_name: &str,
    description: Option<&str>,
    avatar: Option<&str>,
) -> Result<()> {
    let now = fauna_core::data::Timestamp::now_secs();
    conn.execute(
        "INSERT OR REPLACE INTO bluesky_saved_feeds \
         (actor_id, feed_uri, display_name, description, avatar, saved_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![actor_hex, uri, display_name, description, avatar, now],
    )?;
    Ok(())
}

/// Remove a saved feed for a Fauna actor.
pub fn unsave_feed(conn: &Connection, actor_hex: &str, uri: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM bluesky_saved_feeds WHERE actor_id = ?1 AND feed_uri = ?2",
        rusqlite::params![actor_hex, uri],
    )?;
    Ok(())
}

/// Get the write_through setting for a Fauna actor (0 = off, 1 = copy, 2 = mirror).
///
/// Returns 0 if the actor has no linked account.
pub fn get_write_through(conn: &Connection, actor_hex: &str) -> Result<i64> {
    match conn.query_row(
        "SELECT write_through FROM bluesky_accounts WHERE actor_id = ?1",
        rusqlite::params![actor_hex],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Set the write_through setting for a Fauna actor.
pub fn set_write_through(conn: &Connection, actor_hex: &str, mode: i64) -> Result<()> {
    conn.execute(
        "UPDATE bluesky_accounts SET write_through = ?2 WHERE actor_id = ?1",
        rusqlite::params![actor_hex, mode],
    )?;
    Ok(())
}

/// Store the AT-URI mapping for a cross-posted Fauna post.
///
/// `cid` is the `createRecord` reply's CID, stored so [`resolve_uri_and_cid`]
/// needs no `getRecord` round trip (`bridges.md` § Unified feed ingestion →
/// *Bridge ingestion*, ruling 2).
///
/// `content_json`, `cached_at` and `expires_at` are the residue of the cache
/// design this table was first built for: written empty / now / 0, read by
/// nothing, retired only by an explicit contract step (the reconciler refuses a
/// drop).
pub fn store_crosspost_mapping(
    conn: &Connection,
    fauna_post_id: &str,
    at_uri: &str,
    author_did: &str,
    cid: &str,
) -> Result<()> {
    let now = fauna_core::data::Timestamp::now_secs();
    conn.execute(
        "INSERT OR REPLACE INTO bluesky_posts \
         (fauna_post_id, at_uri, author_did, content_json, interacted, cached_at, expires_at, cid) \
         VALUES (?1, ?2, ?3, X'', 1, ?4, 0, ?5)",
        rusqlite::params![fauna_post_id, at_uri, author_did, now, cid],
    )?;
    Ok(())
}

/// Record the map row of a post the consume side **ingested** — one per
/// stored post, `INSERT OR IGNORE` on the table's `UNIQUE(at_uri)`, which is
/// the dedupe: a re-poll of the same window, or a second linked account whose
/// timeline carries the same post, inserts nothing. Returns whether this call
/// inserted the row.
///
/// The same table the write-through fills for the other direction — one map
/// for both, so an ingested post is resolvable as an interaction and reply
/// target by exactly the lookup a cross-posted one is (`bridges.md`
/// § Cross-posting → *A post that references a Bluesky record*).
pub fn insert_ingested_post_mapping(
    conn: &Connection,
    fauna_post_id: &str,
    at_uri: &str,
    cid: &str,
    author_did: &str,
) -> Result<bool> {
    let now = fauna_core::data::Timestamp::now_secs();
    let n = conn.execute(
        "INSERT OR IGNORE INTO bluesky_posts \
         (fauna_post_id, at_uri, author_did, content_json, interacted, cached_at, expires_at, cid) \
         VALUES (?1, ?2, ?3, X'', 0, ?4, 0, ?5)",
        rusqlite::params![fauna_post_id, at_uri, author_did, now, cid],
    )?;
    Ok(n > 0)
}

/// The local post id (hex) an AT-URI maps to, if the nest holds the post —
/// how the ingest resolves a reply parent or a quote target to a local row,
/// and the dedupe read before a store.
pub fn get_post_id_for_at_uri(conn: &Connection, at_uri: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT fauna_post_id FROM bluesky_posts WHERE at_uri = ?1",
            rusqlite::params![at_uri],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// The map row's `(at_uri, cid)` for a Fauna post. Both writers store the
/// CID (the cross-post's `createRecord` reply, the ingested post's own).
pub fn get_crosspost_uri_and_cid(
    conn: &Connection,
    fauna_post_id: &str,
) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT at_uri, cid FROM bluesky_posts WHERE fauna_post_id = ?1",
            rusqlite::params![fauna_post_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?)
}

/// Look up the AT-URI for a cross-posted Fauna post.
pub fn get_crosspost_uri(conn: &Connection, fauna_post_id: &str) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT at_uri FROM bluesky_posts WHERE fauna_post_id = ?1",
        rusqlite::params![fauna_post_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(uri) => Ok(Some(uri)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A Bluesky record a Fauna post maps to, read back from the user's PDS:
/// its AT-URI (from `bluesky_posts`), its **current** CID and the record
/// value itself — the last is what the reply derivation reads the thread
/// root from ([`thread_root_of`]).
pub struct ResolvedRecord {
    pub at_uri: String,
    pub cid: String,
    pub value: serde_json::Value,
}

/// Resolve the AT-URI and CID for a Fauna post.
///
/// Reads the map row in `bluesky_posts` — both writers store the CID, so the
/// pair is returned with no network step. Returns `None` if no mapping exists.
///
/// **The stored CID is never re-fetched** (`bridges.md` § Unified feed
/// ingestion → *Bridge ingestion*, ruling 2): a Bluesky post has one revision
/// in practice, and a strong ref naming the revision the nest saw is the one
/// every like, repost and quote should carry. The reply derivation is the one
/// caller that still fetches — it needs the parent *record* for the thread
/// root, which nothing stores — and goes through [`resolve_record`] directly.
pub async fn resolve_uri_and_cid(
    state: &AppState,
    fauna_post_id: &str,
) -> Result<Option<(String, String)>> {
    let conn = state.db.conn().await;
    get_crosspost_uri_and_cid(&conn, fauna_post_id)
}

/// The record a Fauna post maps to, read back from the PDS — one `getRecord`
/// round trip serving the current CID and the record value alike. Always a
/// network step: the value (the thread root the reply derivation reads) is
/// not stored, so the stored CID cannot answer for it.
pub async fn resolve_record(
    state: &AppState,
    actor_hex: &str,
    fauna_post_id: &str,
) -> Result<Option<ResolvedRecord>> {
    let conn = state.db.conn().await;
    let at_uri = match get_crosspost_uri(&conn, fauna_post_id) {
        Ok(Some(uri)) => uri,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
    };
    drop(conn);

    // Parse the AT-URI to extract repo + collection + rkey
    // Format: at://did:plc:xxx/app.bsky.feed.post/rkey
    let parts: Vec<&str> = at_uri
        .strip_prefix("at://")
        .unwrap_or(&at_uri)
        .splitn(3, '/')
        .collect();
    if parts.len() != 3 {
        anyhow::bail!("malformed AT-URI: {at_uri}");
    }

    let agent = get_agent_for_actor(state, actor_hex)
        .await
        .map_err(|e| anyhow::anyhow!("agent error: {e}"))?;

    use fauna_bridge_atproto::atrium_api::com::atproto::repo::get_record;

    let params = get_record::ParametersData {
        collection: parts[1].parse().map_err(|_| anyhow::anyhow!("bad NSID"))?,
        repo: fauna_bridge_atproto::atrium_api::types::string::AtIdentifier::Did(
            parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("bad DID in AT-URI"))?,
        ),
        rkey: parts[2]
            .parse()
            .map_err(|_| anyhow::anyhow!("bad rkey in AT-URI"))?,
        cid: None,
    };

    let output = agent
        .api
        .com
        .atproto
        .repo
        .get_record(params.into())
        .await
        .map_err(|e| anyhow::anyhow!("getRecord failed: {e}"))?;

    let cid = output
        .cid
        .as_ref()
        .map(|c| c.as_ref().to_string())
        .unwrap_or_default();
    let value = serde_json::to_value(&output.value)
        .map_err(|e| anyhow::anyhow!("getRecord value not JSON-shaped: {e}"))?;

    Ok(Some(ResolvedRecord { at_uri, cid, value }))
}

/// The thread root a reply to `parent` must name — the ATProto rule the
/// hosted projection applies too (`atproto-pds-bridge.md` § Projection &
/// backfill → *Translation edges*): the parent record's own `reply.root` when
/// it carries one, else the parent itself starts the thread. Pure over the
/// record JSON so the rule is pinned without a PDS.
pub fn thread_root_of(
    parent_record: &serde_json::Value,
    parent_uri: &str,
    parent_cid: &str,
) -> (String, String) {
    let root = &parent_record["reply"]["root"];
    match (root["uri"].as_str(), root["cid"].as_str()) {
        (Some(uri), Some(cid)) if !uri.is_empty() && !cid.is_empty() => {
            (uri.to_string(), cid.to_string())
        }
        _ => (parent_uri.to_string(), parent_cid.to_string()),
    }
}

/// The `reply: {parent, root}` refs for a `Reference::Reply` whose target is
/// the Fauna post `parent_fauna_post_id` — `None` when that post maps to no
/// Bluesky record (the caller then cross-posts standalone, the projection's
/// rule). One `getRecord`: the parent's current CID and its own root.
pub async fn resolve_reply_refs(
    state: &AppState,
    actor_hex: &str,
    parent_fauna_post_id: &str,
) -> Result<Option<fauna_bridge_atproto::outbound::ReplyRefs>> {
    let Some(parent) = resolve_record(state, actor_hex, parent_fauna_post_id).await? else {
        return Ok(None);
    };
    let (root_uri, root_cid) = thread_root_of(&parent.value, &parent.at_uri, &parent.cid);
    Ok(Some(fauna_bridge_atproto::outbound::ReplyRefs {
        parent_uri: parent.at_uri,
        parent_cid: parent.cid,
        root_uri,
        root_cid,
    }))
}

/// Every Fauna post id (hex) with a **write-through** mapping — the rows
/// [`store_crosspost_mapping`] wrote (`interacted = 1`), never the ingested
/// rows [`insert_ingested_post_mapping`] wrote (`interacted = 0`), which map
/// someone else's record this nest has no business deleting. The post-delete
/// re-drive (`post_delete_redrive`) scans these for a cross-post whose post
/// is gone: a mapping outliving its post is exactly the retained-on-failure
/// row `write_through_delete_inner` left for a retry.
pub fn list_crosspost_post_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT fauna_post_id FROM bluesky_posts WHERE interacted = 1")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Delete a cross-post mapping.
pub fn delete_crosspost_mapping(conn: &Connection, fauna_post_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM bluesky_posts WHERE fauna_post_id = ?1",
        rusqlite::params![fauna_post_id],
    )?;
    Ok(())
}

/// Get an authenticated Bluesky agent for a Fauna actor.
///
/// Performs **no** identity-backing check: the session is restored by the
/// linked DID alone, which never sees the actor's backing. A caller that must
/// honour D7 checks [`consume_side_poll_allowed`] itself first.
pub async fn get_agent_for_actor(
    state: &AppState,
    actor_hex: &str,
) -> Result<BlueskyAgent, ApiError> {
    let oauth = state
        .bluesky_oauth()
        .ok_or_else(|| ApiError::internal("Bluesky not configured"))?;

    let conn = state.db.conn().await;
    let account = get_linked_account(&conn, actor_hex)
        .map_err(|e| ApiError::internal(format!("db error: {e}")))?;
    drop(conn);

    let account = account.ok_or_else(|| ApiError::not_found("No linked Bluesky account"))?;

    restore_agent(&oauth, &account.bluesky_did)
        .await
        .map_err(|e| {
            tracing::warn!("session restore failed for {actor_hex}: {e}");
            ApiError::unauthorized("Bluesky session expired — re-link required")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    /// The write site admits the canonical spelling and nothing else. The one
    /// production caller hands this helper an id split out of the OAuth
    /// callback's attacker-controlled `state`; a row stored under any other
    /// spelling names no actor any route can reach, and is exactly the row a
    /// spelling-sensitive gate fails to exclude (D7).
    #[tokio::test]
    async fn only_the_canonical_actor_spelling_is_ever_written() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn().await;
        crate::bluesky::apply_schema(&conn).expect("apply bluesky schema");

        let canonical = hex::encode([0xabu8; 32]);
        for bad in [
            canonical.to_uppercase(),
            hex::encode([0xabu8; 31]),
            hex::encode([0xabu8; 33]),
            format!("{}zz", &canonical[..62]),
            format!("{canonical}|/bridges"),
            String::new(),
        ] {
            assert!(
                upsert_linked_account(&conn, &bad, "did:plc:ext", "ext.bsky.social").is_err(),
                "a non-canonical actor id must be refused at the write site: {bad:?}"
            );
        }
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM bluesky_accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "no refused spelling may reach the table");

        upsert_linked_account(&conn, &canonical, "did:plc:ext", "ext.bsky.social")
            .expect("the canonical spelling is written");
        assert!(get_linked_account(&conn, &canonical).unwrap().is_some());
    }

    /// The reply derivation's root rule (`bridges.md` § Cross-posting → *A
    /// post that references a Bluesky record*): a parent that is itself a
    /// reply hands its own root down, so a Fauna reply deep in a Bluesky
    /// thread lands in that thread rather than starting a new one under the
    /// parent — the "future improvement" the retired nest-mint arm never made.
    #[test]
    fn a_reply_inherits_the_parents_thread_root() {
        let parent = serde_json::json!({
            "$type": "app.bsky.feed.post",
            "text": "mid-thread",
            "reply": {
                "root": { "uri": "at://did:plc:a/app.bsky.feed.post/root", "cid": "bafyroot" },
                "parent": { "uri": "at://did:plc:b/app.bsky.feed.post/gp", "cid": "bafygp" },
            },
        });
        assert_eq!(
            thread_root_of(&parent, "at://did:plc:c/app.bsky.feed.post/p", "bafyparent"),
            (
                "at://did:plc:a/app.bsky.feed.post/root".into(),
                "bafyroot".into()
            )
        );
    }

    /// A top-level parent starts the thread: it is its own root.
    #[test]
    fn a_top_level_parent_is_its_own_root() {
        let parent = serde_json::json!({ "$type": "app.bsky.feed.post", "text": "top" });
        assert_eq!(
            thread_root_of(&parent, "at://did:plc:c/app.bsky.feed.post/p", "bafyparent"),
            (
                "at://did:plc:c/app.bsky.feed.post/p".into(),
                "bafyparent".into()
            )
        );
        // A malformed root (present but empty) falls back the same way rather
        // than emitting a ref no repo serves.
        let broken = serde_json::json!({ "reply": { "root": { "uri": "", "cid": "" } } });
        assert_eq!(
            thread_root_of(&broken, "at://did:plc:c/app.bsky.feed.post/p", "bafyparent").0,
            "at://did:plc:c/app.bsky.feed.post/p"
        );
    }
}
