//! The admin's block of one reported item — the store side.
//!
//! Owner: `docs/goal/behavior/moderation.md` § Admin block of a reported item.
//! The block is the legal takedown's withhold with a second **ground**: a
//! nullable flag beside `legal_takedown_ref` on the same two rows —
//! `content_meta.admin_block_report_id` for a post,
//! `segment_records.admin_block_report_id` for a conversation record — whose
//! value is the `abuse_reports.id` the block cites. The report is the block's
//! citation (the legal reference is the takedown's), so its reason is the
//! notice's explanation and every serve path joins the report row to name it.
//!
//! Both transactions here are the takedown's F1/F4 shape
//! (`post_legal_takedown_txn`): the flag `UPDATE` is asserted to match exactly
//! one row, and the flag and its transparency rows land all-or-nothing — a
//! withheld item always has its audit row, and an `"blocked"` reply never names
//! a flag that did not land. **Tombstone, not delete**: only a flag toggles.

use super::CacheDb;
use super::moderation::{
    ABUSE_REPORT_COLUMNS, AbuseReportRow, abuse_report_by_id, abuse_report_from_row,
};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use std::collections::HashSet;

/// What a block names — resolved by the handler from the report's subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminBlockTarget {
    /// A post this nest stores or projects; `author` keys the obligation row.
    Post { post_id: [u8; 32], author: [u8; 32] },
    /// A conversation record, by the record cid the mirror keys on. Its
    /// obligation row is keyed by the record's nest-attested author
    /// (`conv_record_authors`), read inside the block's own transaction; a
    /// record no attestation names writes none, and its appeal stays any
    /// holder's (the takedown's conversation half, the same rule).
    Conversation { record_cid: fauna_cbor::Cid },
}

impl AdminBlockTarget {
    fn read_flag(&self, conn: &rusqlite::Connection) -> Result<Option<String>> {
        match self {
            AdminBlockTarget::Post { post_id, .. } => Ok(conn
                .query_row(
                    "SELECT admin_block_report_id FROM content_meta WHERE content_id = ?1",
                    rusqlite::params![post_id.as_slice()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .context("read content_meta.admin_block_report_id")?
                .flatten()),
            AdminBlockTarget::Conversation { record_cid } => {
                crate::segments::records_db::conv_admin_block_report(conn, record_cid)
            }
        }
    }

    /// Write the flag; `only_if` narrows it to a row blocked under that report.
    fn write_flag(
        &self,
        conn: &rusqlite::Connection,
        report_id: Option<&str>,
        only_if: Option<&str>,
    ) -> Result<usize> {
        match self {
            AdminBlockTarget::Post { post_id, .. } => {
                let n = match only_if {
                    None => conn.execute(
                        "UPDATE content_meta SET admin_block_report_id = ?1 WHERE content_id = ?2",
                        rusqlite::params![report_id, post_id.as_slice()],
                    ),
                    Some(current) => conn.execute(
                        "UPDATE content_meta SET admin_block_report_id = ?1
                          WHERE content_id = ?2 AND admin_block_report_id = ?3",
                        rusqlite::params![report_id, post_id.as_slice(), current],
                    ),
                }
                .context("write content_meta.admin_block_report_id")?;
                Ok(n)
            }
            AdminBlockTarget::Conversation { record_cid } => {
                crate::segments::records_db::set_conv_admin_block(
                    conn, record_cid, report_id, only_if,
                )
            }
        }
    }
}

/// The outcome of [`CacheDb::admin_block_txn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminBlockWrite {
    /// The flag, the transparency rows and the report's resolution landed; the
    /// resolved report row rides along for the reporter's notification, and the
    /// author's queue row id (when one was written) for its delivery to a
    /// foreign author's home.
    Blocked {
        report: Box<AbuseReportRow>,
        obligation_row: Option<i64>,
    },
    /// A replay: the report is resolved and the item is already blocked under
    /// it — nothing was written.
    AlreadyBlocked,
    /// The report is no longer open (resolved otherwise, or withdrawn) — a
    /// block cites an open report.
    ReportNotOpen,
}

/// The outcome of [`CacheDb::admin_unblock_txn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminUnblockWrite {
    /// The flag cleared and the overturn audited.
    Restored,
    /// A replay: the report's block was already lifted — nothing was written.
    AlreadyRestored,
    /// The item is blocked under a different report, or this report never
    /// blocked it — nothing to lift through this one.
    NotBlockedByThisReport,
}

