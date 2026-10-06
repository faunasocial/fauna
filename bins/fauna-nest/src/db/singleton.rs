//! The `WHERE id = 1` **singleton-row** primitives — one home for the
//! deployment-wide-setting convention the rest of `db/` is built on.
//!
//! A singleton row is how nest stores an admin's *one* deployment-wide choice
//! (`caldav_port`, `mail_enabled`, the declared region, the CORS list, …): a
//! table whose only row is `id = 1`, read with `SELECT <column> … WHERE id = 1`
//! and written with `INSERT … VALUES (1, …) ON CONFLICT(id) DO UPDATE`. The
//! upsert is the *single atomic decision point* the crash-safety invariant asks
//! for (`nest/common.md` § Client-state recoverability): a crash before or after
//! it leaves a coherent value, never a half-state. An **absent** row means "the
//! admin has never chosen", which every caller folds to its own default — so the
//! read hands back `Option` and the fold stays in the typed wrapper, where the
//! reason for that particular default is documented.
//!
//! Two shapes, both of them here:
//!
//! * **Scalar + `set_at`** — [`CacheDb::get_singleton_column`] /
//!   [`CacheDb::set_singleton_column`], generic over the stored SQL type, so
//!   one pair serves the `INTEGER` toggles and ports, the `TEXT` wire-enums and
//!   JSON lists, the `BLOB` actor id, and the nullable column whose `NULL` is a
//!   meaningful "explicitly cleared" (read it as `Option<Option<_>>`).
//! * **Whole-blob JSON overrides** — [`CacheDb::read_singleton_json`] /
//!   [`CacheDb::write_singleton_json`] for the `overrides_json` policy tables,
//!   where a missing row and a missing field both decode to `None` ⇒ "use the
//!   catalog default".
//!
//! **These are plumbing, not policy.** Each singleton keeps its own module and
//! its own `pub async fn get_X`/`set_X` wrapper: that wrapper owns the value's
//! type (`bool`, `u16`, a parsed enum), its coercion, and the doc comment
//! explaining what the setting means and where its default lives. Only the
//! ceremony — lock, format the fixed SQL, stamp `set_at`, attach context — is
//! shared.
//!
//! ## What deliberately does *not* belong here
//!
//! The rest of `db/` holds several near-neighbours that look like this family
//! from a grep but are a different mechanism. They were each read and ruled out;
//! don't fold them in without re-deciding, because in every case the difference
//! is load-bearing:
//!
//! * **`INSERT OR REPLACE` rows** — the VAPID keypair (`db/push.rs`) and the
//!   deployment keypair (`db/subscriptions.rs`). Replace is delete-then-insert,
//!   not update-in-place: it fires delete triggers and resets any column the
//!   statement omits. Rewriting them as `ON CONFLICT DO UPDATE` would be an
//!   at-rest behavior change, not a refactor.
//! * **Multi-column records** — `db/domain_expiry.rs`, `db/nest_host_address.rs`,
//!   the registration posture in `db/node_policy.rs`, the spam baseline in
//!   `db/moderation.rs` (which is keyed `id = 0`). These write a whole record in
//!   one upsert; a per-column helper would need one call per field and lose the
//!   atomicity that is the point.
//! * **Stateful counter rows** — `db/mail_warmup.rs` seeds with
//!   `INSERT OR IGNORE` and then mutates in place with `UPDATE`, because its
//!   value is accumulated rather than chosen.
//! * **Transaction-scoped reads.** A caller already inside a `tx` (the identity
//!   rotation in `db/nest_rotation.rs`) reads its singleton through that
//!   transaction. These helpers take the connection mutex themselves, so they
//!   cannot serve — and must not be made to serve — a caller that already holds
//!   it. A *write* that must land in the same transaction as something else
//!   uses the free [`set_singleton_column_in`] instead, which takes the open
//!   transaction rather than the mutex — the `nest_region` declaration write
//!   carries the owed-render mark that way
//!   (`web-content-hosting.md` § Routing, render, serving → *A revoke is
//!   durable*).

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use rusqlite::types::{FromSql, ToSql};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::CacheDb;

impl CacheDb {
    /// Read `column` from a table's single `id = 1` row, or `None` when the row
    /// has never been written.
    ///
    /// `T` is the column's SQL type as Rust sees it — `i64` for the `INTEGER`
    /// toggles/ports, `String` for a wire-enum or JSON column, `Vec<u8>` for a
    /// `BLOB`. For a **nullable** column whose `NULL` is meaningful, read
    /// `Option<i64>`: the outer `Option` is then row presence and the inner one
    /// the stored `NULL`.
    ///
    /// `table` and `column` are fixed `&'static str`s from the typed wrappers
    /// (never user input) — safe to interpolate into the SQL.
    pub(super) async fn get_singleton_column<T: FromSql>(
        &self,
        table: &'static str,
        column: &'static str,
    ) -> Result<Option<T>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = 1"),
            [],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("get {table}"))
    }

    /// Upsert `value` into a table's single `id = 1` row, stamping `set_at` with
    /// the current epoch second. The first write inserts, every later one
    /// overwrites — the latest write wins.
    ///
    /// `table` and `column` are fixed `&'static str`s from the typed wrappers
    /// (never user input) — safe to interpolate into the SQL.
    pub(super) async fn set_singleton_column<T: ToSql>(
        &self,
        table: &'static str,
        column: &'static str,
        value: T,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        set_singleton_column_in(&conn, table, column, value)
    }

    /// Read the single JSON-blob `overrides_json` row from `table`, or the
    /// all-`None` default when no row has been written yet.
    ///
    /// `table` is a fixed `&'static str` from the typed wrappers below (never
    /// user input) — safe to interpolate into the SQL.
    pub(super) async fn read_singleton_json<T: DeserializeOwned + Default>(
        &self,
        table: &'static str,
    ) -> Result<T> {
        let conn = self.conn.lock().await;
        let json: Option<String> = conn
            .query_row(
                &format!("SELECT overrides_json FROM {table} WHERE id = 1"),
                [],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("read {table} row"))?;
        match json {
            Some(s) => {
                serde_json::from_str(&s).with_context(|| format!("decode {table} overrides JSON"))
            }
            None => Ok(T::default()),
        }
    }

    /// Upsert the single JSON-blob `overrides_json` row into `table`. Idempotent
    /// on the fixed `id = 1` row; replaces the whole blob (the admin form
    /// submits the complete sub-struct override, so this is a PUT not a merge).
    ///
    /// `table` is a fixed `&'static str` from the typed wrappers below (never
    /// user input) — safe to interpolate into the SQL.
    pub(super) async fn write_singleton_json<T: Serialize>(
        &self,
        table: &'static str,
        overrides: &T,
    ) -> Result<()> {
        let json = serde_json::to_string(overrides)
            .with_context(|| format!("encode {table} overrides"))?;
        let conn = self.conn.lock().await;
        conn.execute(
            &format!(
                "INSERT INTO {table} (id, overrides_json) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET overrides_json = excluded.overrides_json"
            ),
            rusqlite::params![json],
        )
        .with_context(|| format!("upsert {table} row"))?;
        Ok(())
    }
}

