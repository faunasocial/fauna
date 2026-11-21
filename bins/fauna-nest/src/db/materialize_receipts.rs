//! The materialize receipt — one `materialize_receipts` row per
//! `(actor, set)` a re-seed's flip made live (table defined in
//! `migrations::MIGRATIONS_MATERIALIZE_RECEIPTS`).
//!
//! Owner: `docs/goal/behavior/backup-destinations.md` § Third destination kind
//! → *A lived-in segment set is completed by record in the same gesture, and
//! "already restored" is said only on the nest's receipt*, rule (2).
//! `fauna.backup.custody.materialize` (`backup::materialize`) writes the row
//! inside the one transaction that makes the set live — the segment arm's
//! commit, and the covered-folder arm's page that leaves nothing remaining —
//! so a receipt exists exactly when the flip committed. A later call for a
//! copy the receipt covers answers `already_materialized`, which leaves
//! `target_not_empty` meaning one thing: live content this ceremony did not
//! put there. A standing in-flight marker is read first; the receipt is no
//! force arm and no freshness answer.
//!
//! What "covers" means per axis: a segment set's copy is covered when its
//! mirror's ledger generation is at or below the receipt's; a covered-folder
//! set has no ledger, so its copy is covered when its anchor — the digest of
//! its `(path_hash, manifest_hash)` pairs ([`folder_copy_anchor`]) — is the
//! receipt's.

use anyhow::{Context as _, Result, anyhow};
use rusqlite::OptionalExtension;

use super::CacheDb;

/// One receipt: what the flip of `(actor, set_name)` made live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeReceipt {
    /// The corpus's ledger generation — a segment set's mirror
    /// `next_segment_id_seen`; `0` on the covered-folder axis, which has none.
    pub ledger_generation: u32,
    /// The copy's manifest anchor — a segment set's `manifest.<kind>` custody
    /// manifest hash; a covered-folder set's [`folder_copy_anchor`].
    pub manifest_anchor: [u8; 32],
    /// Epoch seconds of the commit.
    pub materialized_at: i64,
}

impl CacheDb {
    /// The receipt for `(actor, set_name)`, or `None` when no flip of it ever
    /// committed on this nest.
    pub async fn materialize_receipt_get(
        &self,
        actor: &[u8; 32],
        set_name: &str,
    ) -> Result<Option<MaterializeReceipt>> {
        let conn = self.conn.lock().await;
        receipt_get(&conn, actor, set_name)
    }
}

/// [`CacheDb::materialize_receipt_get`] on `conn` (a transaction derefs to one).
pub(crate) fn receipt_get(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    set_name: &str,
) -> Result<Option<MaterializeReceipt>> {
    let row = conn
        .query_row(
            "SELECT ledger_generation, manifest_anchor, materialized_at
             FROM materialize_receipts WHERE actor_id = ?1 AND set_name = ?2",
            rusqlite::params![actor.as_slice(), set_name],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .context("read materialize_receipts")?;
    let Some((generation, anchor, materialized_at)) = row else {
        return Ok(None);
    };
    Ok(Some(MaterializeReceipt {
        ledger_generation: u32::try_from(generation).map_err(|_| {
            anyhow!("materialize_receipts.ledger_generation {generation} is not a u32")
        })?,
        manifest_anchor: <[u8; 32]>::try_from(anchor.as_slice()).map_err(|_| {
            anyhow!(
                "materialize_receipts.manifest_anchor of {} bytes, not 32",
                anchor.len()
            )
        })?,
        materialized_at,
    }))
}

/// Write (or overwrite) the receipt for `(actor, set_name)` on `tx` — always
/// inside the transaction that makes the set live, so the receipt and the
/// live corpus commit together or not at all.
pub(crate) fn receipt_put_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actor: &[u8; 32],
    set_name: &str,
    receipt: &MaterializeReceipt,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO materialize_receipts
            (actor_id, set_name, ledger_generation, manifest_anchor, materialized_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            actor.as_slice(),
            set_name,
            i64::from(receipt.ledger_generation),
            receipt.manifest_anchor.as_slice(),
            receipt.materialized_at,
        ],
    )
    .context("write materialize_receipts")?;
    Ok(())
}

/// A covered-folder copy's anchor: blake3 over its `(path_hash,
/// manifest_hash)` pairs in ascending order, under its own domain tag — the
/// same copy re-delivered gives the same anchor, a copy differing in any path
/// or any content a different one.
pub fn folder_copy_anchor<'a>(
    pairs: impl IntoIterator<Item = (&'a [u8; 32], &'a [u8; 32])>,
) -> [u8; 32] {
    let mut sorted: Vec<(&[u8; 32], &[u8; 32])> = pairs.into_iter().collect();
    sorted.sort();
    let mut h = blake3::Hasher::new();
    h.update(b"fauna.materialize.folder-copy-anchor.v1");
    for (path, manifest) in sorted {
        h.update(path);
        h.update(manifest);
    }
    *h.finalize().as_bytes()
}
