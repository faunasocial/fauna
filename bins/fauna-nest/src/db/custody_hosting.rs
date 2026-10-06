//! Custody-**hosting** rows — the custodian-nest runtime's stage (b) store
//! (`account-data-plane.md` § Replica posture → The custody grant + ceremony,
//! the device-or-nest bullet, item 6).
//!
//! One row per `(host_actor_id, grant_id)` in `custody_hosting` (table defined
//! in `migrations::MIGRATIONS_CUSTODY_HOSTING`): a custody this nest's own
//! user accepted NEST-anchored, deposited over
//! `fauna.custody.hosting.register` so the nest's pump can pull the custodied
//! owner's planes with no host device running. The row is a **projection of
//! the host's client-sealed ceremony record** (`HeldCustody`, a `fauna.state.custody-ceremony`
//! plane row, which this nest cannot read — the same reason `backup_destinations`
//! exists): witness verbatim, the owner's nest URL, the opaque owner-fleet
//! endpoints snapshot, and the host-chosen budget. Losing it loses nothing a
//! re-deposit cannot recreate.
//!
//! Two writer roles share the row and never touch each other's columns:
//!
//! * the **host** (over the register door) writes the registered fields —
//!   `put_custody_hosting` is an UPSERT whose DO-UPDATE arm deliberately
//!   excludes the metering columns, so a stop or budget rewrite can never
//!   zero what the pump has metered;
//! * the **pump** writes the metering (`held_bytes`, `last_receipt_at`) via
//!   `update_custody_hosting_metering`, read back over
//!   `fauna.custody.hosting.list` for the host UI.
//!
//! CRUD only — witness verification (this-nest binding, owner signature,
//! window) is the handler's job (`custody_hosting_handlers.rs`, which holds
//! the nest identity); URL *policy* is checked at the door AND re-checked by
//! the pump every pass; this module never dials or
//! interprets a URL.

use anyhow::{Result, anyhow};

use super::{CacheDb, now_epoch_secs};

/// One custody-hosting row, as the host's app reads it back and as the pump
/// enumerates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyHostingRow {
    /// The ceremony's grant id (`CustodyGrant.grant_id`).
    pub grant_id: Vec<u8>,
    /// The custodied OWNER's 32-byte actor id.
    pub owner_actor_id: Vec<u8>,
    /// The owner-signed custody witness, canonical bytes verbatim.
    pub witness: Vec<u8>,
    /// The owner's nest URL — the pull leg's only dial anchor.
    pub owner_nest_url: String,
    /// Opaque owner-fleet endpoints snapshot (canonical CBOR of
    /// `Vec<DeviceEndpoints>`); carried for the restore/serve follow-on, never
    /// dialled by the pull leg.
    pub owner_devices: Vec<u8>,
    /// Host-chosen byte budget; `0` = cap missing (the pump substitutes the
    /// hard-coded default — never "hold nothing").
    pub retained_bytes_cap: u64,
    /// Stopped rows are kept (the UI lists them) but the pump skips them.
    pub stopped: bool,
    /// Unix seconds of the last register/rewrite.
    pub updated_at: i64,
    /// Pump-metered bytes currently held (post-eviction); `0` until the first
    /// pull pass.
    pub held_bytes: u64,
    /// `attested_at` (microseconds, the receipt's own stamp) of the newest
    /// receipt the pump DEPOSITED at the owner's nest for this row; `0` =
    /// none yet. Advances only on an acked deposit, so a failed deposit
    /// leaves the receipt "due" and the next pass redrives it.
    pub last_receipt_at: u64,
    /// Whether that last deposited receipt read degraded — `receipt_due`'s
    /// flip detector.
    pub last_receipt_degraded: bool,
}