/// One currently-blocked item, for `fauna.moderation.admin_block.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminBlockListRow {
    /// The report the block cites (its subject, reason and resolution time).
    pub report: AbuseReportRow,
}

impl CacheDb {
    /// Atomically block `target` under the open report `report_id`
    /// (`moderation.md` § Admin block of a reported item): the flag, the post's
    /// owed web render, the author's `ObligationAction::Blocked` row (a post,
    /// or a conversation record whose author the nest attested), the `moderation:admin-block` audit row and the report resolved
    /// `acted` by `admin` — ONE transaction, every write asserted. `now_us` is
    /// microseconds.
    #[allow(clippy::too_many_arguments)]
    pub async fn admin_block_txn(
        &self,
        target: &AdminBlockTarget,
        content_id_hex: &str,
        report_id: &str,
        reason_token: &str,
        admin: &[u8; 32],
        audit_detail: &str,
        now_us: i64,
    ) -> Result<AdminBlockWrite> {
        let conn = self.conn.lock().await;
        let report = abuse_report_by_id(&conn, report_id)
            .context("admin_block_txn: read report")?
            .context("admin_block_txn: the report vanished")?;
        if report.status != "open" {
            let replay = report.status == "resolved"
                && report.outcome.as_deref() == Some("acted")
                && target.read_flag(&conn)?.as_deref() == Some(report_id);
            return Ok(if replay {
                AdminBlockWrite::AlreadyBlocked
            } else {
                AdminBlockWrite::ReportNotOpen
            });
        }
        let tx = conn
            .unchecked_transaction()
            .context("begin admin-block tx")?;
        let n = target.write_flag(&tx, Some(report_id), None)?;
        if n != 1 {
            // Dropping `tx` rolls back: no audit, obligation or resolution row
            // can claim a block whose flag did not land.
            anyhow::bail!(
                "admin-block flag write matched {n} rows for {content_id_hex} (want exactly 1); rolled back"
            );
        }
        // The author-queue row (`fauna.moderation.actions`) — the appeal
        // handle, `category` the report's reason.
        let queued = match target {
            AdminBlockTarget::Post { post_id, author } => {
                // The rendered site still carries the body this transaction
                // just withheld; the owed render rides the commit, as for a
                // takedown.
                super::web::mark_web_render_owed_for_post(&tx, post_id)?;
                Some(("post", *author))
            }
            AdminBlockTarget::Conversation { record_cid } => {
                crate::segments::records_db::conv_record_author(&tx, record_cid)?
                    .map(|author| ("conversation", author))
            }
        };
        let obligation_row = match queued {
            Some((content_type, author)) => Some(super::moderation::insert_enforcement_row(
                &tx,
                content_type,
                content_id_hex,
                &author,
                admin.as_slice(),
                reason_token,
                fauna_core::obligation::ObligationAction::Blocked,
                now_us,
            )?),
            None => None,
        };
        super::admin::audit_on_conn(
            &tx,
            None,
            "moderation:admin-block",
            Some(content_id_hex),
            Some(audit_detail),
        )?;
        let resolved = tx
            .execute(
                "UPDATE abuse_reports
                    SET status = 'resolved', outcome = 'acted', resolved_at = ?2, resolved_by = ?3
                  WHERE id = ?1 AND status = 'open'",
                rusqlite::params![report_id, now_us, admin.as_slice()],
            )
            .context("resolve the blocked report")?;
        if resolved != 1 {
            anyhow::bail!(
                "admin-block: the report left the open state mid-transaction; rolled back"
            );
        }
        let row = abuse_report_by_id(&tx, report_id)
            .context("admin_block_txn: read back")?
            .context("admin_block_txn: report vanished")?;
        tx.commit().context("commit admin-block tx")?;
        Ok(AdminBlockWrite::Blocked {
            report: Box::new(row),
            obligation_row,
        })
    }

