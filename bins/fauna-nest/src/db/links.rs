use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// A content link row.
#[derive(Debug, Clone)]
pub struct ContentLink {
    pub id: i64,
    pub link_type: String,
    pub source_id: Option<Vec<u8>>,
    pub target_id: Option<Vec<u8>>,
    pub actor_id: Option<Vec<u8>>,
    pub status: Option<String>,
    pub metadata: Option<Vec<u8>>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Insert a new link. Returns the autoincrement id.
#[allow(clippy::too_many_arguments)]
pub fn insert_link(
    conn: &Connection,
    link_type: &str,
    source_id: Option<&[u8]>,
    target_id: Option<&[u8]>,
    actor_id: Option<&[u8]>,
    status: Option<&str>,
    metadata: Option<&[u8]>,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO content_links (link_type, source_id, target_id, actor_id, status, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        rusqlite::params![link_type, source_id, target_id, actor_id, status, metadata, now],
    ).context("insert link")?;
    Ok(conn.last_insert_rowid())
}

/// Upsert a link — insert or update status/metadata if a matching unique link exists.
/// Matching is by (link_type, source_id, actor_id) for actor-scoped links,
/// or (link_type, source_id, target_id) for content-scoped links.
#[allow(clippy::too_many_arguments)]
pub fn upsert_link(
    conn: &Connection,
    link_type: &str,
    source_id: Option<&[u8]>,
    target_id: Option<&[u8]>,
    actor_id: Option<&[u8]>,
    status: Option<&str>,
    metadata: Option<&[u8]>,
    now: i64,
) -> Result<i64> {
    let existing_id: Option<i64> = if actor_id.is_some() {
        conn.query_row(
            "SELECT id FROM content_links WHERE link_type = ?1 AND source_id IS ?2 AND actor_id IS ?3",
            rusqlite::params![link_type, source_id, actor_id],
            |row| row.get(0),
        ).optional()?
    } else {
        conn.query_row(
            "SELECT id FROM content_links WHERE link_type = ?1 AND source_id IS ?2 AND target_id IS ?3",
            rusqlite::params![link_type, source_id, target_id],
            |row| row.get(0),
        ).optional()?
    };

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE content_links SET status = ?1, metadata = ?2, updated_at = ?3 WHERE id = ?4",
            rusqlite::params![status, metadata, now, id],
        )?;
        Ok(id)
    } else {
        insert_link(
            conn, link_type, source_id, target_id, actor_id, status, metadata, now,
        )
    }
}

/// Query links by source_id and link_type.
pub fn links_by_source(
    conn: &Connection,
    source_id: &[u8],
    link_type: &str,
) -> Result<Vec<ContentLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, link_type, source_id, target_id, actor_id, status, metadata, created_at, updated_at
         FROM content_links WHERE source_id = ?1 AND link_type = ?2
         ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, link_type], map_link)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Query links by actor_id and link_type.
pub fn links_by_actor(
    conn: &Connection,
    actor_id: &[u8],
    link_type: &str,
    limit: u32,
) -> Result<Vec<ContentLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, link_type, source_id, target_id, actor_id, status, metadata, created_at, updated_at
         FROM content_links WHERE actor_id = ?1 AND link_type = ?2
         ORDER BY created_at DESC LIMIT ?3"
    )?;
    let rows = stmt.query_map(rusqlite::params![actor_id, link_type, limit], map_link)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Delete a link by id.
pub fn delete_link(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute("DELETE FROM content_links WHERE id = ?1", [id])?;
    Ok(rows > 0)
}

/// Delete all links matching source_id and link_type.
pub fn delete_links_by_source(conn: &Connection, source_id: &[u8], link_type: &str) -> Result<u64> {
    let rows = conn.execute(
        "DELETE FROM content_links WHERE source_id = ?1 AND link_type = ?2",
        rusqlite::params![source_id, link_type],
    )?;
    Ok(rows as u64)
}

