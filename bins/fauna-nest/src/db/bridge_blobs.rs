//! Bridge wrapped-blob storage. Nest stores opaque ciphertext only —
//! no parsing or decoding of blob bodies. Size limits are enforced
//! to prevent abuse; per-shape constants are visible at the module
//! root so handlers can map them to RpcErrors.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};

/// Maximum bytes allowed for a wrapped-MLS blob (per (actor, credential)).
pub const MAX_WRAPPED_MLS_BYTES: usize = 64 * 1024;
/// Maximum bytes allowed for an MLS snapshot blob (per actor).
pub const MAX_MLS_SNAPSHOT_BYTES: usize = 1024 * 1024;
/// Maximum bytes allowed for a WebDAV served-set key blob (per actor). One
/// [`fauna_mls::wrapped_blob::WebdavKeysBlob`] carrying the content keys of an
/// actor's served sets; uncapped generation history per set, so size the ceiling
/// like the MLS snapshot.
pub const MAX_WEBDAV_KEYS_BYTES: usize = 1024 * 1024;
/// Exact length of a plaintext MSEK (the mail storage encryption key is a
/// 32-byte symmetric root; `derive_recipient_hpke_keypair` takes `&[u8; 32]`).
pub const PLAINTEXT_MSEK_LEN: usize = 32;
/// Maximum bytes allowed for a wrapped submission-token blob.
pub const MAX_WRAPPED_SUBMISSION_TOKEN_BYTES: usize = 64 * 1024;
/// Maximum bytes allowed for a wrapped TLS-cert blob.
pub const MAX_TLS_CERT_BYTES: usize = 64 * 1024;

