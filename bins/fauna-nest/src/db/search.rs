use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// Index a content item for full-text search.
/// Uses content_fts_map to track the FTS5 rowid for reliable deletes.
///
/// `created_at` (epoch seconds) is the recency stamp the per-bridge newest-N
/// cap evicts on, and the fallback ordering key for rows that have no `content`
/// row to join. Callers that genuinely have no timestamp pass `0`.
#[allow(clippy::too_many_arguments)]
pub fn index_content(
    conn: &Connection,
    content_id: &[u8; 32],
    schema: &str,
    title: &str,
    body: &str,
    author_name: &str,
    tags: &str,
    created_at: i64,
) -> Result<()> {
    // Remove existing entry if present
    remove_content(conn, content_id)?;

    conn.execute(
        "INSERT INTO content_fts (title, body, author_name, tags, schema)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![title, body, author_name, tags, schema],
    )
    .context("index content FTS")?;

    let rowid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO content_fts_map (content_id, fts_rowid, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![content_id.as_slice(), rowid, created_at],
    )
    .context("insert FTS map")?;
    Ok(())
}

/// Delete every indexed row whose `schema` equals `schema` — the
/// purge-on-toggle-off arm of the per-bridge search policy
/// (`content-index.md` § Bridge content in the Search corpus). Returns how many
/// rows went.
pub fn purge_schema(conn: &Connection, schema: &str) -> Result<usize> {
    let rowids = rowids_for_schema(conn, schema, 0)?;
    delete_by_rowids(conn, &rowids)
}

/// Trim the rows of `schema` down to the newest `keep`, deleting the rest —
/// the newest-N eviction the cap enforces at ingest, and the prune a
/// cap-lowering setting write performs. Returns how many rows went.
pub fn trim_schema_to_newest(conn: &Connection, schema: &str, keep: u32) -> Result<usize> {
    let rowids = rowids_for_schema(conn, schema, keep)?;
    delete_by_rowids(conn, &rowids)
}

/// The `(content_id, fts_rowid)` pairs of `schema`, oldest first, skipping the
/// newest `keep`. `keep = 0` returns every row.
///
/// Ties on `created_at` break on `fts_rowid` — insertion order — so a burst of
/// same-second inserts still evicts in a stable, genuinely-oldest-first order
/// instead of an arbitrary one.
fn rowids_for_schema(conn: &Connection, schema: &str, keep: u32) -> Result<Vec<(Vec<u8>, i64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT m.content_id, m.fts_rowid \
             FROM content_fts_map m \
             JOIN content_fts f ON f.rowid = m.fts_rowid \
             WHERE f.schema = ?1 \
             ORDER BY m.created_at DESC, m.fts_rowid DESC \
             LIMIT -1 OFFSET ?2",
        )
        .context("prepare schema row scan")?;
    let rows = stmt
        .query_map(rusqlite::params![schema, keep], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
        })
        .context("scan schema rows")?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn delete_by_rowids(conn: &Connection, rows: &[(Vec<u8>, i64)]) -> Result<usize> {
    for (content_id, fts_rowid) in rows {
        conn.execute("DELETE FROM content_fts WHERE rowid = ?1", [fts_rowid])
            .context("delete from FTS")?;
        conn.execute(
            "DELETE FROM content_fts_map WHERE content_id = ?1",
            rusqlite::params![content_id.as_slice()],
        )?;
    }
    Ok(rows.len())
}

/// Remove a content item from the FTS index.
/// Looks up the FTS5 rowid via content_fts_map, deletes by rowid.
pub fn remove_content(conn: &Connection, content_id: &[u8; 32]) -> Result<()> {
    let rowid: Option<i64> = conn
        .query_row(
            "SELECT fts_rowid FROM content_fts_map WHERE content_id = ?1",
            rusqlite::params![content_id.as_slice()],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(rowid) = rowid {
        conn.execute("DELETE FROM content_fts WHERE rowid = ?1", [rowid])
            .context("delete from FTS")?;
        conn.execute(
            "DELETE FROM content_fts_map WHERE content_id = ?1",
            rusqlite::params![content_id.as_slice()],
        )?;
    }
    Ok(())
}

/// Full-text search result.
#[derive(Debug)]
pub struct FtsResult {
    pub content_id: Vec<u8>,
    pub schema: String,
    pub rank: f64,
}

/// Search across all content types.
/// Joins content_fts with content_fts_map to return content IDs.
pub fn search(
    conn: &Connection,
    query: &str,
    schema_filter: Option<&str>,
    limit: u32,
) -> Result<Vec<FtsResult>> {
    let mut sql = String::from(
        "SELECT m.content_id, f.schema, f.rank
         FROM content_fts f
         JOIN content_fts_map m ON m.fts_rowid = f.rowid
         WHERE content_fts MATCH ?1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(query.to_string())];
    let mut idx = 2;

    if let Some(schema) = schema_filter {
        sql.push_str(&format!(" AND f.schema = ?{idx}"));
        params.push(Box::new(schema.to_string()));
        idx += 1;
    }
    sql.push_str(&format!(" ORDER BY f.rank LIMIT ?{idx}"));
    params.push(Box::new(limit));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(FtsResult {
            content_id: row.get(0)?,
            schema: row.get(1)?,
            rank: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::apply_unified_schema;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_unified_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn index_and_search_content() {
        let conn = setup();
        let id = [1u8; 32];
        index_content(
            &conn,
            &id,
            "post/text",
            "",
            "hello world",
            "",
            "rust programming",
            0,
        )
        .unwrap();

        let results = search(&conn, "hello", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].schema, "post/text");
        assert_eq!(results[0].content_id, id.to_vec());
    }

    #[test]
    fn search_with_schema_filter() {
        let conn = setup();
        let id1 = [1u8; 32];
        let id2 = [2u8; 32];
        index_content(&conn, &id1, "post/text", "", "hello", "", "", 0).unwrap();
        index_content(&conn, &id2, "email/v1", "hello subject", "", "", "", 0).unwrap();

        let all = search(&conn, "hello", None, 10).unwrap();
        assert_eq!(all.len(), 2);

        let posts_only = search(&conn, "hello", Some("post/text"), 10).unwrap();
        assert_eq!(posts_only.len(), 1);
    }

    #[test]
    fn remove_content_from_fts() {
        let conn = setup();
        let id = [1u8; 32];
        index_content(&conn, &id, "post/text", "", "searchable text", "", "", 0).unwrap();
        remove_content(&conn, &id).unwrap();

        let results = search(&conn, "searchable", None, 10).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn reindex_replaces_existing() {
        let conn = setup();
        let id = [1u8; 32];
        index_content(&conn, &id, "post/text", "", "original text", "", "", 0).unwrap();
        index_content(&conn, &id, "post/text", "", "updated text", "", "", 0).unwrap();

        let results = search(&conn, "original", None, 10).unwrap();
        assert_eq!(results.len(), 0);
        let results = search(&conn, "updated", None, 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
