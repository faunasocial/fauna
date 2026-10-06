//! Per-nest engagement statistics, trust scoring, and anomaly detection.
//!
//! Tracks how many events each remote nest publishes (by type), maintains
//! a trust score, and flags basic anomalies (volume spikes, uniform timing).

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use fauna_core::engagement::AnomalyFlag;

use super::CacheDb;

/// Aggregate engagement statistics for a single nest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestEngagementStats {
    pub total_events: i64,
    pub distinct_event_types: i64,
}

impl CacheDb {
    /// Record (or increment) an engagement event for a nest.
    ///
    /// Uses UPSERT: if the (nest_id, event_type) row already exists, the
    /// event_count is incremented and last_seen is updated.
    pub async fn record_nest_engagement(
        &self,
        nest_id: &[u8; 32],
        event_type: &str,
        timestamp: i64,
    ) -> Result<()> {
        let nest_id = nest_id.to_vec();
        let event_type = event_type.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO nest_engagement_stats (nest_id, event_type, event_count, last_seen)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(nest_id, event_type) DO UPDATE SET
                 event_count = event_count + 1,
                 last_seen = MAX(last_seen, excluded.last_seen)",
            rusqlite::params![nest_id, event_type, timestamp],
        )
        .context("record nest engagement")?;
        Ok(())
    }

    /// Return aggregate engagement statistics for a nest.
    pub async fn get_nest_engagement_stats(
        &self,
        nest_id: &[u8; 32],
    ) -> Result<NestEngagementStats> {
        let nest_id = nest_id.to_vec();
        let conn = self.conn.lock().await;
        let stats = conn
            .query_row(
                "SELECT COALESCE(SUM(event_count), 0), COUNT(DISTINCT event_type)
                 FROM nest_engagement_stats
                 WHERE nest_id = ?1",
                rusqlite::params![nest_id],
                |row| {
                    Ok(NestEngagementStats {
                        total_events: row.get(0)?,
                        distinct_event_types: row.get(1)?,
                    })
                },
            )
            .context("get nest engagement stats")?;
        Ok(stats)
    }

    /// Return the trust score for a nest. Defaults to 1.0 for unknown nests.
    pub async fn get_nest_trust(&self, nest_id: &[u8; 32]) -> Result<f64> {
        let nest_id = nest_id.to_vec();
        let conn = self.conn.lock().await;
        let score = conn
            .query_row(
                "SELECT trust_score FROM nest_trust WHERE nest_id = ?1",
                rusqlite::params![nest_id],
                |row| row.get(0),
            )
            .optional()
            .context("get nest trust")?;
        Ok(score.unwrap_or(1.0))
    }

    /// Set (or update) the trust score for a nest.
    pub async fn set_nest_trust(
        &self,
        nest_id: &[u8; 32],
        trust_score: f64,
        reason: &str,
    ) -> Result<()> {
        let nest_id = nest_id.to_vec();
        let reason = reason.to_string();
        let now = super::now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO nest_trust (nest_id, trust_score, reason, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(nest_id) DO UPDATE SET
                 trust_score = excluded.trust_score,
                 reason = excluded.reason,
                 updated_at = excluded.updated_at",
            rusqlite::params![nest_id, trust_score, reason, now],
        )
        .context("set nest trust")?;
        Ok(())
    }

    /// Detect basic engagement anomalies for a nest.
    ///
    /// Checks:
    /// - **VolumeAnomaly**: total events exceed `volume_threshold`.
    /// - **UniformTiming**: only 1 distinct event type with >10 events
    ///   (real users produce diverse event types).
    pub async fn detect_engagement_anomalies(
        &self,
        nest_id: &[u8; 32],
        volume_threshold: i64,
    ) -> Result<Vec<AnomalyFlag>> {
        let stats = self.get_nest_engagement_stats(nest_id).await?;
        let mut flags = Vec::new();

        if stats.total_events > volume_threshold {
            flags.push(AnomalyFlag::VolumeAnomaly);
        }

        if stats.distinct_event_types == 1 && stats.total_events > 10 {
            flags.push(AnomalyFlag::UniformTiming);
        }

        Ok(flags)
    }
}
