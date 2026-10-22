//! The nest admin's published rules at rest — the `nest_content_policy`
//! singleton (`docs/goal/behavior/nest-rules.md` § The nest content policy →
//! *Nest state, written in the app*).
//!
//! Nothing here validates or signs: the handler
//! (`crate::content_policy_handlers`) validates the document with the shared
//! grammar and signs it with the nest's identity key; this module stores what
//! it was handed and answers it back byte for byte, so a restart re-serves the
//! very bytes the nest signed.
//!
//! **The sequence is the arbiter.** A write names the sequence it read
//! ([`CacheDb::put_nest_content_policy`]'s `read_sequence`) and lands only if
//! the row still holds it, inside one transaction — two admins writing at once
//! can never both stamp the same sequence over different documents, and the
//! sequence a member compares never runs backwards.

use anyhow::{Context, Result};
use fauna_protocol::content_policy::SignedContentPolicy;
use rusqlite::OptionalExtension;

use super::CacheDb;

/// The audit action of an accepted write (`nest-rules.md` § The nest content
/// policy; `moderation.md`'s `moderation:` family).
pub const AUDIT_CONTENT_POLICY_SET: &str = "moderation:content-policy-set";

/// The rules in force, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredNestContentPolicy {
    /// The nest domain the document was validated for — its rules' publisher,
    /// `nest:<domain>`, which every factor a bundled scorer produces names.
    pub domain: String,
    pub policy: SignedContentPolicy,
}

impl CacheDb {
    /// The rules in force, or `None` on a nest that has never published any.
    pub async fn get_nest_content_policy(&self) -> Result<Option<StoredNestContentPolicy>> {
        let conn = self.conn.lock().await;
        read(&conn)
    }

    /// Store an accepted write and audit it, in one transaction — only if the
    /// row still holds `read_sequence` (`None`: still absent). Returns whether
    /// the write landed; `false` means another write took the row since the
    /// caller read it, and the caller re-reads and re-stamps.
    ///
    /// `mark_publishing_renders` owes every publishing site a render in the
    /// same transaction — the region declaration's reason
    /// ([`CacheDb::set_declared_region`]): the publisher leg folds this document
    /// over the public pages, which are static, and a restart between this
    /// commit and the re-render walk must not leave them showing the old rules.
    pub async fn put_nest_content_policy(
        &self,
        stored: &StoredNestContentPolicy,
        read_sequence: Option<u64>,
        actor_id: &[u8; 32],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin put_nest_content_policy tx")?;
        let current = read(&tx)?.map(|row| row.policy.sequence);
        if current != read_sequence {
            return Ok(false);
        }
        let sequence =
            i64::try_from(stored.policy.sequence).context("content-policy sequence overflows")?;
        tx.execute(
            "INSERT INTO nest_content_policy (id, domain, document, sequence, issued_at, signature)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                domain = excluded.domain,
                document = excluded.document,
                sequence = excluded.sequence,
                issued_at = excluded.issued_at,
                signature = excluded.signature",
            rusqlite::params![
                stored.domain,
                stored.policy.document,
                sequence,
                stored.policy.issued_at,
                stored.policy.signature,
            ],
        )
        .context("store nest content policy")?;
        super::admin::audit_on_conn(
            &tx,
            Some(&actor_id[..]),
            AUDIT_CONTENT_POLICY_SET,
            Some(&stored.policy.sequence.to_string()),
            Some(&stored.domain),
        )?;
        super::web::mark_web_render_owed_for_publishing_actors(&tx)?;
        tx.commit().context("commit put_nest_content_policy")?;
        Ok(true)
    }
}

fn read(conn: &rusqlite::Connection) -> Result<Option<StoredNestContentPolicy>> {
    let row = conn
        .query_row(
            "SELECT domain, document, sequence, issued_at, signature
             FROM nest_content_policy WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .context("read nest content policy")?;
    let Some((domain, document, sequence, issued_at, signature)) = row else {
        return Ok(None);
    };
    Ok(Some(StoredNestContentPolicy {
        domain,
        policy: SignedContentPolicy {
            document,
            sequence: u64::try_from(sequence)
                .context("stored content-policy sequence is negative")?,
            issued_at,
            signature,
            extra: Default::default(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(sequence: u64, document: &[u8]) -> StoredNestContentPolicy {
        StoredNestContentPolicy {
            domain: "rules.example".into(),
            policy: SignedContentPolicy {
                document: document.to_vec(),
                sequence,
                issued_at: 1_700_000_000,
                signature: vec![7; 64],
                extra: Default::default(),
            },
        }
    }

    /// A write lands only over the sequence it read, and what is stored is
    /// answered back byte for byte.
    #[tokio::test]
    async fn a_write_lands_only_over_the_sequence_it_read() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        assert_eq!(db.get_nest_content_policy().await.unwrap(), None);

        assert!(
            db.put_nest_content_policy(&stored(1, b"one"), None, &actor)
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_nest_content_policy().await.unwrap(),
            Some(stored(1, b"one"))
        );

        // A second writer that read the absent row lost the race.
        assert!(
            !db.put_nest_content_policy(&stored(1, b"other"), None, &actor)
                .await
                .unwrap()
        );
        assert!(
            db.put_nest_content_policy(&stored(2, b"two"), Some(1), &actor)
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_nest_content_policy().await.unwrap(),
            Some(stored(2, b"two"))
        );
    }
}