impl CacheDb {
    pub async fn put_wrapped_mls_blob(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
        blob: &[u8],
    ) -> Result<()> {
        if blob.len() > MAX_WRAPPED_MLS_BYTES {
            return Err(anyhow!(
                "wrapped_mls blob too large: {} bytes (max {})",
                blob.len(),
                MAX_WRAPPED_MLS_BYTES
            ));
        }
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO bridge_wrapped_mls_blobs
                (actor_id, credential_id, blob, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&actor[..], credential, blob, now],
        )
        .context("put wrapped mls blob")?;
        Ok(())
    }

    pub async fn get_wrapped_mls_blob(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM bridge_wrapped_mls_blobs
              WHERE actor_id = ?1 AND credential_id = ?2",
            rusqlite::params![&actor[..], credential],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get wrapped mls blob")
    }

    pub async fn delete_wrapped_mls_blob(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<bool> {
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM bridge_wrapped_mls_blobs
              WHERE actor_id = ?1 AND credential_id = ?2",
                rusqlite::params![&actor[..], credential],
            )
            .context("delete wrapped mls blob")?;
        Ok(n > 0)
    }

    pub async fn put_mls_snapshot_blob(&self, actor_id: &[u8; 32], blob: &[u8]) -> Result<()> {
        if blob.len() > MAX_MLS_SNAPSHOT_BYTES {
            return Err(anyhow!(
                "mls_snapshot blob too large: {} bytes (max {})",
                blob.len(),
                MAX_MLS_SNAPSHOT_BYTES
            ));
        }
        let actor = *actor_id;
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO bridge_mls_snapshot_blobs
                (actor_id, blob, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![&actor[..], blob, now],
        )
        .context("put mls snapshot blob")?;
        Ok(())
    }

    pub async fn get_mls_snapshot_blob(&self, actor_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM bridge_mls_snapshot_blobs WHERE actor_id = ?1",
            rusqlite::params![&actor[..]],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get mls snapshot blob")
    }

    /// Store the actor's opaque WebDAV served-set key blob (single row per
    /// actor; INSERT OR REPLACE, so a re-provision on a served-set/generation
    /// change overwrites). Nest never decodes the blob.
    pub async fn put_webdav_keys_blob(&self, actor_id: &[u8; 32], blob: &[u8]) -> Result<()> {
        if blob.len() > MAX_WEBDAV_KEYS_BYTES {
            return Err(anyhow!(
                "webdav_keys blob too large: {} bytes (max {})",
                blob.len(),
                MAX_WEBDAV_KEYS_BYTES
            ));
        }
        let actor = *actor_id;
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO bridge_webdav_keys_blobs
                (actor_id, blob, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![&actor[..], blob, now],
        )
        .context("put webdav keys blob")?;
        Ok(())
    }

    pub async fn get_webdav_keys_blob(&self, actor_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM bridge_webdav_keys_blobs WHERE actor_id = ?1",
            rusqlite::params![&actor[..]],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get webdav keys blob")
    }

    pub async fn put_wrapped_submission_token(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
        blob: &[u8],
    ) -> Result<()> {
        if blob.len() > MAX_WRAPPED_SUBMISSION_TOKEN_BYTES {
            return Err(anyhow!(
                "wrapped_submission_token blob too large: {} bytes (max {})",
                blob.len(),
                MAX_WRAPPED_SUBMISSION_TOKEN_BYTES
            ));
        }
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO bridge_wrapped_submission_tokens
                (actor_id, credential_id, blob, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&actor[..], credential, blob, now],
        )
        .context("put wrapped submission token")?;
        Ok(())
    }

    pub async fn get_wrapped_submission_token(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM bridge_wrapped_submission_tokens
              WHERE actor_id = ?1 AND credential_id = ?2",
            rusqlite::params![&actor[..], credential],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get wrapped submission token")
    }

    pub async fn delete_wrapped_submission_token(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<bool> {
        let actor = *actor_id;
        let credential = credential_id.to_string();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM bridge_wrapped_submission_tokens
              WHERE actor_id = ?1 AND credential_id = ?2",
                rusqlite::params![&actor[..], credential],
            )
            .context("delete wrapped submission token")?;
        Ok(n > 0)
    }

    pub async fn put_tls_cert_blob(
        &self,
        bridge_role: &str,
        bridge_id: &str,
        domain: &str,
        blob: &[u8],
    ) -> Result<()> {
        if blob.len() > MAX_TLS_CERT_BYTES {
            return Err(anyhow!(
                "tls_cert blob too large: {} bytes (max {})",
                blob.len(),
                MAX_TLS_CERT_BYTES
            ));
        }
        let bridge_role = bridge_role.to_string();
        let bridge_id = bridge_id.to_string();
        let domain = domain.to_string();
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO bridge_tls_cert_blobs
                (bridge_role, bridge_id, domain, blob, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![bridge_role, bridge_id, domain, blob, now],
        )
        .context("put tls cert blob")?;
        Ok(())
    }

    pub async fn get_tls_cert_blob(
        &self,
        bridge_role: &str,
        bridge_id: &str,
        domain: &str,
    ) -> Result<Option<Vec<u8>>> {
        let bridge_role = bridge_role.to_string();
        let bridge_id = bridge_id.to_string();
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM bridge_tls_cert_blobs
              WHERE bridge_role = ?1 AND bridge_id = ?2 AND domain = ?3",
            rusqlite::params![bridge_role, bridge_id, domain],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get tls cert blob")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_wrapped_mls_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let blob = vec![0xAA; 256];
        db.put_wrapped_mls_blob(&actor, "cred-1", &blob)
            .await
            .unwrap();
        let got = db.get_wrapped_mls_blob(&actor, "cred-1").await.unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
    }

    #[tokio::test]
    async fn missing_wrapped_mls_blob_returns_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let got = db.get_wrapped_mls_blob(&actor, "cred-x").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn delete_wrapped_mls_blob_returns_existed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        db.put_wrapped_mls_blob(&actor, "cred-1", &[0xBB; 32])
            .await
            .unwrap();
        assert!(db.delete_wrapped_mls_blob(&actor, "cred-1").await.unwrap());
        assert!(!db.delete_wrapped_mls_blob(&actor, "cred-1").await.unwrap());
    }

    #[tokio::test]
    async fn wrapped_mls_blob_size_limit_enforced() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let too_big = vec![0u8; MAX_WRAPPED_MLS_BYTES + 1];
        let err = db.put_wrapped_mls_blob(&actor, "cred-1", &too_big).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn round_trip_mls_snapshot_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let blob = vec![0xCC; 4096];
        db.put_mls_snapshot_blob(&actor, &blob).await.unwrap();
        let got = db.get_mls_snapshot_blob(&actor).await.unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
    }

    #[tokio::test]
    async fn put_wrapped_mls_blob_overwrites_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let blob_a = vec![0xAA; 256];
        let blob_b = vec![0xBB; 512];
        db.put_wrapped_mls_blob(&actor, "cred-1", &blob_a)
            .await
            .unwrap();
        db.put_wrapped_mls_blob(&actor, "cred-1", &blob_b)
            .await
            .unwrap();
        let got = db.get_wrapped_mls_blob(&actor, "cred-1").await.unwrap();
        assert_eq!(got.as_deref(), Some(&blob_b[..]));
    }

    #[tokio::test]
    async fn wrapped_mls_blob_at_exact_size_limit_is_accepted() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let blob = vec![0u8; MAX_WRAPPED_MLS_BYTES];
        db.put_wrapped_mls_blob(&actor, "cred-1", &blob)
            .await
            .unwrap();
        let got = db.get_wrapped_mls_blob(&actor, "cred-1").await.unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
    }

    #[tokio::test]
    async fn round_trip_submission_token() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        let blob = vec![0x11; 128];
        db.put_wrapped_submission_token(&actor, "tok-1", &blob)
            .await
            .unwrap();
        let got = db
            .get_wrapped_submission_token(&actor, "tok-1")
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
        assert!(
            db.delete_wrapped_submission_token(&actor, "tok-1")
                .await
                .unwrap()
        );
        assert!(
            db.get_wrapped_submission_token(&actor, "tok-1")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn round_trip_tls_cert_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        let blob = vec![0x33; 2048];
        db.put_tls_cert_blob("mta", "mta-eu-1", "example.com", &blob)
            .await
            .unwrap();
        let got = db
            .get_tls_cert_blob("mta", "mta-eu-1", "example.com")
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
    }
}
