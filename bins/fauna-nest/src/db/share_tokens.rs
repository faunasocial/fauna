//! Share-link control-plane registry: client-minted `ShareToken` metadata.
//!
//! A ShareToken is client-minted, stateless and self-verifying
//! (`fauna_core::share::ShareToken`) — the nest never holds a user's signing
//! key. This registry records a minted token's metadata so its author can
//! enumerate their live shares (`fauna.share.list`), revoke one
//! (`fauna.share.revoke`), and so `GET /share/{token}` can refuse a revoked
//! token with `410 Gone`. The PK `token_id` is `blake3` of the canonical signed
//! wire bytes, derived identically by the registering handler and the GET path
//! (`fauna_core::share::token_id_from_base64url`).
//!
//! Registration is **idempotent** (INSERT OR IGNORE on `token_id`) and **not** a
//! serving gate for a **live** author: an unregistered token still serves
//! statelessly. After the author's identity succession, a token serves only
//! through its registry row — an unregistered one refuses `410`
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope, ruled
//! 2026-08-15; `share_routes.rs` step 3b). Revocation is **sticky** —
//! re-registering an already-revoked token returns the revoked row rather than
//! reviving it (the signed bytes are the same physical capability that was
//! revoked, so un-revoking via re-register would be a bypass).
//!
//! Spec: `docs/goal/architecture/api-layers.md` § Share.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::CacheDb;

/// A row from the `share_tokens` table.
pub struct ShareTokenRow {
    pub token_id: [u8; 32],
    pub author: [u8; 32],
    pub manifest_hash: [u8; 32],
    /// Token expiry, Unix seconds (the ShareToken's `expires`).
    pub expires_at: i64,
    pub public: bool,
    pub revoked: bool,
    /// Server-side registration time, Unix seconds.
    pub created_at: i64,
    /// A fragment-keyed private link: the row carries a key envelope.
    pub key_in_fragment: bool,
    /// The author's sealed copy of the filename (`share-links.md` § The
    /// filename rests sealed) — opaque to the nest, and the only form the
    /// name rests in.
    pub filename_sealed: Vec<u8>,
}

impl CacheDb {
    /// Register a minted token's metadata, returning the stored row.
    ///
    /// Idempotent on `token_id` (INSERT OR IGNORE): a re-register of an existing
    /// token leaves the original `created_at` / `revoked` untouched and returns
    /// the stored row. `created_at` is set to now on first insert.
    ///
    /// `filename_sealed` is the author's sealed copy of the name, stored
    /// opaque; the table holds the name in no other form.
    pub async fn register_share_token(
        &self,
        token_id: &[u8; 32],
        author: &[u8; 32],
        manifest_hash: &[u8; 32],
        filename_sealed: &[u8],
        expires_at: i64,
        public: bool,
    ) -> Result<ShareTokenRow> {
        self.insert_share_token(
            token_id,
            author,
            manifest_hash,
            filename_sealed,
            expires_at,
            public,
            None,
        )
        .await
    }

    /// [`Self::register_share_token`] for a fragment-keyed private link: the
    /// same idempotent insert, carrying the link's sealed key envelope in the
    /// one statement, so no registered private link ever lacks its envelope. A
    /// re-register keeps the first envelope (the token id is the same physical
    /// link).
    pub async fn register_private_share_token(
        &self,
        token_id: &[u8; 32],
        author: &[u8; 32],
        manifest_hash: &[u8; 32],
        filename_sealed: &[u8],
        expires_at: i64,
        key_envelope: &[u8],
    ) -> Result<ShareTokenRow> {
        self.insert_share_token(
            token_id,
            author,
            manifest_hash,
            filename_sealed,
            expires_at,
            true,
            Some(key_envelope),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_share_token(
        &self,
        token_id: &[u8; 32],
        author: &[u8; 32],
        manifest_hash: &[u8; 32],
        filename_sealed: &[u8],
        expires_at: i64,
        public: bool,
        key_envelope: Option<&[u8]>,
    ) -> Result<ShareTokenRow> {
        let token_vec = token_id.to_vec();
        let author_vec = author.to_vec();
        let manifest_vec = manifest_hash.to_vec();
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO share_tokens
                 (token_id, author, manifest_hash, expires_at, public, revoked, created_at,
                  key_envelope, filename_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)",
            rusqlite::params![
                token_vec,
                author_vec,
                manifest_vec,
                expires_at,
                public as i64,
                now,
                key_envelope,
                filename_sealed
            ],
        )
        .context("insert share token")?;
        conn.query_row(
            &format!("SELECT {ROW_COLUMNS} FROM share_tokens WHERE token_id = ?1"),
            rusqlite::params![token_vec],
            parse_row,
        )
        .context("read back share token")
    }

    /// A registered private link's sealed key envelope: `None` for a token
    /// never registered or registered without one (every public link). The
    /// serve path's private arm reads it; the nest never opens it.
    pub async fn share_token_key_envelope(&self, token_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let token_vec = token_id.to_vec();
        let conn = self.conn.lock().await;
        let envelope: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT key_envelope FROM share_tokens WHERE token_id = ?1",
                rusqlite::params![token_vec],
                |row| row.get(0),
            )
            .optional()
            .context("read share token key envelope")?;
        Ok(envelope.flatten())
    }

