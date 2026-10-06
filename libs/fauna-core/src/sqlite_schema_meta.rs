//! Shared SQLite `schema_meta` single-row persistence for the two-number
//! at-rest schema-version scheme (`version-compatibility.md` § 2.2) — the
//! exact SQL every native SQLite-backed store composing the scheme shares
//! (`fauna-mls`'s `mls.db`, the nest's cache DB).
//!
//! Each store keeps its own version constants and `SchemaVerdict`/
//! `check_schema_compatibility` independently mirrored (each store's own
//! `version.rs`/`migrations.rs` module doc says so explicitly — a deliberate
//! design, not a gap this module reopens). Only the SQL persistence — the DDL
//! and the read/write of the single row — was an unreviewed duplicate; this
//! module is that one shared home.

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};

/// The single-row `schema_meta` table DDL. `CREATE TABLE IF NOT EXISTS`, so a
/// store's open can run it unconditionally (a `PRAGMA user_version` holds only
/// one 32-bit int, hence a table).
pub const SCHEMA_META_DDL: &str = "
    CREATE TABLE IF NOT EXISTS schema_meta (
        id                 INTEGER PRIMARY KEY CHECK (id = 1),
        schema_version     INTEGER NOT NULL,
        min_reader_version INTEGER NOT NULL,
        updated_at         INTEGER NOT NULL
    );
";

/// Read the recorded `(schema_version, min_reader_version)`. An absent
/// `schema_meta` table or an absent row both read as `(baseline, baseline)` —
/// the reading of a FRESH database, which every caller checks before its first
/// write. A non-empty database carrying no stamp is not this function's call:
/// each store refuses it at open itself (the nest's genesis check, `mls.db`'s
/// pre-scheme refusal).
pub fn read_schema_meta(conn: &Connection, baseline: u32) -> anyhow::Result<(u32, u32)> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);
    if !table_exists {
        return Ok((baseline, baseline));
    }
    let row = conn
        .query_row(
            "SELECT schema_version, min_reader_version FROM schema_meta WHERE id = 1",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32)),
        )
        .optional()?;
    Ok(row.unwrap_or((baseline, baseline)))
}

/// Stamp `(schema_version, min_reader_version, now_secs)` into the single
/// row. The `WHERE` refuses to lower an existing `schema_version`, so an
/// older build operating a newer-but-compatible database does not restamp it
/// *down* and lie to the newer build that wrote it; `min_reader_version`
/// rides along only when the version stamp does, so the breaking-change
/// floor is never lowered either.
pub fn record_schema_meta(
    conn: &Connection,
    schema_version: u32,
    min_reader_version: u32,
    now_secs: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO schema_meta (id, schema_version, min_reader_version, updated_at) \
         VALUES (1, ?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET \
            schema_version = excluded.schema_version, \
            min_reader_version = excluded.min_reader_version, \
            updated_at = excluded.updated_at \
         WHERE excluded.schema_version >= schema_meta.schema_version",
        rusqlite::params![schema_version as i64, min_reader_version as i64, now_secs],
    )?;
    Ok(())
}

/// A column as reported by `PRAGMA table_info`, reduced to the parts that decide
/// whether and how it can be added with `ALTER TABLE ... ADD COLUMN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    /// Declared type (may be empty for a type-less column).
    pub ty: String,
    pub notnull: bool,
    /// SQL text of the column default exactly as SQLite stores it (already a
    /// valid literal), or `None` for no default.
    pub dflt: Option<String>,
}

/// Real, non-virtual base tables in `conn` — excludes views, FTS5 virtual tables
/// and their shadow tables, and SQLite-internal tables. `PRAGMA table_list`
/// reports shadow tables as type `'shadow'` and virtual tables as `'virtual'`, so
/// filtering to `'table'` leaves exactly the tables that accept `ALTER`.
pub fn managed_tables(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM pragma_table_list \
         WHERE schema = 'main' AND type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    let tables = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(tables)
}

