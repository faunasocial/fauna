use anyhow::Result;
use base64::Engine;
use rusqlite::Connection;
use std::io::Write;

/// Tables to dump. Content table excludes payload (only blob_hash).
///
/// # This list's NARROWNESS is a ratified confidentiality boundary
///
/// It is a **deliberate subset**, not an inventory that fell behind. The S9
/// path-sealing flip's self-backup ruling is
/// ratified with an explicitly bounded scope, and this list is what bounds it:
/// the hourly unencrypted hot-copies of `nest.db` carry pre-scrub plaintext for
/// up to 24 rotation cycles, **but the daily logical dump carries none at all**.
/// Both owner docs say so in those terms and rest the ruling on it:
///
/// - `docs/goal/behavior/path-sealing.md` § the S9 self-backup scope paragraph:
///   the dump "omits `folders`, `sync_changes`, `snapshots` and `snapshot_files`
///   entirely, so it carries no name plane at all".
/// - `docs/goal/architecture/encryption-at-rest.md` § Carve-outs: the dump "is
///   out of scope — `DUMP_TABLES` omits every name-plane table".
///
/// [`tests::the_dump_carries_no_sealed_plane`] is the standing witness for that
/// claim, and it **walks** the live schema rather than re-listing the four table
/// names, so it also catches the two ways a future change could break the ruling
/// without touching those names: a *new* sealed table added to this list, and a
/// *new* sealed column added to a table already on it.
///
/// # ⚠ Do NOT point this list at `ACTOR_TABLES` (or at `sqlite_master`)
///
/// `db/actor_tables.rs`'s module doc and `docs/goal/architecture/account-data-plane.md`
/// § Nest-side requirements item 1 both name "the logical dump still carries its
/// own hand list" as unclaimed W1 (account-data-plane.md § Workstreams) follow-on drift, and closing that drift the
/// obvious way — walk the registry, or walk every table — would ship a
/// **regression against the ratified ruling above**, because
/// `folders`/`sync_changes`/`snapshots`/`snapshot_files` are all actor-scoped and
/// would join the dump. The drift is real but the remedy is not a walk: any
/// convergence has to carry a per-table confidentiality disposition. The gate
/// below will red if this is attempted; that is deliberate.
///
/// Adding an ordinary (non-sealed) table here is fine and needs no ceremony.
pub const DUMP_TABLES: &[&str] = &[
    "admins",
    "users",
    "tiers",
    "audit_log",
    // The legal-takedown floor the box cannot recompute once a taken-down
    // post is deleted (`moderation.md` § Legal takedown → *Posts*, 2026-09-11):
    // the flag rides in `content_meta`, the order in `audit_log`, and this is
    // the third row a custody copy of the box must carry for a restore to
    // withhold what the box withheld (the admin whole-store export shares this
    // list — path 2 of the owner-scoped ruling). Post ids, citations and blob
    // digests only; nothing sealed.
    "legal_takedown_deleted_posts",
    "sessions",
    "content",
    "content_links",
    "content_meta",
    "content_fts_map",
    "nest_pairings",
    "outbox",
    "namespace_entries",
    "blob_metadata",
    "backup_snapshots",
    // operational tables from migrations:
    "contacts",
    "knocks",
    "feeds",
    "feed_contributors",
    "email_filters",
];

/// Append one `<prefix><table>.ndjson` entry per [`DUMP_TABLES`] name that this
/// connection actually has.
///
/// The single owner of "which tables the dump carries, and how each is
/// serialized" — both consumers (the daily logical dump below and the admin
/// nest-wide export in `admin_export_routes.rs`) go through here, so the
/// confidentiality contract documented on [`DUMP_TABLES`] has one enforcement
/// point rather than two hand-copied loops that can drift apart. `prefix` is the
/// only thing that ever differed between them.
pub fn append_dump_tables<W: Write>(
    conn: &Connection,
    tar_builder: &mut tar::Builder<W>,
    prefix: &str,
) -> Result<()> {
    for table_name in DUMP_TABLES {
        // Skip tables this DB doesn't have (e.g. in-memory test DBs).
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table_name],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !exists {
            continue;
        }

        let ndjson = dump_table(conn, table_name)?;
        let data = ndjson.as_bytes();
        let filename = format!("{prefix}{table_name}.ndjson");

        super::archive::append_entry(tar_builder, &filename, data)?;
    }
    Ok(())
}

pub fn write_logical_dump(conn: &Connection, writer: impl Write) -> Result<()> {
    let encoder = zstd::stream::Encoder::new(writer, 3)?;
    let mut tar_builder = tar::Builder::new(encoder);

    append_dump_tables(conn, &mut tar_builder, "")?;

    super::archive::finish(tar_builder)
}