    /// Atomically lift the block `report_id` placed on `target`: the flag
    /// cleared (asserted to one row still blocked under that report) and the
    /// `moderation:admin-block-restore` audit row. The report stays resolved
    /// and the obligation row stays as history.
    pub async fn admin_unblock_txn(
        &self,
        target: &AdminBlockTarget,
        content_id_hex: &str,
        report_id: &str,
        audit_detail: &str,
    ) -> Result<AdminUnblockWrite> {
        let conn = self.conn.lock().await;
        match target.read_flag(&conn)? {
            Some(current) if current == report_id => {}
            Some(_) => return Ok(AdminUnblockWrite::NotBlockedByThisReport),
            None => {
                let acted = abuse_report_by_id(&conn, report_id)
                    .context("admin_unblock_txn: read report")?
                    .is_some_and(|r| r.outcome.as_deref() == Some("acted"));
                return Ok(if acted {
                    AdminUnblockWrite::AlreadyRestored
                } else {
                    AdminUnblockWrite::NotBlockedByThisReport
                });
            }
        }
        let tx = conn
            .unchecked_transaction()
            .context("begin admin-unblock tx")?;
        let n = target.write_flag(&tx, None, Some(report_id))?;
        if n != 1 {
            anyhow::bail!(
                "admin-unblock flag write matched {n} rows for {content_id_hex} (want exactly 1); rolled back"
            );
        }
        super::admin::audit_on_conn(
            &tx,
            None,
            "moderation:admin-block-restore",
            Some(content_id_hex),
            Some(audit_detail),
        )?;
        tx.commit().context("commit admin-unblock tx")?;
        Ok(AdminUnblockWrite::Restored)
    }

    /// The reason token of the report a post is blocked under, or `None` when
    /// it is not blocked — the serve-time read behind `get_post_core`'s
    /// author-exempt withhold. A blocked post whose report row cannot be read
    /// still reads as blocked, under `other`: the flag is the withhold, the
    /// reason only its explanation.
    pub async fn get_post_admin_block(&self, post_id: &[u8; 32]) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COALESCE(ar.reason, 'other')
               FROM content_meta cm
               LEFT JOIN abuse_reports ar ON ar.id = cm.admin_block_report_id
              WHERE cm.content_id = ?1 AND cm.admin_block_report_id IS NOT NULL",
            rusqlite::params![post_id.as_slice()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .context("get_post_admin_block")
    }

    /// Every currently-blocked item's report, newest block first — the
    /// `admin-nest-blocked-section` list. A block whose report row is gone is
    /// not listed (nothing to cite an unblock with); none exists on any path
    /// today, as a report row is never deleted while it backs a block.
    pub async fn list_admin_blocks(&self) -> Result<Vec<AdminBlockListRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports
                  WHERE id IN (
                        SELECT admin_block_report_id FROM content_meta
                         WHERE admin_block_report_id IS NOT NULL
                        UNION
                        SELECT admin_block_report_id FROM segment_records
                         WHERE kind = 'conv' AND admin_block_report_id IS NOT NULL)
                  ORDER BY resolved_at DESC, id"
            ))
            .context("prepare list_admin_blocks")?;
        let rows = stmt
            .query_map([], abuse_report_from_row)
            .context("query list_admin_blocks")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read list_admin_blocks")?;
        Ok(rows
            .into_iter()
            .map(|report| AdminBlockListRow { report })
            .collect())
    }

    /// The post ids an admin block currently withholds — the blob withhold's
    /// second input ([`crate::moderation_withhold`]), beside
    /// [`Self::taken_down_post_ids`].
    pub async fn admin_blocked_post_ids(&self) -> Result<HashSet<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT content_id FROM content_meta WHERE admin_block_report_id IS NOT NULL")
            .context("prepare admin_blocked_post_ids")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .context("query admin_blocked_post_ids")?;
        let mut out = HashSet::new();
        for row in rows {
            let id = row.context("read content_meta.content_id")?;
            if let Ok(arr) = <[u8; 32]>::try_from(id.as_slice()) {
                out.insert(arr);
            }
        }
        Ok(out)
    }

    /// Seed a post's admin-block flag without its transparency rows — a
    /// test-seeding / serve-gate helper only, like
    /// [`Self::set_post_legal_takedown`]; the handler uses
    /// [`Self::admin_block_txn`].
    pub async fn set_post_admin_block(
        &self,
        post_id: &[u8; 32],
        report_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE content_meta SET admin_block_report_id = ?1 WHERE content_id = ?2",
            rusqlite::params![report_id, post_id.as_slice()],
        )?;
        Ok(())
    }

    /// Seed a conv record's admin-block flag — the test twin of
    /// [`Self::set_post_admin_block`].
    pub async fn set_conv_admin_block(
        &self,
        record_cid: &fauna_cbor::Cid,
        report_id: Option<&str>,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::set_conv_admin_block(&conn, record_cid, report_id, None)
    }
}
