//! Fresh-IP outbound warm-up state
//! (`docs/goal/behavior/mail-deliverability.md` § Fresh-IP warm-up → § State
//! model + § Enforcement at submission time + § Manual reset).
//!
//! ONE deployment-wide row in `mail_outbound_warmup_state` (the warm-up is the
//! deployment's shared outbound-IP reputation, not per-actor). The cap *curve*
//! lives in shared `fauna_mail::warmup`; this module owns the *state*: the
//! day-counter, the lazy 00:00-UTC daily reset of `mails_sent_today`, the
//! atomic check-and-consume at submission time, and the admin reset/read.
//!
//! Timestamps are Unix **seconds** (`now_epoch_secs` from the handler / the
//! `outbound_now` test-clock seam). All three methods normalise the cached
//! `current_day` + the daily counter against `now` under the connection lock,
//! so a stale row never leaks a wrong cap.

use super::CacheDb;
use anyhow::{Context, Result};
use fauna_mail::warmup::{current_day_for, max_for_day};

/// The submission-time warm-up decision for one message
/// (`mail-deliverability.md` § Enforcement at submission time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmupDecision {
    /// Under today's cap — the recipients counted against `mails_sent_today`;
    /// enqueue normally (`next_attempt_at = now`).
    Allowed,
    /// Over today's cap — the message is **queued for tomorrow** (the caller
    /// stamps `next_attempt_at = fauna_mail::warmup::next_utc_midnight(now)`);
    /// `mails_sent_today` is left unchanged, `deferred_total` is bumped.
    Deferred,
}

/// A normalised snapshot of the warm-up state for the admin status RPC
/// (`mail-deliverability.md` § Wire shapes → `outbound_warmup_status`). `0`
/// encodes "never" for the two nullable timestamps; `today_max = None` is the
/// day-30+ unlimited state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmupStatus {
    pub current_day: i64,
    pub today_used: i64,
    pub today_max: Option<i64>,
    pub ramp_end_date: i64,
    pub lifetime_total: i64,
    pub first_outbound_at: i64,
    pub last_reset_at: i64,
}

const DAY_SECS: i64 = 86_400;

/// 00:00-UTC epoch-secs of the warm-up's final ramp day
/// ([`fauna_mail::warmup::WARMUP_UNLIMITED_DAY`]); `0` if no outbound yet.
fn ramp_end_date(first_outbound_at: Option<i64>) -> i64 {
    match first_outbound_at {
        Some(first) => {
            let first_day = first.div_euclid(DAY_SECS);
            (first_day + (fauna_mail::warmup::WARMUP_UNLIMITED_DAY as i64 - 1)) * DAY_SECS
        }
        None => 0,
    }
}

/// The raw row fields, before normalisation.
struct WarmupRow {
    first_outbound_at: Option<i64>,
    mails_sent_today: i64,
    mails_sent_total: i64,
    counter_epoch_day: i64,
    deferred_total: i64,
    last_reset_at: Option<i64>,
}

impl CacheDb {
    /// Read (creating the single row if absent) the raw warm-up row.
    fn read_warmup_row(conn: &rusqlite::Connection) -> Result<WarmupRow> {
        conn.execute(
            "INSERT OR IGNORE INTO mail_outbound_warmup_state (id) VALUES (1)",
            [],
        )
        .context("ensure warmup row")?;
        conn.query_row(
            "SELECT first_outbound_at, mails_sent_today, mails_sent_total, \
                    counter_epoch_day, deferred_total, last_reset_at \
             FROM mail_outbound_warmup_state WHERE id = 1",
            [],
            |r| {
                Ok(WarmupRow {
                    first_outbound_at: r.get(0)?,
                    mails_sent_today: r.get(1)?,
                    mails_sent_total: r.get(2)?,
                    counter_epoch_day: r.get(3)?,
                    deferred_total: r.get(4)?,
                    last_reset_at: r.get(5)?,
                })
            },
        )
        .context("read warmup row")
    }