/// Columns of `table` in declaration order, reduced to the parts that decide
/// whether and how a column can be added with `ALTER TABLE ... ADD COLUMN`.
pub fn column_defs(conn: &Connection, table: &str) -> anyhow::Result<Vec<ColumnDef>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let cols = stmt
        .query_map([], |r| {
            Ok(ColumnDef {
                name: r.get::<_, String>(1)?,
                ty: r.get::<_, String>(2)?,
                notnull: r.get::<_, i64>(3)? != 0,
                dflt: r.get::<_, Option<String>>(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// Backstop for the outage class: without this, growing a table by
/// one column breaks every existing database in the field — not at open, but
/// *lazily at the first write*. After every explicit migration has run, diff
/// each managed table against a freshly-built reference schema and
/// `ALTER TABLE ... ADD COLUMN` any column the current schema declares but this
/// (older) database lacks. `build_reference` populates a throwaway in-memory
/// reference database — each caller passes its own schema-construction entry
/// point, so the `CREATE TABLE` blocks stay that caller's single source of
/// truth and no parallel hand-maintained column list can drift from it.
///
/// Scope is exactly SQLite's additive `ADD COLUMN` class: nullable columns and
/// constant-default columns. A column that is `NOT NULL` without a default cannot
/// be added this way — that change needs an explicit table-rebuild migration, so
/// it is surfaced as a hard error here rather than silently skipped. Columns the
/// database has but the schema no longer declares are left untouched (never
/// dropped).
pub fn reconcile_added_columns(
    conn: &Connection,
    build_reference: impl FnOnce(&Connection) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let reference = Connection::open_in_memory().context("open reconcile reference db")?;
    build_reference(&reference).context("build reference schema for column reconcile")?;

    for table in managed_tables(&reference)? {
        let have: std::collections::HashSet<String> = column_defs(conn, &table)?
            .into_iter()
            .map(|c| c.name)
            .collect();
        for col in column_defs(&reference, &table)? {
            if have.contains(&col.name) {
                continue;
            }
            if col.notnull && col.dflt.is_none() {
                anyhow::bail!(
                    "schema column `{table}.{}` is missing from this database and is \
                     NOT NULL without a default, so it cannot be added automatically — \
                     it needs an explicit table-rebuild migration",
                    col.name
                );
            }
            let mut sql = format!("ALTER TABLE \"{table}\" ADD COLUMN \"{}\"", col.name);
            if !col.ty.is_empty() {
                sql.push(' ');
                sql.push_str(&col.ty);
            }
            if col.notnull {
                sql.push_str(" NOT NULL");
            }
            if let Some(dflt) = &col.dflt {
                sql.push_str(" DEFAULT ");
                sql.push_str(dflt);
            }
            conn.execute_batch(&sql)
                .with_context(|| format!("reconcile-add missing column: {sql}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        Connection::open_in_memory().expect("open")
    }

    #[test]
    fn absent_table_reads_as_the_baseline() {
        let c = conn();
        assert_eq!(read_schema_meta(&c, 7).expect("read"), (7, 7));
    }

    #[test]
    fn absent_row_in_a_present_table_reads_as_the_baseline() {
        let c = conn();
        c.execute_batch(SCHEMA_META_DDL).expect("ddl");
        assert_eq!(read_schema_meta(&c, 3).expect("read"), (3, 3));
    }

    #[test]
    fn a_recorded_stamp_reads_back_exactly() {
        let c = conn();
        c.execute_batch(SCHEMA_META_DDL).expect("ddl");
        record_schema_meta(&c, 5, 2, 1_700_000_000).expect("record");
        assert_eq!(read_schema_meta(&c, 1).expect("read"), (5, 2));
    }

    /// The load-bearing guard: a lower `schema_version` must never overwrite a
    /// higher one already recorded — an older build reading a newer-but-
    /// compatible database must not restamp it down and lie to the newer
    /// build that wrote it.
    #[test]
    fn recording_a_lower_schema_version_does_not_overwrite_a_higher_one() {
        let c = conn();
        c.execute_batch(SCHEMA_META_DDL).expect("ddl");
        record_schema_meta(&c, 9, 4, 100).expect("record v9");
        record_schema_meta(&c, 3, 1, 200).expect("record v3, refused");
        assert_eq!(
            read_schema_meta(&c, 0).expect("read"),
            (9, 4),
            "the higher stamp must survive the lower build's write attempt"
        );
    }

    /// `min_reader_version` rides along only when the version stamp does —
    /// perturbing this guard's inequality (e.g. to `>` or dropping it)
    /// would let a lower `min_reader_version` slip through even while the
    /// `schema_version` guard holds; the equal-version case is what proves
    /// the two columns move together, not independently.
    #[test]
    fn an_equal_schema_version_still_updates_min_reader_version() {
        let c = conn();
        c.execute_batch(SCHEMA_META_DDL).expect("ddl");
        record_schema_meta(&c, 5, 5, 100).expect("record");
        record_schema_meta(&c, 5, 2, 200).expect("record, same version, lower min_reader");
        assert_eq!(
            read_schema_meta(&c, 0).expect("read"),
            (5, 2),
            "an equal schema_version is not a lower one — the WHERE's `>=` must let it through"
        );
    }

    #[test]
    fn reconcile_adds_a_missing_nullable_and_a_missing_defaulted_column() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .expect("old ddl");
        reconcile_added_columns(&c, |reference| {
            reference.execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT, flag INTEGER NOT NULL DEFAULT 0)",
            )?;
            Ok(())
        })
        .expect("reconcile");
        let cols: std::collections::HashSet<String> = column_defs(&c, "t")
            .expect("column_defs")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(cols.contains("note"), "nullable column must be added");
        assert!(
            cols.contains("flag"),
            "NOT NULL column with a default must be added"
        );
    }

    /// The load-bearing refusal: a NOT NULL column with no default cannot be
    /// added via `ALTER TABLE ... ADD COLUMN` (SQLite itself refuses it), so
    /// this must fail loudly rather than leave the database half-migrated.
    #[test]
    fn reconcile_refuses_a_missing_not_null_column_with_no_default() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .expect("old ddl");
        let err = reconcile_added_columns(&c, |reference| {
            reference
                .execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, required TEXT NOT NULL)")?;
            Ok(())
        })
        .expect_err("must refuse a NOT NULL column with no default");
        assert!(
            err.to_string().contains("required"),
            "error must name the offending column: {err}"
        );
    }

    /// A column the database already has but the reference no longer declares
    /// is left untouched — never dropped (I1).
    #[test]
    fn reconcile_leaves_an_undeclared_existing_column_untouched() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, legacy TEXT)")
            .expect("old ddl");
        reconcile_added_columns(&c, |reference| {
            reference.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")?;
            Ok(())
        })
        .expect("reconcile");
        let cols: std::collections::HashSet<String> = column_defs(&c, "t")
            .expect("column_defs")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(
            cols.contains("legacy"),
            "a column the reference no longer declares must not be dropped"
        );
    }

    /// Idempotent: reconciling twice against the same reference is a no-op the
    /// second time, not a duplicate-column error.
    #[test]
    fn reconcile_is_idempotent() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .expect("old ddl");
        let build = |reference: &Connection| -> anyhow::Result<()> {
            reference.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT)")?;
            Ok(())
        };
        reconcile_added_columns(&c, build).expect("first reconcile");
        reconcile_added_columns(&c, build).expect("second reconcile must not error");
    }

    #[test]
    fn managed_tables_excludes_sqlite_internal_tables() {
        let c = conn();
        c.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY); CREATE VIEW v AS SELECT id FROM t;",
        )
        .expect("ddl");
        let tables = managed_tables(&c).expect("managed_tables");
        assert_eq!(
            tables,
            vec!["t".to_string()],
            "views and sqlite_% tables must be excluded"
        );
    }
}