    /// Return all share tokens registered by `author`, newest first.
    pub async fn list_share_tokens_for_author(
        &self,
        author: &[u8; 32],
    ) -> Result<Vec<ShareTokenRow>> {
        let author_vec = author.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ROW_COLUMNS}
                 FROM share_tokens
                 WHERE author = ?1
                 ORDER BY created_at DESC, rowid DESC"
            ))
            .context("prepare list_share_tokens_for_author")?;
        let rows = stmt
            .query_map(rusqlite::params![author_vec], parse_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query list_share_tokens_for_author")?;
        Ok(rows)
    }

    /// Mark a token revoked. Scoped to `author`: a token owned by another actor
    /// is not affected. Returns `true` iff a row owned by `author` was updated
    /// (an already-revoked row still counts — the UPDATE matches it).
    pub async fn revoke_share_token(&self, token_id: &[u8; 32], author: &[u8; 32]) -> Result<bool> {
        let token_vec = token_id.to_vec();
        let author_vec = author.to_vec();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE share_tokens SET revoked = 1 WHERE token_id = ?1 AND author = ?2",
                rusqlite::params![token_vec, author_vec],
            )
            .context("revoke share token")?;
        Ok(affected > 0)
    }

    /// The registry's verdict on a token id: `None` when the token was never
    /// registered, `Some(revoked)` for a registered row.
    ///
    /// One fetch answers the serve path's two registry gates: revocation
    /// (`Some(true)` → `410`) and — for a token whose author a succession has
    /// retired — **existence**, the only thing such a token may serve by
    /// (`succession-aftermath.md` § Re-key scope; `share_routes.rs` step 3b).
    /// `None` being a first-class answer rather than "not revoked" is the
    /// point: the gate needs "unregistered" to be distinguishable.
    pub async fn share_token_registration(&self, token_id: &[u8; 32]) -> Result<Option<bool>> {
        let token_vec = token_id.to_vec();
        let conn = self.conn.lock().await;
        let revoked: Option<i64> = conn
            .query_row(
                "SELECT revoked FROM share_tokens WHERE token_id = ?1",
                rusqlite::params![token_vec],
                |row| row.get(0),
            )
            .optional()
            .context("read share token registration")?;
        Ok(revoked.map(|r| r == 1))
    }

    /// Whether a token is registered **and** revoked. An unregistered token
    /// returns `false` (it still serves statelessly for a live author) —
    /// registration is not a serving gate outside the succession case
    /// (`share_token_registration` is the serve path's richer consult).
    pub async fn is_share_token_revoked(&self, token_id: &[u8; 32]) -> Result<bool> {
        Ok(self.share_token_registration(token_id).await? == Some(true))
    }
}

/// The column list [`parse_row`] reads, in its order. The envelope itself is
/// never projected into a row — only whether one exists.
const ROW_COLUMNS: &str = "token_id, author, manifest_hash, expires_at, public, \
                           revoked, created_at, key_envelope IS NOT NULL, filename_sealed";