    /// Atomically apply the warm-up cap to a submission of `recipient_count`
    /// external recipients at `now` (Unix seconds), per `mail-deliverability.md`
    /// § Enforcement at submission time:
    ///
    /// 1. Lazily reset `mails_sent_today` to 0 on a 00:00-UTC day rollover.
    /// 2. Stamp `first_outbound_at = now` on the deployment's first ever
    ///    outbound mail; derive `current_day` from it.
    /// 3. `today_used = mails_sent_today + recipient_count`; if it exceeds
    ///    `max_for_day(current_day)` → [`WarmupDecision::Deferred`] (counter
    ///    unchanged, `deferred_total += recipient_count`); else
    ///    [`WarmupDecision::Allowed`] (`mails_sent_today`/`mails_sent_total`
    ///    both `+= recipient_count`). Day 30+ (`None` cap) is always Allowed.
    pub async fn try_consume_warmup(
        &self,
        now: i64,
        recipient_count: u32,
    ) -> Result<WarmupDecision> {
        let conn = self.conn.lock().await;
        let row = Self::read_warmup_row(&conn)?;

        let today_epoch_day = now.div_euclid(DAY_SECS);
        // First-ever outbound stamps first_outbound_at = now.
        let first_outbound_at = row.first_outbound_at.unwrap_or(now);
        // Lazy daily reset: a new UTC day zeroes the today-counter.
        let rolled_over = row.counter_epoch_day != today_epoch_day;
        let mails_sent_today = if rolled_over { 0 } else { row.mails_sent_today };

        let current_day = current_day_for(first_outbound_at, now);
        let cap = max_for_day(current_day).map(|c| c as i64);
        let today_used = mails_sent_today + recipient_count as i64;

        let (decision, new_today, new_total, new_deferred) = match cap {
            Some(max) if today_used > max => (
                WarmupDecision::Deferred,
                mails_sent_today,
                row.mails_sent_total,
                row.deferred_total + recipient_count as i64,
            ),
            _ => (
                WarmupDecision::Allowed,
                today_used,
                row.mails_sent_total + recipient_count as i64,
                row.deferred_total,
            ),
        };

        conn.execute(
            "UPDATE mail_outbound_warmup_state SET \
                first_outbound_at = ?1, current_day = ?2, mails_sent_today = ?3, \
                mails_sent_total = ?4, counter_epoch_day = ?5, deferred_total = ?6 \
             WHERE id = 1",
            rusqlite::params![
                first_outbound_at,
                current_day as i64,
                new_today,
                new_total,
                today_epoch_day,
                new_deferred,
            ],
        )
        .context("update warmup state")?;
        Ok(decision)
    }

    /// Read the normalised warm-up status (`mail-deliverability.md` § Wire
    /// shapes → `outbound_warmup_status`). Normalises `current_day` + the daily
    /// counter against `now` and persists the normalisation so an external
    /// reader sees a fresh row. No counters are consumed.
    pub async fn read_warmup_status(&self, now: i64) -> Result<WarmupStatus> {
        let conn = self.conn.lock().await;
        let row = Self::read_warmup_row(&conn)?;
        let today_epoch_day = now.div_euclid(DAY_SECS);
        let rolled_over = row.counter_epoch_day != today_epoch_day;
        let mails_sent_today = if rolled_over { 0 } else { row.mails_sent_today };
        // current_day only advances once first_outbound_at is set; before any
        // outbound it stays at day 1 (cap 50, nothing used).
        let current_day = match row.first_outbound_at {
            Some(first) => current_day_for(first, now),
            None => 1,
        };
        if rolled_over || row.first_outbound_at.is_some() {
            conn.execute(
                "UPDATE mail_outbound_warmup_state SET \
                    current_day = ?1, mails_sent_today = ?2, counter_epoch_day = ?3 \
                 WHERE id = 1",
                rusqlite::params![current_day as i64, mails_sent_today, today_epoch_day],
            )
            .context("normalise warmup status")?;
        }
        Ok(WarmupStatus {
            current_day: current_day as i64,
            today_used: mails_sent_today,
            today_max: max_for_day(current_day).map(|c| c as i64),
            ramp_end_date: ramp_end_date(row.first_outbound_at),
            lifetime_total: row.mails_sent_total,
            first_outbound_at: row.first_outbound_at.unwrap_or(0),
            last_reset_at: row.last_reset_at.unwrap_or(0),
        })
    }