/// Count links matching source_id, link_type, and optionally a status filter.
pub fn count_links(
    conn: &Connection,
    source_id: &[u8],
    link_type: &str,
    status_filter: Option<&str>,
) -> Result<u64> {
    let count: i64 = if let Some(status) = status_filter {
        conn.query_row(
            "SELECT COUNT(*) FROM content_links WHERE source_id = ?1 AND link_type = ?2 AND status = ?3",
            rusqlite::params![source_id, link_type, status],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM content_links WHERE source_id = ?1 AND link_type = ?2",
            rusqlite::params![source_id, link_type],
            |row| row.get(0),
        )?
    };
    Ok(count as u64)
}

fn map_link(row: &rusqlite::Row) -> rusqlite::Result<ContentLink> {
    Ok(ContentLink {
        id: row.get(0)?,
        link_type: row.get(1)?,
        source_id: row.get(2)?,
        target_id: row.get(3)?,
        actor_id: row.get(4)?,
        status: row.get(5)?,
        metadata: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
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
    fn insert_and_query_link() {
        let conn = setup();
        let source = [1u8; 32];
        let actor = [2u8; 32];
        let id = insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&actor),
            Some("going"),
            None,
            1000,
        )
        .unwrap();
        assert!(id > 0);

        let links = links_by_source(&conn, &source, "attendee").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].status.as_deref(), Some("going"));
        assert_eq!(links[0].actor_id.as_deref(), Some(actor.as_slice()));
    }

    #[test]
    fn upsert_link_updates_existing() {
        let conn = setup();
        let source = [1u8; 32];
        let actor = [2u8; 32];

        let id1 = upsert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&actor),
            Some("invited"),
            None,
            1000,
        )
        .unwrap();
        let id2 = upsert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&actor),
            Some("going"),
            None,
            2000,
        )
        .unwrap();
        assert_eq!(id1, id2);

        let links = links_by_source(&conn, &source, "attendee").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].status.as_deref(), Some("going"));
        assert_eq!(links[0].updated_at, 2000);
    }

    #[test]
    fn unique_constraint_prevents_duplicate_attendees() {
        let conn = setup();
        let source = [1u8; 32];
        let actor = [2u8; 32];
        insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&actor),
            Some("going"),
            None,
            1000,
        )
        .unwrap();
        let result = insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&actor),
            Some("declined"),
            None,
            2000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn count_links_with_status_filter() {
        let conn = setup();
        let source = [1u8; 32];
        let a1 = [2u8; 32];
        let a2 = [3u8; 32];
        let a3 = [4u8; 32];
        insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&a1),
            Some("going"),
            None,
            1000,
        )
        .unwrap();
        insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&a2),
            Some("going"),
            None,
            1001,
        )
        .unwrap();
        insert_link(
            &conn,
            "attendee",
            Some(&source),
            None,
            Some(&a3),
            Some("declined"),
            None,
            1002,
        )
        .unwrap();

        assert_eq!(count_links(&conn, &source, "attendee", None).unwrap(), 3);
        assert_eq!(
            count_links(&conn, &source, "attendee", Some("going")).unwrap(),
            2
        );
    }

    #[test]
    fn delete_links_by_source_removes_all() {
        let conn = setup();
        let source = [1u8; 32];
        let a1 = [2u8; 32];
        let a2 = [3u8; 32];
        insert_link(
            &conn,
            "tag",
            Some(&source),
            None,
            None,
            Some("rust"),
            None,
            1000,
        )
        .unwrap();
        insert_link(
            &conn,
            "delivery",
            Some(&source),
            None,
            Some(&a1),
            Some("unread"),
            None,
            1000,
        )
        .unwrap();
        insert_link(
            &conn,
            "delivery",
            Some(&source),
            None,
            Some(&a2),
            Some("read"),
            None,
            1001,
        )
        .unwrap();

        let removed = delete_links_by_source(&conn, &source, "delivery").unwrap();
        assert_eq!(removed, 2);
        assert_eq!(count_links(&conn, &source, "tag", None).unwrap(), 1);
    }
}
