//! The one shape every federation bridge's `nest.db` schema is applied in
//! ([`crate::nostr::apply_schema`], [`crate::bluesky::apply_schema`],
//! [`crate::activitypub::apply_schema`]): the bridge's `CREATE_TABLES_SQL` is
//! its **genesis** — the tables at their current shape, with no step written
//! for a database predating the nest's genesis (`nest/common.md` § Database;
//! such a `nest.db` is refused at open before a bridge schema is applied) —
//! paired with the additive column reconciler the nest's own genesis runs.
//!
//! Growing a bridge table by a nullable or constant-default column therefore
//! needs no hand-written `ALTER`: add the column to the block, and the
//! reconciler diffs each table against a throwaway built from the block and
//! adds what a long-lived database lacks. A change outside that additive class
//! (a `NOT NULL` column without a default, a type change, a drop) is refused
//! by the reconciler and needs an explicit expand→migrate→contract step.

use rusqlite::Connection;

/// Apply a bridge's genesis block, then reconcile every column the block
/// declares that this database lacks. Idempotent: the `CREATE`s are
/// `IF NOT EXISTS` and a reconcile over a current database adds nothing.
pub(crate) fn apply_genesis(conn: &Connection, create_tables_sql: &str) -> anyhow::Result<()> {
    conn.execute_batch(create_tables_sql)?;
    fauna_core::sqlite_schema_meta::reconcile_added_columns(conn, |reference| {
        Ok(reference.execute_batch(create_tables_sql)?)
    })
}

/// The pin each bridge runs over its own `apply_schema`: on `conn` (already
/// carrying the bridge's schema), drop every nullable or defaulted column
/// SQLite lets go of (not a key, not indexed) across every table the genesis
/// block declares, re-run `apply_schema`, and assert each table's shape comes
/// back whole. Returns how many columns the probe dropped, so the caller can
/// assert the probe actually bit.
#[cfg(test)]
pub(crate) fn drop_additive_columns_and_reapply(
    conn: &Connection,
    create_tables_sql: &str,
    apply_schema: impl Fn(&Connection) -> anyhow::Result<()>,
) -> usize {
    use fauna_core::sqlite_schema_meta::{column_defs, managed_tables};
    let reference = Connection::open_in_memory().unwrap();
    reference.execute_batch(create_tables_sql).unwrap();
    let shape = |c: &Connection, t: &str| -> Vec<(String, String, bool, Option<String>)> {
        let mut cols: Vec<_> = column_defs(c, t)
            .unwrap()
            .into_iter()
            .map(|d| (d.name, d.ty, d.notnull, d.dflt))
            .collect();
        cols.sort();
        cols
    };
    let mut dropped = 0;
    for table in managed_tables(&reference).unwrap() {
        for col in column_defs(&reference, &table).unwrap() {
            // The additive class only: a `NOT NULL` column without a
            // default is a table rebuild, which the reconciler refuses.
            if col.notnull && col.dflt.is_none() {
                continue;
            }
            if conn
                .execute_batch(&format!(
                    "ALTER TABLE \"{table}\" DROP COLUMN \"{}\"",
                    col.name
                ))
                .is_ok()
            {
                dropped += 1;
            }
        }
    }
    apply_schema(conn).unwrap();
    for table in managed_tables(&reference).unwrap() {
        assert_eq!(
            shape(conn, &table),
            shape(&reference, &table),
            "`{table}` is reconciled back to the genesis shape"
        );
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENESIS: &str =
        "CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, note TEXT, flag INTEGER DEFAULT 0);";

    #[test]
    fn applies_the_genesis_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn, GENESIS).unwrap();
        apply_genesis(&conn, GENESIS).unwrap();
        conn.execute("INSERT INTO t (id, note, flag) VALUES (1, 'hi', 1)", [])
            .unwrap();
    }

    /// The mechanism that replaced every bridge's hand-written `ALTER`: a
    /// long-lived table lacking a column the block now declares gains it.
    #[test]
    fn adds_a_column_the_block_declares_and_the_database_lacks() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        let dropped =
            drop_additive_columns_and_reapply(&conn, GENESIS, |c| apply_genesis(c, GENESIS));
        assert_eq!(dropped, 0, "the old table had nothing left to drop");
        conn.execute("INSERT INTO t (id, note, flag) VALUES (1, 'hi', 1)", [])
            .unwrap();
    }

    #[test]
    fn a_genesis_error_propagates() {
        let conn = Connection::open_in_memory().unwrap();
        apply_genesis(&conn, "CREATE TABLE broken (").expect_err("a malformed genesis must fail");
    }
}