    /// Admin manual reset (`mail-deliverability.md` § Manual reset) — used only
    /// after the deployment's outbound IP changed (e.g. a VPS migration):
    /// `first_outbound_at = now`, `current_day = 1`, `mails_sent_today = 0`,
    /// `last_reset_at = now`. The lifetime `mails_sent_total` (and the
    /// `deferred_total` metric) are **preserved**. Returns the post-reset status.
    pub async fn reset_warmup(&self, now: i64) -> Result<WarmupStatus> {
        {
            let conn = self.conn.lock().await;
            Self::read_warmup_row(&conn)?; // ensure the row exists
            conn.execute(
                "UPDATE mail_outbound_warmup_state SET \
                    first_outbound_at = ?1, current_day = 1, mails_sent_today = 0, \
                    counter_epoch_day = ?2, last_reset_at = ?1 \
                 WHERE id = 1",
                rusqlite::params![now, now.div_euclid(DAY_SECS)],
            )
            .context("reset warmup state")?;
        }
        self.read_warmup_status(now).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const DAY: i64 = 86_400;

    #[tokio::test]
    async fn first_submission_stamps_day_one_and_counts() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 1000 * DAY + 12 * 3600; // day 1000, noon
        assert_eq!(
            db.try_consume_warmup(now, 10).await.unwrap(),
            WarmupDecision::Allowed
        );
        let s = db.read_warmup_status(now).await.unwrap();
        assert_eq!(s.current_day, 1);
        assert_eq!(s.today_used, 10);
        assert_eq!(s.today_max, Some(50));
        assert_eq!(s.lifetime_total, 10);
        assert_eq!(s.first_outbound_at, now);
        assert_eq!(s.last_reset_at, 0);
        assert_eq!(s.ramp_end_date, (1000 + 29) * DAY);
    }

    #[tokio::test]
    async fn over_cap_defers_without_counting_and_bumps_deferred() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 5 * DAY; // day 1, cap 50
        // Consume up to the cap.
        assert_eq!(
            db.try_consume_warmup(now, 50).await.unwrap(),
            WarmupDecision::Allowed
        );
        // One more recipient → today_used 51 > 50 → deferred, counter unchanged.
        assert_eq!(
            db.try_consume_warmup(now, 1).await.unwrap(),
            WarmupDecision::Deferred
        );
        let s = db.read_warmup_status(now).await.unwrap();
        assert_eq!(s.today_used, 50, "deferred recipients must not count today");
        assert_eq!(s.lifetime_total, 50);
    }

    #[tokio::test]
    async fn daily_counter_resets_at_utc_midnight_but_day_advances() {
        let db = CacheDb::open_in_memory().unwrap();
        let d1 = 7 * DAY + 3600; // day 1
        db.try_consume_warmup(d1, 40).await.unwrap();
        assert_eq!(db.read_warmup_status(d1).await.unwrap().today_used, 40);
        // Next UTC day: counter resets to 0, current_day → 2 (cap 100).
        let d2 = 8 * DAY + 3600;
        let s = db.read_warmup_status(d2).await.unwrap();
        assert_eq!(s.today_used, 0);
        assert_eq!(s.current_day, 2);
        assert_eq!(s.today_max, Some(100));
        // Day-2 submission counts against the fresh day.
        db.try_consume_warmup(d2, 100).await.unwrap();
        assert_eq!(db.read_warmup_status(d2).await.unwrap().today_used, 100);
    }

    #[tokio::test]
    async fn day_30_is_unlimited_never_defers() {
        let db = CacheDb::open_in_memory().unwrap();
        let first = 0; // day 1 = epoch day 0
        db.try_consume_warmup(first, 1).await.unwrap();
        let day30 = 29 * DAY; // 30th day
        assert_eq!(
            db.try_consume_warmup(day30, 1_000_000).await.unwrap(),
            WarmupDecision::Allowed
        );
        let s = db.read_warmup_status(day30).await.unwrap();
        assert_eq!(s.current_day, 30);
        assert_eq!(s.today_max, None);
    }

    #[tokio::test]
    async fn reset_restarts_day_one_preserving_lifetime() {
        let db = CacheDb::open_in_memory().unwrap();
        let d1 = 100 * DAY;
        db.try_consume_warmup(d1, 30).await.unwrap();
        let later = 110 * DAY; // day 11
        let s = db.reset_warmup(later).await.unwrap();
        assert_eq!(s.current_day, 1, "reset restarts the ramp at day 1");
        assert_eq!(s.today_used, 0);
        assert_eq!(s.today_max, Some(50));
        assert_eq!(s.lifetime_total, 30, "lifetime total is preserved");
        assert_eq!(s.first_outbound_at, later);
        assert_eq!(s.last_reset_at, later);
    }
}
