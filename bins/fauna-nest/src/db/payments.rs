//! Payment-provider configs + post-payment claim codes (monetization.md
//! § Pillar 3). Providers are per-author rows keyed `(author_id, kind)`
//! (mirroring `subscription_tiers`); claim codes are globally-unique codes
//! carrying an unbound entitlement until redeemed. Claim rows are never
//! deleted — refund voids, redemption stamps — so the payment audit trail
//! survives (`external_ref` links back to the provider dashboard).

use super::{CacheDb, PaymentClaimRow, PaymentProviderRow, now_epoch_secs};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl CacheDb {
    // ── Provider configs ────────────────────────────────────────────

    /// Insert-or-replace the author's config for one provider kind. A
    /// re-`set` rotates the secret and/or re-points the tier mapping.
    pub async fn upsert_payment_provider(
        &self,
        author_id: &[u8; 32],
        kind: &str,
        webhook_secret: &str,
        tier_name: &str,
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO payment_providers (author_id, kind, webhook_secret, tier_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(author_id, kind) DO UPDATE SET
                 webhook_secret = excluded.webhook_secret,
                 tier_name = excluded.tier_name",
            rusqlite::params![author_id.as_slice(), kind, webhook_secret, tier_name, now],
        )
        .context("upsert payment provider")?;
        Ok(())
    }

    pub async fn get_payment_provider(
        &self,
        author_id: &[u8; 32],
        kind: &str,
    ) -> Result<Option<PaymentProviderRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT kind, webhook_secret, tier_name, created_at,
                    last_verified_at, last_rejected_at
             FROM payment_providers WHERE author_id = ?1 AND kind = ?2",
            rusqlite::params![author_id.as_slice(), kind],
            Self::map_provider_row,
        )
        .optional()
        .context("get payment provider")
    }

    pub async fn list_payment_providers(
        &self,
        author_id: &[u8; 32],
    ) -> Result<Vec<PaymentProviderRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT kind, webhook_secret, tier_name, created_at,
                        last_verified_at, last_rejected_at
                 FROM payment_providers WHERE author_id = ?1 ORDER BY kind ASC",
            )
            .context("prepare list payment providers")?;
        let rows = stmt
            .query_map(
                rusqlite::params![author_id.as_slice()],
                Self::map_provider_row,
            )
            .context("list payment providers")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect payment provider rows")
    }

    pub async fn remove_payment_provider(&self, author_id: &[u8; 32], kind: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM payment_providers WHERE author_id = ?1 AND kind = ?2",
                rusqlite::params![author_id.as_slice(), kind],
            )
            .context("remove payment provider")?;
        Ok(changed > 0)
    }

    /// Stamp a successful webhook-signature verification against an existing
    /// `(author_id, kind)` row (monetization.md § Pillar 3 → Provider status).
    /// No-op (`false`) if the row doesn't exist — the caller already 404s
    /// before reaching this, so that should never happen in practice.
    pub async fn stamp_payment_provider_verified(
        &self,
        author_id: &[u8; 32],
        kind: &str,
    ) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE payment_providers SET last_verified_at = ?3
                 WHERE author_id = ?1 AND kind = ?2",
                rusqlite::params![author_id.as_slice(), kind, now],
            )
            .context("stamp payment provider verified")?;
        Ok(changed > 0)
    }

    /// Stamp a failed webhook-signature verification against an existing
    /// `(author_id, kind)` row.
    pub async fn stamp_payment_provider_rejected(
        &self,
        author_id: &[u8; 32],
        kind: &str,
    ) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE payment_providers SET last_rejected_at = ?3
                 WHERE author_id = ?1 AND kind = ?2",
                rusqlite::params![author_id.as_slice(), kind, now],
            )
            .context("stamp payment provider rejected")?;
        Ok(changed > 0)
    }

    fn map_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PaymentProviderRow> {
        Ok(PaymentProviderRow {
            kind: row.get(0)?,
            webhook_secret: row.get(1)?,
            tier_name: row.get(2)?,
            created_at: row.get(3)?,
            last_verified_at: row.get(4)?,
            last_rejected_at: row.get(5)?,
        })
    }

    // ── Claim codes ─────────────────────────────────────────────────

    /// Insert a fresh unbound claim. Fails on a duplicate `code` (the caller
    /// re-mints — same retry contract as invite codes) and on a duplicate
    /// `(author, provider, external_ref)` (the caller should have looked the
    /// existing claim up first — webhook redelivery is idempotent).
    pub async fn insert_payment_claim(
        &self,
        code: &str,
        author_id: &[u8; 32],
        tier_name: &str,
        provider: &str,
        external_ref: &str,
        valid_until: Option<i64>,
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO payment_claim_codes
             (code, author_id, tier_name, provider, external_ref, valid_until, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                code,
                author_id.as_slice(),
                tier_name,
                provider,
                external_ref,
                valid_until,
                now
            ],
        )
        .context("insert payment claim")?;
        Ok(())
    }

    /// The author's own claim codes, newest first — the audit surface for
    /// both manually-minted and webhook-minted claims (`claims.list`).
    pub async fn list_payment_claims(&self, author_id: &[u8; 32]) -> Result<Vec<PaymentClaimRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT code, author_id, tier_name, provider, external_ref, valid_until,
                        created_at, redeemed_by, redeemed_at, voided_at
                 FROM payment_claim_codes WHERE author_id = ?1 ORDER BY created_at DESC",
            )
            .context("prepare list payment claims")?;
        let rows = stmt
            .query_map(rusqlite::params![author_id.as_slice()], Self::map_claim_row)
            .context("list payment claims")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect payment claim rows")
    }

    pub async fn get_payment_claim(&self, code: &str) -> Result<Option<PaymentClaimRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT code, author_id, tier_name, provider, external_ref, valid_until,
                    created_at, redeemed_by, redeemed_at, voided_at
             FROM payment_claim_codes WHERE code = ?1",
            rusqlite::params![code],
            Self::map_claim_row,
        )
        .optional()
        .context("get payment claim")
    }

    /// The idempotency lookup: has this provider event already minted a claim
    /// for this author?
    pub async fn find_payment_claim_by_external_ref(
        &self,
        author_id: &[u8; 32],
        provider: &str,
        external_ref: &str,
    ) -> Result<Option<PaymentClaimRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT code, author_id, tier_name, provider, external_ref, valid_until,
                    created_at, redeemed_by, redeemed_at, voided_at
             FROM payment_claim_codes
             WHERE author_id = ?1 AND provider = ?2 AND external_ref = ?3",
            rusqlite::params![author_id.as_slice(), provider, external_ref],
            Self::map_claim_row,
        )
        .optional()
        .context("find payment claim by external ref")
    }

    /// Stamp a claim redeemed by `redeemer`. Guarded: only an un-redeemed,
    /// un-voided claim redeems (returns `false` otherwise — the caller maps
    /// that to the precise already-redeemed/voided error by re-reading).
    pub async fn redeem_payment_claim(&self, code: &str, redeemer: &[u8; 32]) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE payment_claim_codes SET redeemed_by = ?2, redeemed_at = ?3
                 WHERE code = ?1 AND redeemed_at IS NULL AND voided_at IS NULL",
                rusqlite::params![code, redeemer.as_slice(), now],
            )
            .context("redeem payment claim")?;
        Ok(changed > 0)
    }

    /// Void an un-redeemed claim (refund/dispute before redemption). Returns
    /// `false` when the claim was already redeemed or voided — an
    /// already-redeemed claim's refund is handled at the subscriber row
    /// instead (`set_subscriber_valid_until`).
    pub async fn void_payment_claim(&self, code: &str) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE payment_claim_codes SET voided_at = ?2
                 WHERE code = ?1 AND redeemed_at IS NULL AND voided_at IS NULL",
                rusqlite::params![code, now],
            )
            .context("void payment claim")?;
        Ok(changed > 0)
    }

    fn map_claim_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PaymentClaimRow> {
        Ok(PaymentClaimRow {
            code: row.get(0)?,
            author_id: row.get(1)?,
            tier_name: row.get(2)?,
            provider: row.get(3)?,
            external_ref: row.get(4)?,
            valid_until: row.get(5)?,
            created_at: row.get(6)?,
            redeemed_by: row.get(7)?,
            redeemed_at: row.get(8)?,
            voided_at: row.get(9)?,
        })
    }
}
