//! The generation escrow-wrap store — R14 (account-data-plane.md § The ratified decisions) build step 4, the nest as v1
//! escrow holder (`account-data-plane.md` § The generation machinery →
//! *The escrow doors*). The nest stores opaque ciphertext only; the wrap is
//! sealed to the identity's published X-Wing escrow target, and nothing here
//! can or tries to read it.
//!
//! **The kept wrap** (`owner-key-material.md` § Path A-sibling-2 →
//! *Rotation*, the succession rider → *The kept wrap*). A wrap rests under
//! the actor id that deposited it — the identity whose escrow target it
//! seals to — and a succession leaves it there (`Succession::Stay`). So the
//! account's wraps are its own and those of every identity its recorded
//! successions retired into it ([`escrow_chain`]): `get` serves the chain,
//! a successor's `put` of a generation sweeps the predecessors' wraps of
//! that generation (the deposit they were kept for), and `delete` and the
//! belted receipt retire's sweep take the generation across the chain.

use anyhow::{Context, Result};

use super::successions::local_predecessors;
use super::{CacheDb, now_epoch_millis};

/// The actor ids whose wraps are this account's: `actor_id` first, then
/// every identity the nest's recorded successions retired into it — the
/// chain account deletion walks ([`local_predecessors`]).
pub(super) fn escrow_chain(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
) -> Result<Vec<[u8; 32]>> {
    let mut chain = vec![*actor_id];
    chain.extend(local_predecessors(conn, actor_id)?);
    Ok(chain)
}

/// Delete `generation_id`'s wraps under every id of `chain`; the count
/// deleted.
pub(super) fn delete_chain_generation_wraps(
    conn: &rusqlite::Connection,
    chain: &[[u8; 32]],
    generation_id: &[u8; 32],
) -> Result<i64> {
    let mut n = 0;
    for actor in chain {
        n += conn
            .execute(
                "DELETE FROM generation_escrow_wraps
                  WHERE actor_id = ?1 AND generation_id = ?2",
                rusqlite::params![&actor[..], &generation_id[..]],
            )
            .context("delete generation escrow wraps")?;
    }
    Ok(n as i64)
}

/// One deposited escrow wrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowWrapRow {
    pub generation_id: [u8; 32],
    pub wrap: Vec<u8>,
    /// First-deposit instant, unix ms — the stamp the holder-signed receipt
    /// carries (stable across idempotent re-deposits by construction).
    pub created_at: i64,
}

impl CacheDb {
    /// Deposit one wrap, idempotently per `(generation id, wrap hash)`:
    /// the first deposit inserts; a byte-identical re-deposit changes
    /// nothing. Returns the surviving row's `created_at` — the stamp the
    /// receipt signs — so a crash-retrying depositor receives a
    /// byte-identical receipt rather than a second variant racing the
    /// immutable `fauna.state.escrow-receipt` plane row.
    ///
    /// In the same transaction it deletes every chain predecessor's wraps of
    /// the same generation: the kept wrap is swept by the successor deposit
    /// it was kept for (module docs).
    pub async fn put_generation_escrow_wrap(
        &self,
        actor_id: &[u8; 32],
        generation_id: &[u8; 32],
        wrap_hash: &[u8; 32],
        wrap: &[u8],
    ) -> Result<i64> {
        let actor = *actor_id;
        let generation = *generation_id;
        let hash = *wrap_hash;
        let wrap = wrap.to_vec();
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin escrow put")?;
        tx.execute(
            "INSERT OR IGNORE INTO generation_escrow_wraps
                (actor_id, generation_id, wrap_hash, wrap, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![&actor[..], &generation[..], &hash[..], wrap, now],
        )
        .context("put generation escrow wrap")?;
        let predecessors = local_predecessors(&tx, &actor)?;
        delete_chain_generation_wraps(&tx, &predecessors, &generation)?;
        let stamp = tx
            .query_row(
                "SELECT created_at FROM generation_escrow_wraps
                  WHERE actor_id = ?1 AND generation_id = ?2 AND wrap_hash = ?3",
                rusqlite::params![&actor[..], &generation[..], &hash[..]],
                |row| row.get::<_, i64>(0),
            )
            .context("read back generation escrow wrap stamp")?;
        tx.commit().context("commit escrow put")?;
        Ok(stamp)
    }

    /// The account's deposited wraps — its own and every chain
    /// predecessor's kept ones (module docs) — optionally one generation's,
    /// in deposit order.
    pub async fn get_generation_escrow_wraps(
        &self,
        actor_id: &[u8; 32],
        generation_id: Option<&[u8; 32]>,
    ) -> Result<Vec<EscrowWrapRow>> {
        let generation = generation_id.copied();
        let conn = self.conn.lock().await;
        let chain = escrow_chain(&conn, actor_id)?;
        let mut out: Vec<(EscrowWrapRow, Vec<u8>)> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT generation_id, wrap, created_at, wrap_hash FROM generation_escrow_wraps
              WHERE actor_id = ?1 AND (?2 IS NULL OR generation_id = ?2)",
        )?;
        let g: Option<&[u8]> = generation.as_ref().map(|g| &g[..]);
        for actor in &chain {
            let mut rows = stmt.query(rusqlite::params![&actor[..], g])?;
            while let Some(row) = rows.next()? {
                let generation_id: Vec<u8> = row.get(0)?;
                let wrap: Vec<u8> = row.get(1)?;
                let created_at: i64 = row.get(2)?;
                let wrap_hash: Vec<u8> = row.get(3)?;
                if let Ok(generation_id) = <[u8; 32]>::try_from(generation_id.as_slice()) {
                    out.push((
                        EscrowWrapRow {
                            generation_id,
                            wrap,
                            created_at,
                        },
                        wrap_hash,
                    ));
                }
            }
        }
        out.sort_by(|(a, ah), (b, bh)| (a.created_at, ah).cmp(&(b.created_at, bh)));
        Ok(out.into_iter().map(|(row, _)| row).collect())
    }

    /// Delete one generation's wraps across the account's chain (module
    /// docs) — the holder-side half of the per-generation crypto-shred.
    /// Idempotent: deleting an absent generation deletes zero rows.
    pub async fn delete_generation_escrow_wraps(
        &self,
        actor_id: &[u8; 32],
        generation_id: &[u8; 32],
    ) -> Result<i64> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin escrow delete")?;
        let chain = escrow_chain(&tx, actor_id)?;
        let n = delete_chain_generation_wraps(&tx, &chain, generation_id)?;
        tx.commit().context("commit escrow delete")?;
        Ok(n)
    }
}
