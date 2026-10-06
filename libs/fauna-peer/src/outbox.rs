//! Message outbox — tracks outbound messages and their delivery status.

use crate::delivery::DeliveryStatus;
use anyhow::Result;
use rusqlite::{Connection, params};

pub struct Outbox {
    conn: Connection,
}

impl Outbox {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbox (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                recipient_actor_id  BLOB NOT NULL,
                message_hash        TEXT NOT NULL,
                message_body        TEXT NOT NULL,
                status              TEXT NOT NULL DEFAULT 'queued',
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                attempts            INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn enqueue(&self, recipient: &[u8; 32], hash: &str, body: &str) -> Result<i64> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO outbox (recipient_actor_id, message_hash, message_body, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?4)",
            params![recipient.as_slice(), hash, body, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_status(&self, id: i64, status: DeliveryStatus) -> Result<()> {
        let status_str = status_to_str(status);
        self.conn.execute(
            "UPDATE outbox SET status = ?1, updated_at = ?2, attempts = attempts + 1 WHERE id = ?3",
            params![status_str, now_secs(), id],
        )?;
        Ok(())
    }

    pub fn pending_for(&self, recipient: &[u8; 32]) -> Result<Vec<OutboxEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, recipient_actor_id, message_hash, message_body, status, created_at, updated_at, attempts
             FROM outbox WHERE recipient_actor_id = ?1 AND status IN ('queued', 'wake_sent')
             ORDER BY created_at"
        )?;
        let entries = stmt
            .query_map(params![recipient.as_slice()], row_to_entry)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    pub fn all_pending(&self) -> Result<Vec<OutboxEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, recipient_actor_id, message_hash, message_body, status, created_at, updated_at, attempts
             FROM outbox WHERE status IN ('queued', 'wake_sent')
             ORDER BY created_at"
        )?;
        let entries = stmt
            .query_map([], row_to_entry)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }
}

#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub id: i64,
    pub recipient_actor_id: [u8; 32],
    pub message_hash: String,
    pub message_body: String,
    pub status: DeliveryStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub attempts: i32,
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

fn status_to_str(s: DeliveryStatus) -> &'static str {
    match s {
        DeliveryStatus::Queued => "queued",
        DeliveryStatus::SentP2P => "sent_p2p",
        DeliveryStatus::SentNest => "sent_nest",
        DeliveryStatus::WakeSent => "wake_sent",
        DeliveryStatus::Failed => "failed",
    }
}

fn str_to_status(s: &str) -> DeliveryStatus {
    match s {
        "sent_p2p" => DeliveryStatus::SentP2P,
        "sent_nest" => DeliveryStatus::SentNest,
        "wake_sent" => DeliveryStatus::WakeSent,
        "failed" => DeliveryStatus::Failed,
        _ => DeliveryStatus::Queued,
    }
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<OutboxEntry> {
    let id: i64 = row.get(0)?;
    let recipient_blob: Vec<u8> = row.get(1)?;
    let recipient: [u8; 32] = recipient_blob.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            1,
            "recipient_actor_id".into(),
            rusqlite::types::Type::Blob,
        )
    })?;
    let status_str: String = row.get(4)?;
    Ok(OutboxEntry {
        id,
        recipient_actor_id: recipient,
        message_hash: row.get(2)?,
        message_body: row.get(3)?,
        status: str_to_status(&status_str),
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        attempts: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_outbox() -> Outbox {
        Outbox::open(":memory:").unwrap()
    }

    #[test]
    fn enqueue_and_query() {
        let outbox = test_outbox();
        let id = outbox
            .enqueue(&[1u8; 32], "hash123", r#"{"text":"hello"}"#)
            .unwrap();
        let pending = outbox.pending_for(&[1u8; 32]).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].status, DeliveryStatus::Queued);
    }

    #[test]
    fn update_status_to_sent() {
        let outbox = test_outbox();
        let id = outbox.enqueue(&[1u8; 32], "hash123", "body").unwrap();
        outbox.update_status(id, DeliveryStatus::SentP2P).unwrap();
        let pending = outbox.pending_for(&[1u8; 32]).unwrap();
        assert_eq!(pending.len(), 0); // SentP2P is not pending
    }

    #[test]
    fn all_pending_across_recipients() {
        let outbox = test_outbox();
        outbox.enqueue(&[1u8; 32], "h1", "b1").unwrap();
        outbox.enqueue(&[2u8; 32], "h2", "b2").unwrap();
        let all = outbox.all_pending().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn row_to_entry_errors_on_malformed_recipient_blob() {
        let outbox = test_outbox();
        outbox
            .conn
            .execute(
                "INSERT INTO outbox (recipient_actor_id, message_hash, message_body, status, created_at, updated_at)
                 VALUES (X'ffee', 'hash', 'body', 'queued', 1, 1)",
                [],
            )
            .unwrap();

        let result: rusqlite::Result<OutboxEntry> = outbox.conn.query_row(
            "SELECT id, recipient_actor_id, message_hash, message_body, status, created_at, updated_at, attempts
             FROM outbox WHERE message_hash = 'hash'",
            [],
            row_to_entry,
        );

        assert!(
            result.is_err(),
            "a malformed (non-32-byte) recipient_actor_id blob must not silently zero-fill"
        );
    }
}
