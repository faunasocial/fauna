//! The in-flight marker of a re-seed's flip — one `materialize_in_flight` row
//! per `(actor, kind)` (table defined in
//! `migrations::MIGRATIONS_MATERIALIZE_IN_FLIGHT`).
//!
//! Owner: `docs/goal/behavior/backup-destinations.md` § Third destination kind
//! → *Re-seed* → *An interrupted flip completes on retry, whatever arrived
//! since — the in-flight marker*. `fauna.backup.custody.materialize`
//! (`backup::materialize`) writes the row before its first write anything else
//! can observe, and deletes it inside the one transaction that makes the corpus
//! live; while it stands the target is mid-flip, not lived-in.
//!
//! Get and put only. The delete is raw SQL inside materialize's commit
//! transaction, so the marker cannot outlive the commit or leave before it.
//!
//! Beside the marker, `materialize_deleted_collections` names each collection a
//! door deleted while the marker stood ([`note_collection_deleted_in_flight`],
//! written inside the door's own transaction); the commit deletes those rows in
//! the same statement group as the marker.

use anyhow::{Context as _, Result, anyhow};
use rusqlite::OptionalExtension;

use super::CacheDb;

/// Which journal shape the flipped copy holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InFlightJournal {
    /// A journaled kind (mail, calendar, contacts): the corpus's journal is
    /// adopted beside its content.
    Adopt,
    /// A kind with no placement layer (posts).
    None,
}

impl InFlightJournal {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::None => "none",
        }
    }

    fn from_sql(s: &str) -> Result<Self> {
        match s {
            "adopt" => Ok(Self::Adopt),
            "none" => Ok(Self::None),
            other => Err(anyhow!(
                "materialize_in_flight.journal `{other}` is not a journal shape"
            )),
        }
    }
}

/// One standing marker: the gesture materialize recorded for `(actor, kind)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeInFlight {
    /// The reserved custody set the gesture flips.
    pub set_name: String,
    /// The corpus's content segment ids, ascending — everything live outside
    /// them is post-gesture activity.
    pub segment_ids: Vec<u32>,
    pub journal: InFlightJournal,
    /// Epoch seconds of the gesture's first run; a retry keeps it.
    pub started_at: i64,
}

impl CacheDb {
    /// The standing marker for `(actor, kind)`, or `None` when no flip is in
    /// flight.
    pub async fn materialize_in_flight_get(
        &self,
        actor: &[u8; 32],
        kind: &str,
    ) -> Result<Option<MaterializeInFlight>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT set_name, segment_ids, journal, started_at FROM materialize_in_flight
                 WHERE actor_id = ?1 AND kind = ?2",
                rusqlite::params![actor.as_slice(), kind],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .context("read materialize_in_flight")?;
        let Some((set_name, ids, journal, started_at)) = row else {
            return Ok(None);
        };
        let segment_ids: Vec<u32> = fauna_cbor::decode_strict(&ids)
            .map_err(|e| anyhow!("materialize_in_flight.segment_ids does not decode: {e}"))?;
        Ok(Some(MaterializeInFlight {
            set_name,
            segment_ids,
            journal: InFlightJournal::from_sql(&journal)?,
            started_at,
        }))
    }

    /// Record (or re-record) the marker for `(actor, kind)`. `INSERT OR
    /// REPLACE`, so a retry re-putting the marker it found is idempotent.
    pub async fn materialize_in_flight_put(
        &self,
        actor: &[u8; 32],
        kind: &str,
        marker: &MaterializeInFlight,
    ) -> Result<()> {
        let ids = fauna_cbor::encode_canonical(&marker.segment_ids)
            .map_err(|e| anyhow!("encode materialize_in_flight.segment_ids: {e}"))?;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO materialize_in_flight
                (actor_id, kind, set_name, segment_ids, journal, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor.as_slice(),
                kind,
                &marker.set_name,
                ids,
                marker.journal.as_sql(),
                marker.started_at,
            ],
        )
        .context("write materialize_in_flight")?;
        Ok(())
    }

    /// The collections a door deleted while the marker for `(actor, kind)`
    /// stood ([`note_collection_deleted_in_flight`]).
    pub async fn materialize_deleted_collections(
        &self,
        actor: &[u8; 32],
        kind: &str,
    ) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        deleted_collections_in_flight(&conn, actor, kind)
    }
}

/// The collections a door deleted while the marker for `(actor, kind)` stood,
/// read on `conn` (a transaction derefs to one).
pub(crate) fn deleted_collections_in_flight(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    kind: &str,
) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare(
            "SELECT collection_id FROM materialize_deleted_collections
             WHERE actor_id = ?1 AND kind = ?2 ORDER BY collection_id",
        )
        .context("prepare the deleted-collections read")?;
    let rows = stmt
        .query_map(rusqlite::params![actor.as_slice(), kind], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .context("read materialize_deleted_collections")?;
    let mut out = Vec::new();
    for row in rows {
        let bytes = row.context("read a deleted collection id")?;
        out.push(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            anyhow!(
                "materialize_deleted_collections.collection_id of {} bytes, not 32",
                bytes.len()
            )
        })?);
    }
    Ok(out)
}

/// **A door's collection deletion while the flip is torn, recorded for the
/// retry** (`backup-destinations.md` § *the in-flight marker*: a retry keeps
/// post-gesture activity, never undoes it). Called by a collection delete door
/// inside the transaction that deletes the collection; writes a row only when
/// the marker for `(actor, kind)` stands, so outside a flip it is a no-op. The
/// retry then neither re-inserts the collection from the corpus nor keeps the
/// corpus's version of it in the adopted journal.
pub(crate) fn note_collection_deleted_in_flight(
    tx: &rusqlite::Transaction<'_>,
    actor: &[u8; 32],
    kind: &str,
    collection_id: &[u8; 32],
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO materialize_deleted_collections (actor_id, kind, collection_id)
         SELECT ?1, ?2, ?3
         WHERE EXISTS (SELECT 1 FROM materialize_in_flight WHERE actor_id = ?1 AND kind = ?2)",
        rusqlite::params![actor.as_slice(), kind, collection_id.as_slice()],
    )
    .context("record a collection deleted while the flip is in flight")?;
    Ok(())
}