impl CacheDb {
    /// Deposit (or rewrite) a hosting row. UPSERT on `(host, grant_id)`: the
    /// stop control and the budget-adjust are this same verb, and the
    /// DO-UPDATE arm excludes the pump's metering columns by design.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_custody_hosting(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
        owner_actor_id: &[u8],
        witness: &[u8],
        owner_nest_url: &str,
        owner_devices: &[u8],
        retained_bytes_cap: u64,
        stopped: bool,
    ) -> Result<()> {
        if grant_id.is_empty() {
            return Err(anyhow!("grant_id must not be empty"));
        }
        if owner_actor_id.len() != 32 {
            return Err(anyhow!(
                "owner actor id must be 32 bytes, got {}",
                owner_actor_id.len()
            ));
        }
        if witness.is_empty() {
            return Err(anyhow!("witness must not be empty"));
        }
        if owner_nest_url.is_empty() {
            return Err(anyhow!("owner_nest_url must not be empty"));
        }
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let owner_actor_id = owner_actor_id.to_vec();
        let witness = witness.to_vec();
        let owner_devices = owner_devices.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO custody_hosting
                (host_actor_id, grant_id, owner_actor_id, witness, owner_nest_url,
                 owner_devices, retained_bytes_cap, stopped, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(host_actor_id, grant_id) DO UPDATE SET
                owner_actor_id = excluded.owner_actor_id,
                witness = excluded.witness,
                owner_nest_url = excluded.owner_nest_url,
                owner_devices = excluded.owner_devices,
                retained_bytes_cap = excluded.retained_bytes_cap,
                stopped = excluded.stopped,
                updated_at = excluded.updated_at",
            rusqlite::params![
                host_actor_id,
                grant_id,
                owner_actor_id,
                witness,
                owner_nest_url,
                owner_devices,
                retained_bytes_cap as i64,
                stopped,
                now,
            ],
        )
        .map_err(|e| anyhow!("put custody hosting row: {e}"))?;
        Ok(())
    }

    /// The caller's own hosting rows, oldest-registered first — the
    /// `fauna.custody.hosting.list` read-back. **Host-scoped**: a caller only
    /// ever sees its own rows.
    pub async fn list_custody_hosting(
        &self,
        host_actor_id: &[u8],
    ) -> Result<Vec<CustodyHostingRow>> {
        let host_actor_id = host_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT grant_id, owner_actor_id, witness, owner_nest_url, owner_devices,
                    retained_bytes_cap, stopped, updated_at, held_bytes, last_receipt_at,
                    last_receipt_degraded
             FROM custody_hosting
             WHERE host_actor_id = ?1
             ORDER BY updated_at, grant_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![host_actor_id], row_from_sql)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many rows this host already holds, and whether `grant_id` is one of
    /// them — the register door's cap probe (the row count bounds
    /// the pump's per-host outbound dial fan-out and store count, not just
    /// disk).
    ///
    /// Both halves in one query on purpose: the door must admit a **rewrite** of
    /// an existing row even at the cap, or a host sitting at the cap could never
    /// *stop* one of its own rows — which would turn the cap itself into
    /// unrecoverable state.
    pub async fn count_custody_hosting(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
    ) -> Result<(usize, bool)> {
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        let (total, existing): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(grant_id = ?2), 0)
                 FROM custody_hosting WHERE host_actor_id = ?1",
                rusqlite::params![host_actor_id, grant_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| anyhow!("count custody hosting rows: {e}"))?;
        Ok((total.max(0) as usize, existing > 0))
    }

    /// This host's **derived** held-bytes figure — `SUM(held_bytes)` over its
    /// rows.
    ///
    /// Derived, never accumulated: the pump already meters every row every pass,
    /// so summing is the whole accounting. That is what makes removing a hosting
    /// row need no credit-back — dropping the row drops the figure — and it is
    /// why these bytes stay out of `users.storage_bytes_used`, which the sync
    /// plane enforces against the host's own writes.
    pub async fn sum_custody_hosting_held(&self, host_actor_id: &[u8]) -> Result<u64> {
        let host_actor_id = host_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let sum: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(held_bytes), 0) FROM custody_hosting
                 WHERE host_actor_id = ?1",
                rusqlite::params![host_actor_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow!("sum custody hosting held bytes: {e}"))?;
        Ok(sum.max(0) as u64)
    }

    /// Every hosting row on this nest, with its depositing host — the pump's
    /// per-pass enumeration. Stopped rows are included (the pump itself skips
    /// them, and counting them is how a pass report stays honest).
    pub async fn list_all_custody_hosting(&self) -> Result<Vec<(Vec<u8>, CustodyHostingRow)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT host_actor_id, grant_id, owner_actor_id, witness, owner_nest_url,
                    owner_devices, retained_bytes_cap, stopped, updated_at, held_bytes,
                    last_receipt_at, last_receipt_degraded
             FROM custody_hosting
             ORDER BY host_actor_id, updated_at, grant_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let host: Vec<u8> = row.get(0)?;
            Ok((
                host,
                CustodyHostingRow {
                    grant_id: row.get(1)?,
                    owner_actor_id: row.get(2)?,
                    witness: row.get(3)?,
                    owner_nest_url: row.get(4)?,
                    owner_devices: row.get(5)?,
                    retained_bytes_cap: row.get::<_, i64>(6)? as u64,
                    stopped: row.get(7)?,
                    updated_at: row.get(8)?,
                    held_bytes: row.get::<_, i64>(9)? as u64,
                    last_receipt_at: row.get::<_, i64>(10)? as u64,
                    last_receipt_degraded: row.get(11)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One hosting row by its key — the admin remove door's pre-read (it
    /// needs the owner to locate the `(host, owner)` custodied store).
    pub async fn get_custody_hosting(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
    ) -> Result<Option<CustodyHostingRow>> {
        use rusqlite::OptionalExtension;
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT grant_id, owner_actor_id, witness, owner_nest_url, owner_devices,
                    retained_bytes_cap, stopped, updated_at, held_bytes, last_receipt_at,
                    last_receipt_degraded
             FROM custody_hosting
             WHERE host_actor_id = ?1 AND grant_id = ?2",
            rusqlite::params![host_actor_id, grant_id],
            row_from_sql,
        )
        .optional()
        .map_err(|e| anyhow!("get custody hosting row: {e}"))
    }

    /// Drop one hosting row (the admin remove door).
    /// Returns whether a row existed. The derived held-bytes counter needs no
    /// credit-back: dropping the row drops the figure.
    pub async fn delete_custody_hosting(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
    ) -> Result<bool> {
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM custody_hosting WHERE host_actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![host_actor_id, grant_id],
            )
            .map_err(|e| anyhow!("delete custody hosting row: {e}"))?;
        Ok(n > 0)
    }

    /// Remaining rows for one `(host, owner)` pair — the store-teardown gate:
    /// the custodied store dir is shared by every grant of the pair, so it
    /// falls only with the pair's LAST row.
    pub async fn count_custody_hosting_for_host_owner(
        &self,
        host_actor_id: &[u8],
        owner_actor_id: &[u8],
    ) -> Result<usize> {
        let host_actor_id = host_actor_id.to_vec();
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM custody_hosting
                 WHERE host_actor_id = ?1 AND owner_actor_id = ?2",
                rusqlite::params![host_actor_id, owner_actor_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow!("count custody hosting rows for host+owner: {e}"))?;
        Ok(n.max(0) as usize)
    }

    /// The pump's metering write-back after a pass over one row. Touches ONLY
    /// `held_bytes`; a row the host deleted concurrently is a no-op (the pump
    /// never resurrects a row).
    pub async fn update_custody_hosting_metering(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
        held_bytes: u64,
    ) -> Result<()> {
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE custody_hosting
             SET held_bytes = ?3
             WHERE host_actor_id = ?1 AND grant_id = ?2",
            rusqlite::params![host_actor_id, grant_id, held_bytes as i64],
        )
        .map_err(|e| anyhow!("update custody hosting metering: {e}"))?;
        Ok(())
    }

    /// The pump's receipt bookkeeping, written ONLY when the owner's nest
    /// acked a deposit — which is exactly what makes a failed deposit stay
    /// `receipt_due` and redrive next pass. Same no-resurrect rule as the
    /// metering write.
    pub async fn record_custody_hosting_receipt(
        &self,
        host_actor_id: &[u8],
        grant_id: &[u8],
        attested_at: u64,
        degraded: bool,
    ) -> Result<()> {
        let host_actor_id = host_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE custody_hosting
             SET last_receipt_at = ?3, last_receipt_degraded = ?4
             WHERE host_actor_id = ?1 AND grant_id = ?2",
            rusqlite::params![host_actor_id, grant_id, attested_at as i64, degraded],
        )
        .map_err(|e| anyhow!("record custody hosting receipt: {e}"))?;
        Ok(())
    }
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<CustodyHostingRow> {
    Ok(CustodyHostingRow {
        grant_id: row.get(0)?,
        owner_actor_id: row.get(1)?,
        witness: row.get(2)?,
        owner_nest_url: row.get(3)?,
        owner_devices: row.get(4)?,
        retained_bytes_cap: row.get::<_, i64>(5)? as u64,
        stopped: row.get(6)?,
        updated_at: row.get(7)?,
        held_bytes: row.get::<_, i64>(8)? as u64,
        last_receipt_at: row.get::<_, i64>(9)? as u64,
        last_receipt_degraded: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_A: [u8; 32] = [0xA1; 32];
    const HOST_B: [u8; 32] = [0xB1; 32];
    const OWNER_1: [u8; 32] = [0x01; 32];
    const OWNER_2: [u8; 32] = [0x02; 32];
    const GRANT_1: [u8; 16] = [0x11; 16];
    const GRANT_2: [u8; 16] = [0x22; 16];

    async fn put(db: &CacheDb, host: &[u8], grant: &[u8], owner: &[u8], url: &str) {
        db.put_custody_hosting(host, grant, owner, b"witness-bytes", url, b"", 4096, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn register_list_round_trip_host_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.list_custody_hosting(&HOST_A).await.unwrap().is_empty());

        put(&db, &HOST_A, &GRANT_1, &OWNER_1, "https://owner1.example").await;
        put(&db, &HOST_B, &GRANT_2, &OWNER_2, "https://owner2.example").await;

        let a = db.list_custody_hosting(&HOST_A).await.unwrap();
        assert_eq!(a.len(), 1, "host A sees exactly its own row");
        assert_eq!(a[0].grant_id, GRANT_1.to_vec());
        assert_eq!(a[0].owner_actor_id, OWNER_1.to_vec());
        assert_eq!(a[0].witness, b"witness-bytes".to_vec());
        assert_eq!(a[0].owner_nest_url, "https://owner1.example");
        assert_eq!(a[0].retained_bytes_cap, 4096);
        assert!(!a[0].stopped);
        assert_eq!(a[0].held_bytes, 0, "no pull pass yet");
        assert_eq!(a[0].last_receipt_at, 0, "no receipt yet");

        let all = db.list_all_custody_hosting().await.unwrap();
        assert_eq!(all.len(), 2, "the pump enumerates every host's rows");
    }

    #[tokio::test]
    async fn a_host_rewrite_never_zeroes_the_pumps_metering() {
        // The stop control and the budget-adjust are re-registers; the pump's
        // held_bytes/last_receipt_at must survive them, or every budget edit
        // would render "holds nothing / never attested" until the next pass.
        let db = CacheDb::open_in_memory().unwrap();
        put(&db, &HOST_A, &GRANT_1, &OWNER_1, "https://owner1.example").await;
        db.update_custody_hosting_metering(&HOST_A, &GRANT_1, 999)
            .await
            .unwrap();
        db.record_custody_hosting_receipt(&HOST_A, &GRANT_1, 777, true)
            .await
            .unwrap();

        // The stop rewrite (same verb, stopped = true, budget narrowed).
        db.put_custody_hosting(
            &HOST_A,
            &GRANT_1,
            &OWNER_1,
            b"witness-bytes",
            "https://owner1.example",
            b"",
            1024,
            true,
        )
        .await
        .unwrap();

        let rows = db.list_custody_hosting(&HOST_A).await.unwrap();
        assert_eq!(rows.len(), 1, "rewrite replaced in place, never duplicated");
        assert!(rows[0].stopped);
        assert_eq!(rows[0].retained_bytes_cap, 1024);
        assert_eq!(rows[0].held_bytes, 999, "metering survived the rewrite");
        assert_eq!(rows[0].last_receipt_at, 777);
        assert!(rows[0].last_receipt_degraded, "receipt state survived too");
    }

    #[tokio::test]
    async fn metering_update_on_a_missing_row_is_a_no_op_never_a_resurrection() {
        let db = CacheDb::open_in_memory().unwrap();
        db.update_custody_hosting_metering(&HOST_A, &GRANT_1, 5)
            .await
            .unwrap();
        db.record_custody_hosting_receipt(&HOST_A, &GRANT_1, 5, false)
            .await
            .unwrap();
        assert!(
            db.list_custody_hosting(&HOST_A).await.unwrap().is_empty(),
            "the pump never creates a row the host did not deposit"
        );
    }

    /// The register door's cap probe. Host-scoped like the
    /// read-back, and it must distinguish a NEW row from a rewrite — the door
    /// admits a rewrite at the cap so a host at the cap can still stop a row.
    #[tokio::test]
    async fn the_count_probe_is_host_scoped_and_names_a_rewrite() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.count_custody_hosting(&HOST_A, &GRANT_1).await.unwrap(),
            (0, false),
            "no rows yet: nothing held, and this grant is not a rewrite"
        );

        put(&db, &HOST_A, &GRANT_1, &OWNER_1, "https://owner1.example").await;
        put(&db, &HOST_B, &GRANT_1, &OWNER_2, "https://owner2.example").await;

        assert_eq!(
            db.count_custody_hosting(&HOST_A, &GRANT_1).await.unwrap(),
            (1, true),
            "host A's own row is a rewrite, and host B's identical grant id is not counted"
        );
        assert_eq!(
            db.count_custody_hosting(&HOST_A, &GRANT_2).await.unwrap(),
            (1, false),
            "a different grant on the same host is a NEW row against the cap"
        );

        put(&db, &HOST_A, &GRANT_2, &OWNER_2, "https://owner2.example").await;
        assert_eq!(
            db.count_custody_hosting(&HOST_A, &GRANT_2).await.unwrap(),
            (2, true)
        );
    }

    /// The tier-bound accounting's counter. Host-scoped like
    /// the read-back — another host's held bytes never count against this
    /// one — and DERIVED: re-metering replaces the row's figure, so the sum
    /// follows with no credit-back bookkeeping anywhere.
    #[tokio::test]
    async fn the_held_sum_is_derived_and_host_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.sum_custody_hosting_held(&HOST_A).await.unwrap(),
            0,
            "no rows: the sum is zero, not an error"
        );

        put(&db, &HOST_A, &GRANT_1, &OWNER_1, "https://owner1.example").await;
        put(&db, &HOST_A, &GRANT_2, &OWNER_2, "https://owner2.example").await;
        put(&db, &HOST_B, &GRANT_1, &OWNER_2, "https://owner2.example").await;
        assert_eq!(
            db.sum_custody_hosting_held(&HOST_A).await.unwrap(),
            0,
            "unmetered rows hold nothing yet"
        );

        db.update_custody_hosting_metering(&HOST_A, &GRANT_1, 100)
            .await
            .unwrap();
        db.update_custody_hosting_metering(&HOST_A, &GRANT_2, 250)
            .await
            .unwrap();
        db.update_custody_hosting_metering(&HOST_B, &GRANT_1, 999)
            .await
            .unwrap();

        assert_eq!(
            db.sum_custody_hosting_held(&HOST_A).await.unwrap(),
            350,
            "host A's sum is over its own rows alone — host B's 999 not counted"
        );
        assert_eq!(db.sum_custody_hosting_held(&HOST_B).await.unwrap(), 999);

        // Derived, never accumulated: the next pass's metering replaces the
        // figure, it does not add to it.
        db.update_custody_hosting_metering(&HOST_A, &GRANT_1, 0)
            .await
            .unwrap();
        assert_eq!(
            db.sum_custody_hosting_held(&HOST_A).await.unwrap(),
            250,
            "an evicted-to-empty row drops out of the sum by derivation"
        );
    }

    #[tokio::test]
    async fn the_store_refuses_mechanically_malformed_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        for (grant, owner, url, needle) in [
            (&[][..], &OWNER_1[..], "https://o.example", "grant_id"),
            (
                &GRANT_1[..],
                &[0u8; 31][..],
                "https://o.example",
                "32 bytes",
            ),
            (&GRANT_1[..], &OWNER_1[..], "", "owner_nest_url"),
        ] {
            let err = db
                .put_custody_hosting(&HOST_A, grant, owner, b"w", url, b"", 0, false)
                .await
                .unwrap_err();
            assert!(err.to_string().contains(needle), "unexpected error: {err}");
        }
        let err = db
            .put_custody_hosting(
                &HOST_A,
                &GRANT_1,
                &OWNER_1,
                b"",
                "https://o.example",
                b"",
                0,
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("witness"));
        assert!(db.list_custody_hosting(&HOST_A).await.unwrap().is_empty());
    }
}