/// [`CacheDb::set_singleton_column`] over an **already-open** connection or
/// transaction — the one form that can serve a caller which is mid-transaction
/// and must land the singleton write together with something else. The `&self`
/// helpers above take the connection mutex themselves and deliberately cannot
/// (see the module note).
///
/// `table` and `column` are fixed `&'static str`s from the typed wrappers
/// (never user input) — safe to interpolate into the SQL.
pub(super) fn set_singleton_column_in<T: ToSql>(
    conn: &rusqlite::Connection,
    table: &'static str,
    column: &'static str,
    value: T,
) -> Result<()> {
    let now = super::now_epoch_secs();
    conn.execute(
        &format!(
            "INSERT INTO {table} (id, {column}, set_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET {column} = ?1, set_at = ?2"
        ),
        rusqlite::params![value, now],
    )
    .with_context(|| format!("upsert {table}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    /// The scalar pair round-trips every stored SQL type the singleton tables
    /// actually use, through one generic helper: `INTEGER`, `TEXT` and `BLOB`.
    #[tokio::test]
    async fn scalar_singleton_round_trips_integer_text_and_blob() {
        let db = CacheDb::open_in_memory().unwrap();

        // INTEGER (a port), TEXT (a declared region), BLOB (an actor id).
        db.set_singleton_column("caldav_port", "port", 8443i64)
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<i64>("caldav_port", "port")
                .await
                .unwrap(),
            Some(8443)
        );

        db.set_singleton_column("nest_region", "region", "NO")
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<String>("nest_region", "region")
                .await
                .unwrap()
                .as_deref(),
            Some("NO")
        );

        db.set_singleton_column("web_apex_actor", "actor_id", [7u8; 32].as_slice())
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<Vec<u8>>("web_apex_actor", "actor_id")
                .await
                .unwrap(),
            Some(vec![7u8; 32])
        );
    }

    /// An unwritten row reads `None` — the "admin has never chosen" state every
    /// typed wrapper folds to its own default.
    #[tokio::test]
    async fn unwritten_singleton_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_singleton_column::<i64>("serving_port", "port")
                .await
                .unwrap(),
            None
        );
    }

    /// The upsert is the single atomic decision point: the second write
    /// overwrites in place rather than inserting a second row.
    #[tokio::test]
    async fn second_write_overwrites_the_same_row() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_singleton_column("serving_port", "port", 443i64)
            .await
            .unwrap();
        db.set_singleton_column("serving_port", "port", 8443i64)
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<i64>("serving_port", "port")
                .await
                .unwrap(),
            Some(8443)
        );

        let conn = db.conn.lock().await;
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM serving_port", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the singleton table holds exactly the id = 1 row");
    }

    /// A nullable column distinguishes an absent row (never chosen) from a
    /// present row holding `NULL` (explicitly cleared) — the outer/inner
    /// `Option` split `node_policy`'s storage cap depends on.
    #[tokio::test]
    async fn nullable_column_separates_absent_row_from_stored_null() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_singleton_column::<Option<i64>>("nest_max_storage_bytes", "max_bytes")
                .await
                .unwrap(),
            None,
            "absent row"
        );

        db.set_singleton_column("nest_max_storage_bytes", "max_bytes", None::<i64>)
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<Option<i64>>("nest_max_storage_bytes", "max_bytes")
                .await
                .unwrap(),
            Some(None),
            "present row holding NULL"
        );

        db.set_singleton_column("nest_max_storage_bytes", "max_bytes", Some(4096i64))
            .await
            .unwrap();
        assert_eq!(
            db.get_singleton_column::<Option<i64>>("nest_max_storage_bytes", "max_bytes")
                .await
                .unwrap(),
            Some(Some(4096))
        );
    }

    /// The write stamps `set_at`, and a re-write advances it — the column every
    /// singleton table carries for "when did the admin last choose this".
    #[tokio::test]
    async fn write_stamps_set_at() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_singleton_column("caldav_enabled", "enabled", 1i64)
            .await
            .unwrap();
        let conn = db.conn.lock().await;
        let set_at: i64 = conn
            .query_row("SELECT set_at FROM caldav_enabled WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(set_at > 0, "set_at is stamped with the epoch second");
    }
}