/// Parse a single `share_tokens` row into a [`ShareTokenRow`].
fn parse_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareTokenRow> {
    let token_bytes: Vec<u8> = row.get(0)?;
    let author_bytes: Vec<u8> = row.get(1)?;
    let manifest_bytes: Vec<u8> = row.get(2)?;
    let expires_at: i64 = row.get(3)?;
    let public: i64 = row.get(4)?;
    let revoked: i64 = row.get(5)?;
    let created_at: i64 = row.get(6)?;
    let key_in_fragment: bool = row.get(7)?;
    let filename_sealed: Vec<u8> = row.get(8)?;

    let token_id: [u8; 32] = token_bytes.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(0, "token_id".into(), rusqlite::types::Type::Blob)
    })?;
    let author: [u8; 32] = author_bytes.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(1, "author".into(), rusqlite::types::Type::Blob)
    })?;
    let manifest_hash: [u8; 32] = manifest_bytes.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(2, "manifest_hash".into(), rusqlite::types::Type::Blob)
    })?;

    Ok(ShareTokenRow {
        token_id,
        author,
        manifest_hash,
        expires_at,
        public: public != 0,
        revoked: revoked != 0,
        created_at,
        key_in_fragment,
        filename_sealed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn register_list_revoke_round_trip() {
        let db = db();
        let author = [1u8; 32];
        let token_id = [2u8; 32];
        let manifest = [3u8; 32];

        let row = db
            .register_share_token(&token_id, &author, &manifest, b"sealed", 9_999, true)
            .await
            .unwrap();
        assert_eq!(row.token_id, token_id);
        assert_eq!(row.author, author);
        assert_eq!(row.manifest_hash, manifest);
        assert_eq!(row.filename_sealed, b"sealed");
        assert_eq!(row.expires_at, 9_999);
        assert!(row.public);
        assert!(!row.revoked);

        let list = db.list_share_tokens_for_author(&author).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].token_id, token_id);

        assert!(!db.is_share_token_revoked(&token_id).await.unwrap());
        assert!(db.revoke_share_token(&token_id, &author).await.unwrap());
        assert!(db.is_share_token_revoked(&token_id).await.unwrap());

        // Revoked flag visible in the list.
        let list = db.list_share_tokens_for_author(&author).await.unwrap();
        assert!(list[0].revoked);
    }

    #[tokio::test]
    async fn register_is_idempotent_and_revoke_is_sticky() {
        let db = db();
        let author = [1u8; 32];
        let token_id = [2u8; 32];
        let manifest = [3u8; 32];

        let first = db
            .register_share_token(&token_id, &author, &manifest, b"sealed", 100, false)
            .await
            .unwrap();
        db.revoke_share_token(&token_id, &author).await.unwrap();

        // Re-register the identical token: idempotent (no duplicate row), and
        // the revoked flag survives — re-registering does not revive it.
        let second = db
            .register_share_token(&token_id, &author, &manifest, b"sealed", 100, false)
            .await
            .unwrap();
        assert_eq!(first.created_at, second.created_at);
        assert!(second.revoked);
        assert_eq!(
            db.list_share_tokens_for_author(&author)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn revoke_is_scoped_to_author() {
        let db = db();
        let owner = [1u8; 32];
        let attacker = [9u8; 32];
        let token_id = [2u8; 32];
        let manifest = [3u8; 32];
        db.register_share_token(&token_id, &owner, &manifest, b"sealed", 100, true)
            .await
            .unwrap();

        // A different actor cannot revoke it.
        assert!(!db.revoke_share_token(&token_id, &attacker).await.unwrap());
        assert!(!db.is_share_token_revoked(&token_id).await.unwrap());

        // The owner can.
        assert!(db.revoke_share_token(&token_id, &owner).await.unwrap());
        assert!(db.is_share_token_revoked(&token_id).await.unwrap());
    }

    #[tokio::test]
    async fn registration_verdict_is_three_valued() {
        let db = db();
        let author = [1u8; 32];
        let token_id = [2u8; 32];

        // Never registered → None, the answer the succession serve gate keys on.
        assert_eq!(db.share_token_registration(&token_id).await.unwrap(), None);

        db.register_share_token(&token_id, &author, &[3u8; 32], b"sealed", 100, true)
            .await
            .unwrap();
        assert_eq!(
            db.share_token_registration(&token_id).await.unwrap(),
            Some(false)
        );

        db.revoke_share_token(&token_id, &author).await.unwrap();
        assert_eq!(
            db.share_token_registration(&token_id).await.unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn unregistered_token_is_not_revoked() {
        let db = db();
        // Never registered → not revoked (still serves statelessly).
        assert!(!db.is_share_token_revoked(&[7u8; 32]).await.unwrap());
        // Revoking an unregistered token affects no rows.
        assert!(!db.revoke_share_token(&[7u8; 32], &[1u8; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn list_is_per_author() {
        let db = db();
        let a = [1u8; 32];
        let b = [2u8; 32];
        db.register_share_token(&[10u8; 32], &a, &[0u8; 32], b"sealed", 1, true)
            .await
            .unwrap();
        db.register_share_token(&[11u8; 32], &b, &[0u8; 32], b"sealed", 1, true)
            .await
            .unwrap();
        assert_eq!(db.list_share_tokens_for_author(&a).await.unwrap().len(), 1);
        assert_eq!(db.list_share_tokens_for_author(&b).await.unwrap().len(), 1);
    }
}