fn dump_table(conn: &Connection, table_name: &str) -> Result<String> {
    // For the content table, exclude payload column
    let sql = if table_name == "content" {
        "SELECT id, schema, author, created_at, expires_at, source, blob_hash FROM content"
            .to_string()
    } else {
        format!("SELECT * FROM {table_name}")
    };

    let mut stmt = conn.prepare(&sql)?;
    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap().to_string())
        .collect();

    let mut output = String::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let mut obj = serde_json::Map::new();
        for (i, name) in col_names.iter().enumerate() {
            let val = row_value_to_json(row, i);
            obj.insert(name.clone(), val);
        }
        let json_str = serde_json::to_string(&serde_json::Value::Object(obj))?;
        output.push_str(&json_str);
        output.push('\n');
    }
    Ok(output)
}

fn row_value_to_json(row: &rusqlite::Row, idx: usize) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(idx).unwrap_or(ValueRef::Null) {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::json!(f),
        ValueRef::Text(s) => {
            serde_json::Value::String(std::str::from_utf8(s).unwrap_or("").to_string())
        }
        ValueRef::Blob(b) => {
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_dump_produces_valid_tar_zst() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let conn = db.conn_blocking();

        let mut output = Vec::new();
        write_logical_dump(&conn, &mut output).unwrap();
        assert!(!output.is_empty());

        // Decompress and verify tar entries
        let decoder = zstd::stream::Decoder::new(output.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let entries: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(entries.contains(&"content.ndjson".to_string()));
        assert!(entries.contains(&"users.ndjson".to_string()));
        assert!(entries.contains(&"tiers.ndjson".to_string()));
    }

    /// Every table carrying a sealed plane (`sealed` / `*_sealed` column) is
    /// absent from [`DUMP_TABLES`] — the standing witness for the ratified S9
    /// self-backup scope bound documented on that constant.
    ///
    /// This **walks the live schema** instead of re-listing the four table names
    /// the goal docs quote, which is the whole point: a guard that lists is only
    /// as complete as the sweep that wrote it. Two breakages the four names
    /// would miss and this catches — a *new* sealed table added to `DUMP_TABLES`,
    /// and a *new* sealed column grown on a table already dumped.
    #[test]
    fn the_dump_carries_no_sealed_plane() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let conn = db.conn_blocking();

        let dumped: std::collections::HashSet<&str> = DUMP_TABLES.iter().copied().collect();

        let mut tables: Vec<String> = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                tables.push(row.get::<_, String>(0).unwrap());
            }
        }
        // The walk is only a witness if it actually saw the schema.
        assert!(
            tables.len() > 50,
            "schema walk saw only {} tables — the gate would pass vacuously",
            tables.len()
        );

        let mut offenders: Vec<String> = Vec::new();
        let mut sealed_tables_seen = 0usize;
        for table in &tables {
            // `PRAGMA table_info` takes no bind parameter for the table name.
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap_or_else(|e| panic!("table_info({table}): {e}"));
            let mut rows = stmt.query([]).unwrap();
            let mut sealed_cols: Vec<String> = Vec::new();
            while let Some(row) = rows.next().unwrap() {
                let col: String = row.get(1).unwrap();
                // Three spellings, all live in `db/migrations.rs`: the suffix
                // form (`path_sealed`, `name_sealed`, `tags_sealed`, …), the
                // bare column (`nest_content_keys.sealed`), and the *prefix*
                // form (`spam_training_history.sealed_subject`,
                // `*.sealed_blob`) — which a suffix-only rule silently misses.
                if col == "sealed" || col.ends_with("_sealed") || col.starts_with("sealed_") {
                    sealed_cols.push(col);
                }
            }
            if sealed_cols.is_empty() {
                continue;
            }
            sealed_tables_seen += 1;
            if dumped.contains(table.as_str()) {
                offenders.push(format!(
                    "{table} (sealed columns: {})",
                    sealed_cols.join(", ")
                ));
            }
        }

        // If the schema grew no sealed columns at all the walk proves nothing.
        assert!(
            sealed_tables_seen >= 4,
            "walk found only {sealed_tables_seen} sealed-plane tables — expected at least the \
             four the ruling names (folders, sync_changes, snapshots, snapshot_files); the \
             detector is broken, not the schema"
        );

        assert!(
            offenders.is_empty(),
            "DUMP_TABLES carries a sealed plane, breaking the ratified S9 self-backup scope bound \
             (path-sealing.md § the self-backup scope paragraph; encryption-at-rest.md § \
             Carve-outs — 'the logical dump carries no name plane at all'): {}\n\
             The daily logical dump is written UNENCRYPTED-at-source and rests in the self-backup \
             store; a sealed plane must not enter it. See the DUMP_TABLES doc comment.",
            offenders.join("; ")
        );
    }

    #[test]
    fn content_dump_excludes_payload() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let conn = db.conn_blocking();

        // Insert a content row
        conn.execute(
            "INSERT INTO content (id, schema, author, created_at, payload, source) VALUES (?1, 'test', ?2, 1000, ?3, 'fauna')",
            rusqlite::params![[1u8; 32].as_slice(), [2u8; 32].as_slice(), b"secret payload"],
        ).unwrap();

        let ndjson = dump_table(&conn, "content").unwrap();
        // Should NOT contain the actual payload bytes
        assert!(!ndjson.contains("secret payload"));
        // Should contain schema and other metadata
        assert!(ndjson.contains("test"));
    }
}
