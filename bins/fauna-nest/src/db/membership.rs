//! Membership designations (monetization.md § Pillar 4 — paid nest access):
//! the link `(admin actor, subscription tier) → { admin_tier, lapse_tier }`
//! that makes one of an admin's own subscription tiers mean *membership of this
//! nest*.
//!
//! This module is deliberately its own home rather than a corner of
//! [`super::payments`] (Pillar-3 mechanism config), [`super::subscriptions`]
//! (the entitlement object) or [`super::admin`] (the quota policy): the
//! designation is precisely the *link* between those three, and none of them
//! owns it. Steps (3) admission and (4) lapse reconcile of Pillar 4's slice N1
//! grow their queries here.
//!
//! **Never merges the two tier systems.** `tier_name` addresses
//! `subscription_tiers` (what a buyer gets an entitlement to); `admin_tier` /
//! `lapse_tier` address `tiers` (what quota a user runs under). Writing a row
//! here mutates neither table.
//!
//! Row existence + ownership of `tier_name` is enforced by the caller (the
//! `fauna.admin.membership_tiers.set` handler), not by a foreign key — see the
//! rationale on `MIGRATIONS_MEMBERSHIP_TIERS` in [`super::migrations`].

use super::{CacheDb, MembershipTierRow, now_epoch_secs};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl CacheDb {
    /// Designate one of the admin's subscription tiers as a membership tier, or
    /// re-point an existing designation. An **upsert**: re-designating the same
    /// tier is idempotent rather than a conflict, so a replayed reply is
    /// correct and an admin can adjust the linked quota tiers freely.
    ///
    /// `created_at` is preserved across a re-point — it records when the tier
    /// first became a membership tier, which is the audit-useful fact.
    pub async fn upsert_membership_tier(
        &self,
        admin_id: &[u8; 32],
        tier_name: &str,
        admin_tier: &str,
        lapse_tier: &str,
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO membership_tiers (admin_id, tier_name, admin_tier, lapse_tier, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(admin_id, tier_name) DO UPDATE SET
                 admin_tier = excluded.admin_tier,
                 lapse_tier = excluded.lapse_tier",
            rusqlite::params![admin_id.as_slice(), tier_name, admin_tier, lapse_tier, now],
        )
        .context("upsert membership tier")?;
        Ok(())
    }

    /// Every membership designation the admin owns, oldest first.
    pub async fn list_membership_tiers(
        &self,
        admin_id: &[u8; 32],
    ) -> Result<Vec<MembershipTierRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT tier_name, admin_tier, lapse_tier, created_at
                 FROM membership_tiers WHERE admin_id = ?1
                 ORDER BY created_at ASC, tier_name ASC",
            )
            .context("prepare list membership tiers")?;
        let rows = stmt
            .query_map(rusqlite::params![admin_id.as_slice()], |row| {
                Ok(MembershipTierRow {
                    tier_name: row.get(0)?,
                    admin_tier: row.get(1)?,
                    lapse_tier: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .context("query membership tiers")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect membership tier rows")
    }

    /// The single-pair read — is `(admin, tier_name)` a membership designation,
    /// and to what quota tiers? This is the query **admission** wants: an
    /// entitlement (a claim, a webhook event, a zap receipt) already names its
    /// payee and tier, so the pair is known and `list_membership_tiers` (the
    /// caller-scoped enumeration) would be the wrong shape.
    ///
    /// `None` means "not a membership tier" — the entitlement gates content
    /// keys, not nest membership (monetization.md § *One model, many mechanisms,
    /// two targets*: the target axis lives on the tier's designation). A row
    /// whose `tier_name` names a since-deleted `subscription_tiers` row is
    /// **inert**, not a referential guarantee (the designation carries no FK to
    /// `subscription_tiers` — see `MIGRATIONS_MEMBERSHIP_TIERS`); admission never
    /// reaches this read for such a tier because its own tier-existence
    /// pre-check (`grant_paid_entitlement`) fails first.
    pub async fn get_membership_tier(
        &self,
        admin_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<Option<MembershipTierRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT tier_name, admin_tier, lapse_tier, created_at
             FROM membership_tiers WHERE admin_id = ?1 AND tier_name = ?2",
            rusqlite::params![admin_id.as_slice(), tier_name],
            |row| {
                Ok(MembershipTierRow {
                    tier_name: row.get(0)?,
                    admin_tier: row.get(1)?,
                    lapse_tier: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )
        .optional()
        .context("get membership tier")
    }

    /// Freeze the admission-time membership pair onto a `subscribers` row
    /// (monetization.md § Pillar 4 Rail C step 3). Called by **both** admission
    /// arms and by renewal; a non-NULL `admitted_tier` is what marks the row a
    /// membership rather than an ordinary content subscription.
    ///
    /// Stamping at admission — rather than re-reading the live `membership_tiers`
    /// link at sweep time — is what makes lapse policy survive the admin's own
    /// later edit: a re-point or a clear used to strand already-admitted members
    /// above `lapse_tier` forever.
    pub async fn stamp_membership_admission(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
        admitted_tier: &str,
        admitted_lapse_tier: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE subscribers SET admitted_tier = ?4, admitted_lapse_tier = ?5
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3",
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    tier_name,
                    admitted_tier,
                    admitted_lapse_tier
                ],
            )
            .context("stamp membership admission pair")?;
        Ok(changed > 0)
    }

    /// Reconcile lapsed memberships (monetization.md § Pillar 4 Rail C step 3):
    /// a member whose membership window (`subscribers.valid_until`) has passed
    /// degrades to the pair frozen on their row at admission — a reversible quota
    /// downgrade, never suspension, never data loss (over-`lapse_tier`-cap bytes
    /// persist; only *new* over-cap writes are refused, at the quota gates).
    /// Returns the number of users downgraded. Called at webhook ingress (scoped
    /// to `only_buyer`), at boot, and on a hard-coded-cadence sweep (`None`).
    ///
    /// **Lapse policy freezes at admission.** The scan reads
    /// `subscribers.admitted_{tier,lapse_tier}` and never joins the live
    /// `membership_tiers` link, so an admin re-pointing or clearing a designation
    /// cannot strand already-admitted members above `lapse_tier` — the policy a
    /// member was admitted under is the policy they lapse under. A row with no
    /// stamp is not a membership row (an ordinary content subscription) and is
    /// never considered.
    ///
    /// Two guards keep it from ever harming a member:
    /// - **`users.tier = admitted_tier`** — only downgrades a member still sitting
    ///   at the quota tier their admission assigned, so an admin's manual tier
    ///   change (or a higher membership's assignment) is never clobbered.
    /// - **no other active membership** — a member still holding *any* active
    ///   membership (this admin's or another's) is left alone. `tiers` carries no
    ///   rank, so the precise "re-derive the highest still-active membership's
    ///   tier" isn't expressible here; the conservative choice never drops a
    ///   paying member below a tier they still hold (it can leave them slightly
    ///   *above* if a higher membership lapsed while a lower one stays active — a
    ///   generous, self-healing direction that a renewal or the next admin edit
    ///   corrects). Recorded as a bounded simplification in § Implementation status.
    pub async fn reconcile_lapsed_memberships(&self, only_buyer: Option<&[u8; 32]>) -> Result<u64> {
        let now = now_epoch_secs();
        let buyer_owned = only_buyer.map(|b| b.to_vec());
        let conn = self.conn.lock().await;

        // Collect (member, admitted_tier, admitted_lapse_tier) for every expired
        // membership whose holder is still at their admission-time quota tier and
        // holds no other active membership.
        // `admitted_lapse_tier` is a FROZEN historical value, so it deliberately
        // carries no FK to `tiers` (an FK would let a stale stamp block an admin
        // from ever deleting a quota tier). `users.tier` DOES have one, so a
        // stamp naming a since-deleted tier would make the UPDATE fail and, worse,
        // abort the sweep for everyone else. Resolve it here instead: a vanished
        // lapse tier degrades to `free` — the seeded floor and the designation's
        // own default — because "the member still lapses" is the whole point of
        // the finding this closes; silently skipping them would restore the leak.
        let sql = "
            SELECT s.subscriber_id, s.admitted_tier,
                   CASE WHEN EXISTS (SELECT 1 FROM tiers t WHERE t.name = s.admitted_lapse_tier)
                        THEN s.admitted_lapse_tier ELSE 'free' END
            FROM subscribers s
            JOIN users u ON u.actor_id = s.subscriber_id
            WHERE s.valid_until IS NOT NULL AND s.valid_until <= ?1
              AND s.admitted_tier IS NOT NULL
              AND s.admitted_lapse_tier IS NOT NULL
              AND u.tier = s.admitted_tier
              AND (?2 IS NULL OR s.subscriber_id = ?2)
              AND NOT EXISTS (
                SELECT 1 FROM subscribers s2
                WHERE s2.subscriber_id = s.subscriber_id
                  AND s2.admitted_tier IS NOT NULL
                  AND (s2.valid_until IS NULL OR s2.valid_until > ?1)
              )";
        let mut stmt = conn.prepare(sql).context("prepare lapse reconcile")?;
        let rows: Vec<(Vec<u8>, String, String)> = stmt
            .query_map(rusqlite::params![now, buyer_owned.as_deref()], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("query lapsed memberships")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect lapsed memberships")?;
        drop(stmt);

        let mut downgraded = 0u64;
        for (member, admitted_tier, admitted_lapse_tier) in rows {
            // Re-guard on the exact tier inside the same lock: downgrade only if
            // the member is *still* at their admission-time quota tier, so nothing
            // clobbers a manual change (or a higher membership's assignment) that
            // may have landed between the SELECT and this row's UPDATE.
            let changed = conn
                .execute(
                    "UPDATE users SET tier = ?3 WHERE actor_id = ?1 AND tier = ?2",
                    rusqlite::params![member.as_slice(), admitted_tier, admitted_lapse_tier],
                )
                .context("apply lapse downgrade")?;
            downgraded += changed as u64;
        }
        Ok(downgraded)
    }

    /// Drop a designation. Returns `false` if the tier carried none. The
    /// underlying `subscription_tiers` row is untouched — it simply reverts to
    /// an ordinary content tier.
    pub async fn delete_membership_tier(
        &self,
        admin_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM membership_tiers WHERE admin_id = ?1 AND tier_name = ?2",
                rusqlite::params![admin_id.as_slice(), tier_name],
            )
            .context("delete membership tier")?;
        Ok(changed > 0)
    }
}
