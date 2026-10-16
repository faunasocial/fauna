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
}
