//! The controversial-class feature gate's nest state — the per-tier policy
//! store and the usage-counter day buckets of
//! `docs/goal/architecture/dynamic-features.md` § Usage accounting and
//! § Wire & data shape (W2 (account-data-plane.md § Workstreams) slice 2, the enforcement floor).
//!
//! Nothing here decides anything. The decision is one shared pure function,
//! `fauna_core::feature_gate::feature_verdict`, and this module's job is to
//! hand it honest inputs and to apply its answer atomically.
//!
//! **Why check-and-spend is one call.** § Usage accounting requires counting to
//! be *"atomic with the gated operation (spend-on-commit …), so concurrent
//! operations cannot overshoot a quota by racing"*. Splitting it — read the
//! counters, evaluate, then increment — lets two operations both observe
//! `bound - 1` and both proceed. [`CacheDb::try_spend_feature_usage`] therefore
//! sums the windows, evaluates the verdict, and applies the deltas **inside one
//! transaction**, so the last operation to fit is the last one admitted.
//!
//! ⚠ **The spend is deliberately fail-tight, and the direction matters.** The
//! caller spends *before* performing the operation, so an operation that is
//! admitted here and then fails on its own terms leaves its quota spent. That
//! over-counts against the actor, never under-counts — which is the only
//! direction a bound can tolerate, since the alternative (spend after the
//! operation commits) is exactly the race the paragraph above forbids. The same
//! fail-tight reasoning the goal doc applies to the accepted remove→re-add
//! residual (§ The quota grammar's third refinement).
//!
//! ⚠ **Buckets never decrement.** There is no method here that lowers an
//! `amount`, and that absence is load-bearing rather than incidental: it is what
//! makes the counterparty dimension the structural gate § Charter members claims
//! it is — *a counterparty, once counted, stays counted for the window's
//! trailing span regardless of any later removal or revocation; removal must
//! never refund counterparty quota*. Pruning drops whole buckets that have
//! aged out of the largest window; it never adjusts a live one.

use anyhow::{Context, Result};
use fauna_core::day_bucket::window_start_bucket;
use fauna_core::feature_gate::{
    EffectivePolicy, FeatureEntry, FeaturePolicy, FeatureVerdict, GateOp, GatedFeature,
    QuotaDimension, RuleTier, UsageCounters, Window, feature_verdict,
};
use rusqlite::{Connection, OptionalExtension};

use super::{CacheDb, now_epoch_secs};

/// The subject key for the nest-wide tiers (region, admin). A zero-length blob
/// rather than NULL keeps `feature_policies`' primary key total, so an upsert
/// for "the admin's payments policy" can only ever touch one row.
const NEST_WIDE_SUBJECT: &[u8] = b"";

/// Read the guardian tier's authored document for one (ward, feature) pair out
/// of `guardian_policies.features_document`.
///
/// Two `None`s, and neither is the undecodable case: a ward with no guardianship
/// has no row at all, and a document naming a feature this build has never heard
/// of simply does not match the key — an unknown feature is the
/// additive-everywhere case, not a fault. **A document that fails to decode
/// returns `Err`**, which is a third answer entirely: the caller
/// ([`CacheDb::feature_policies_for`]) turns it into a guardian-tier deny rather
/// than an absence (§ Fail posture — the undecodable-document clause), so this
/// function must never widen its `Err` into an `Ok(None)` for tidiness.
fn guardian_feature_policy(
    conn: &Connection,
    actor_id: &[u8; 32],
    feature: GatedFeature,
) -> Result<Option<FeaturePolicy>> {
    let document: Option<Option<Vec<u8>>> = conn
        .query_row(
            "SELECT features_document FROM guardian_policies WHERE supervised_actor_id = ?1",
            rusqlite::params![actor_id.to_vec()],
            |row| row.get(0),
        )
        .optional()
        .context("read guardian feature sub-document")?;
    let Some(Some(bytes)) = document else {
        return Ok(None);
    };
    let documents: fauna_protocol::features::GuardianFeaturePolicies =
        fauna_protocol::decode_strict(&bytes).context("decode guardian feature sub-document")?;
    Ok(fauna_protocol::features::guardian_policy_for(&documents, feature).cloned())
}

/// The largest window any policy can name, in days — the pruning horizon
/// (§ Usage accounting: "pruned past the largest window").
pub const USAGE_RETENTION_DAYS: u32 = 30;

/// The stable column spelling of a quota dimension — [`QuotaDimension::as_str`],
/// which owns that hazard for every side at once.
///
/// A dedicated mapping rather than a `Debug`/`Display` derive: these strings are
/// **at rest**, so a rename in the shared enum must not silently re-key existing
/// buckets and hand every account a fresh quota. That reasoning is why the map
/// now lives on the enum: until 2026-08-23 this column spelling was one of FOUR
/// byte-identical copies, and nothing held them to each other — the hazard this
/// comment names was real and unguarded.
fn dimension_key(dimension: QuotaDimension) -> &'static str {
    dimension.as_str()
}

/// The stable at-rest spelling of a rule tier. Same reasoning as
/// [`dimension_key`], and the same reason the tier is stored as its own column
/// rather than folded into the document: the meet needs to attribute each
/// surviving bound to a tier (§ Composition — "every surviving bound carries the
/// tier that set it").
///
/// The open arm ([`RuleTier::Unknown`], `transport.md` § Rule 3 in full) has
/// no spelling: the nest only ever names tiers it built, so keying a row by
/// one is a bug that fails loudly rather than a row stored under a guess.
fn tier_key(tier: RuleTier) -> Result<i64> {
    Ok(match tier {
        RuleTier::Structural => 1,
        RuleTier::Region => 2,
        RuleTier::Admin => 3,
        RuleTier::Guardian => 4,
        RuleTier::SelfImposed => 5,
        RuleTier::Unknown => anyhow::bail!("refusing to key a feature policy by an unknown tier"),
    })
}

/// The at-rest key of a feature — refusing the open arm
/// ([`GatedFeature::Unknown`]), whose `"unknown"` placeholder must never become
/// a stored row.
fn feature_key(feature: GatedFeature) -> Result<&'static str> {
    if !feature.is_known() {
        anyhow::bail!("refusing to key a feature policy by an unknown feature");
    }
    Ok(feature.as_str())
}

/// Which tiers keep their documents in `feature_policies`, and how each one's
/// subject is keyed.
///
/// The guardian tier is deliberately absent: § Wire & data shape rules that it
/// mints no new kind and rides `fauna.family.policy.update` as an additive
/// sub-document on `guardian_policies`. [`CacheDb::guardian_feature_policy`]
/// reads it from there, and [`CacheDb::feature_policies_for`] folds it in — so
/// this function's return is the *stored-here* tiers, not the applicable ones.
fn stored_tiers(actor_id: &[u8; 32]) -> [(RuleTier, Vec<u8>); 3] {
    [
        (RuleTier::Region, NEST_WIDE_SUBJECT.to_vec()),
        (RuleTier::Admin, NEST_WIDE_SUBJECT.to_vec()),
        (RuleTier::SelfImposed, actor_id.to_vec()),
    ]
}

/// The subject key a tier's document is stored under: nest-wide for region and
/// admin, the actor for the self tier. Derived, never trusted from a caller.
fn subject_for(tier: RuleTier, actor_id: &[u8; 32]) -> Vec<u8> {
    match tier {
        RuleTier::SelfImposed => actor_id.to_vec(),
        _ => NEST_WIDE_SUBJECT.to_vec(),
    }
}

/// One tier's stored document for one feature, exactly as the row reads — the
/// three states `nest/common.md` § Unreadable stored values keeps apart.
///
/// The enforcement fold ([`CacheDb::feature_policies_for`]) turns
/// [`StoredPolicy::Unreadable`] into a deny; the authored-document reads report
/// it as such. Both fold from this one decode, so they cannot disagree about
/// which rows are unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredPolicy {
    /// No row: this tier has no opinion.
    Absent,
    Authored(FeaturePolicy),
    /// A row whose document this nest cannot decode.
    Unreadable,
}

fn read_stored_policy(
    conn: &Connection,
    tier: RuleTier,
    subject: &[u8],
    feature: GatedFeature,
) -> Result<StoredPolicy> {
    let document: Option<Vec<u8>> = conn
        .query_row(
            "SELECT document FROM feature_policies
             WHERE tier = ?1 AND subject_id = ?2 AND feature = ?3",
            rusqlite::params![tier_key(tier)?, subject, feature_key(feature)?],
            |row| row.get(0),
        )
        .optional()
        .context("read feature policy")?;
    let Some(bytes) = document else {
        return Ok(StoredPolicy::Absent);
    };
    Ok(
        match fauna_protocol::decode_strict::<FeaturePolicy>(&bytes) {
            Ok(policy) => StoredPolicy::Authored(policy),
            Err(e) => {
                tracing::error!(
                    tier = ?tier,
                    feature = %feature.as_str(),
                    error = %e,
                    "undecodable feature policy document — denying at this tier until it is re-authored"
                );
                StoredPolicy::Unreadable
            }
        },
    )
}

impl CacheDb {
    /// Write (or replace) one tier's authored policy document for a feature.
    ///
    /// The subject is implied by the tier: region and admin documents are
    /// nest-wide, a self document binds the actor who wrote it. Passing an
    /// actor for a nest-wide tier is not an error the caller can make — it is
    /// ignored, because the key is derived here rather than trusted.
    pub async fn put_feature_policy(
        &self,
        tier: RuleTier,
        actor_id: &[u8; 32],
        feature: GatedFeature,
        policy: &FeaturePolicy,
    ) -> Result<()> {
        let subject = subject_for(tier, actor_id);
        let document = fauna_protocol::encode_canonical(policy).context("encode feature policy")?;
        let tier = tier_key(tier)?;
        let feature = feature_key(feature)?.to_string();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO feature_policies (tier, subject_id, feature, document, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tier, subject_id, feature)
             DO UPDATE SET document = excluded.document, updated_at = excluded.updated_at",
            rusqlite::params![tier, subject, feature, document.to_vec(), now],
        )
        .context("upsert feature policy")?;
        Ok(())
    }

    /// Delete one tier's authored document for a feature (the "no opinion at
    /// this tier" state — which is *not* the same as an authored `allow`).
    pub async fn clear_feature_policy(
        &self,
        tier: RuleTier,
        actor_id: &[u8; 32],
        feature: GatedFeature,
    ) -> Result<bool> {
        let subject = subject_for(tier, actor_id);
        let tier = tier_key(tier)?;
        let feature = feature_key(feature)?.to_string();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM feature_policies WHERE tier = ?1 AND subject_id = ?2 AND feature = ?3",
                rusqlite::params![tier, subject, feature],
            )
            .context("clear feature policy")?;
        Ok(n > 0)
    }

    /// Every authored policy that applies to this (account, feature) pair, in
    /// tier order. Tier 1 is **not** included — `effective_policy` folds the
    /// registry's constants in itself, and passing them again would attribute a
    /// tie to the wrong row.
    ///
    /// **A document that fails to decode denies at its own tier**. *Cannot read the restriction* is not *no
    /// restriction*: every tier here can only tighten, so an authored document
    /// is by construction a restriction someone set, and the only sound reading
    /// of one this nest cannot parse is the most restrictive one. The skip this
    /// replaced was byte-identical to "this tier said nothing", which silently
    /// **lifted** a nest-enforced guardian limit and a region's legal one.
    ///
    /// The deny is never silent and never terminal: it carries its tier through
    /// the meet, so `fauna.features.status` and the typed refusal both name who
    /// bound the caller (boundary 4), and every writer on this table is a
    /// whole-document replace — re-authoring from the app that owns the tier
    /// clears it, with no shell and no migration. Only an **authored** document
    /// can deny; an absent row is still "no opinion at this tier", which is what
    /// keeps a nest nobody has authored a policy on fully open.
    pub async fn feature_policies_for(
        &self,
        actor_id: &[u8; 32],
        feature: GatedFeature,
    ) -> Result<Vec<(RuleTier, FeaturePolicy)>> {
        let subjects = stored_tiers(actor_id);
        let conn = self.conn.lock().await;
        let mut out = Vec::with_capacity(subjects.len());
        for (tier, subject) in subjects {
            match read_stored_policy(&conn, tier, &subject, feature)? {
                StoredPolicy::Absent => {}
                StoredPolicy::Authored(policy) => out.push((tier, policy)),
                StoredPolicy::Unreadable => out.push((tier, FeaturePolicy::DENIED)),
            }
        }
        // The guardian tier, folded in from `guardian_policies` — it has no row
        // in this table by design (see `stored_tiers`). Read under the same lock
        // so a policy write racing this call cannot be observed half-applied
        // across the two tables.
        match guardian_feature_policy(&conn, actor_id, feature) {
            Ok(Some(policy)) => out.push((RuleTier::Guardian, policy)),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(
                    feature = %feature.as_str(),
                    error = %e,
                    "undecodable guardian feature sub-document — denying until the guardian re-authors it"
                );
                out.push((RuleTier::Guardian, FeaturePolicy::DENIED));
            }
        }
        Ok(out)
    }

    /// One tier's stored document for one feature, **not** folded for
    /// enforcement — the authored-document reads' source
    /// (`dynamic-features.md` § Wire & data shape). An undecodable row comes
    /// back [`StoredPolicy::Unreadable`], never as absence and never as the
    /// deny [`CacheDb::feature_policies_for`] enforces it as.
    ///
    /// Only the tiers stored in `feature_policies`; the guardian tier's
    /// document lives on `guardian_policies` and has no authored read here.
    pub async fn stored_feature_policy(
        &self,
        tier: RuleTier,
        actor_id: &[u8; 32],
        feature: GatedFeature,
    ) -> Result<StoredPolicy> {
        let subject = subject_for(tier, actor_id);
        let conn = self.conn.lock().await;
        read_stored_policy(&conn, tier, &subject, feature)
    }

    /// The guardian tier's authored document for one (ward, feature) pair, or
    /// `None` when no guardian has spoken about this feature.
    ///
    /// Exposed for the write path's read-modify-write; the meet reaches it
    /// through [`CacheDb::feature_policies_for`], which is the only place any
    /// caller should be assembling tiers.
    pub async fn guardian_feature_policy(
        &self,
        actor_id: &[u8; 32],
        feature: GatedFeature,
    ) -> Result<Option<FeaturePolicy>> {
        let conn = self.conn.lock().await;
        guardian_feature_policy(&conn, actor_id, feature)
    }

    /// The observed counters for one (account, feature) pair, each dimension
    /// summed over each trailing window ending at `today`.
    pub async fn feature_usage_counters(
        &self,
        actor_id: &[u8; 32],
        feature: GatedFeature,
        today: i64,
    ) -> Result<UsageCounters> {
        let conn = self.conn.lock().await;
        read_counters(&conn, actor_id, feature, today)
    }

    /// **The gate's write half**: evaluate `op` against this account's effective
    /// policy and observed usage and, if it is allowed, spend its deltas — all
    /// inside one transaction, so two concurrent operations cannot both fit into
    /// the last unit of a quota (§ Usage accounting, spend-on-commit).
    ///
    /// Returns the verdict. On anything but [`FeatureVerdict::Allow`] no bucket
    /// moves: a refused operation costs its actor nothing.
    pub async fn try_spend_feature_usage(
        &self,
        actor_id: &[u8; 32],
        entry: &FeatureEntry,
        policy: &EffectivePolicy,
        op: &GateOp,
        today: i64,
    ) -> Result<FeatureVerdict> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin feature-gate spend tx")?;

        let usage = read_counters(&tx, actor_id, entry.feature, today)?;
        let verdict = feature_verdict(entry, policy, &usage, op);
        if !verdict.is_allowed() {
            // Nothing to roll back — the transaction only read — but commit
            // rather than drop so the read is not reported as a rollback.
            tx.commit().context("commit feature-gate refusal")?;
            return Ok(verdict);
        }

        for &dimension in entry.dimensions {
            let delta = op.delta(dimension);
            if delta == 0 {
                continue;
            }
            tx.execute(
                "INSERT INTO feature_usage (actor_id, feature, dimension, day, amount)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (actor_id, feature, dimension, day)
                 DO UPDATE SET amount = MIN(amount + excluded.amount, ?6)",
                rusqlite::params![
                    actor_id.as_slice(),
                    entry.feature.as_str(),
                    dimension_key(dimension),
                    today,
                    delta as i64,
                    i64::MAX,
                ],
            )
            .context("spend feature usage")?;
        }

        tx.commit().context("commit feature-gate spend")?;
        Ok(verdict)
    }

    /// Drop buckets that have aged out of the largest window any policy can
    /// name. Called by the sweeper; a bucket older than the horizon can no
    /// longer contribute to any window sum, so removing it changes no verdict.
    pub async fn prune_feature_usage(&self, today: i64) -> Result<usize> {
        let horizon = window_start_bucket(today, USAGE_RETENTION_DAYS);
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM feature_usage WHERE day < ?1",
                rusqlite::params![horizon],
            )
            .context("prune feature usage")?;
        Ok(n)
    }
}

/// Sum every declared dimension over every window from the day buckets.
///
/// One statement per (dimension, window) rather than a single grouped scan: the
/// windows are nested (`day ⊂ week ⊂ month`), so a grouped query would have to
/// re-bucket in Rust and re-derive the boundaries the shared
/// `window_start_bucket` rule already owns.
fn read_counters(
    conn: &Connection,
    actor_id: &[u8; 32],
    feature: GatedFeature,
    today: i64,
) -> Result<UsageCounters> {
    let mut counters = UsageCounters::default();
    for dimension in [
        QuotaDimension::Operations,
        QuotaDimension::Counterparties,
        QuotaDimension::Volume,
    ] {
        for window in Window::ALL {
            let start = window_start_bucket(today, window.days());
            let sum: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(amount), 0) FROM feature_usage
                     WHERE actor_id = ?1 AND feature = ?2 AND dimension = ?3
                       AND day >= ?4 AND day <= ?5",
                    rusqlite::params![
                        actor_id.as_slice(),
                        feature.as_str(),
                        dimension_key(dimension),
                        start,
                        today,
                    ],
                    |row| row.get(0),
                )
                .context("sum feature usage window")?;
            let sum = sum.max(0) as u64;
            let counts = match dimension {
                QuotaDimension::Operations => &mut counters.operations,
                QuotaDimension::Counterparties => &mut counters.counterparties,
                QuotaDimension::Volume => &mut counters.volume,
            };
            match window {
                Window::Day => counts.day = sum,
                Window::Week => counts.week = sum,
                Window::Month => counts.month = sum,
            }
        }
    }
    Ok(counters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::feature_gate::{Availability, entry, test_support::bound_at};

    async fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    fn actor(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn op(feature: GatedFeature, new_counterparties: u64, magnitude: u64) -> GateOp {
        GateOp {
            feature,
            surface: "test.surface",
            new_counterparties,
            magnitude,
        }
    }

    async fn effective(
        db: &CacheDb,
        actor_id: &[u8; 32],
        feature: GatedFeature,
    ) -> EffectivePolicy {
        let authored = db.feature_policies_for(actor_id, feature).await.unwrap();
        fauna_core::feature_gate::effective_policy(feature, &authored, &[])
    }

    /// Encode a guardian `features` sub-document the way the family policy
    /// handler does, so these tests exercise the real stored shape rather than a
    /// convenient one.
    fn guardian_document(entries: &[(GatedFeature, FeaturePolicy)]) -> Vec<u8> {
        let map: fauna_protocol::features::GuardianFeaturePolicies = entries
            .iter()
            .map(|(f, p)| (f.as_str().to_string(), p.clone()))
            .collect();
        fauna_protocol::encode_canonical(&map).unwrap().to_vec()
    }

    async fn ward_of(db: &CacheDb, guardian: &[u8; 32], ward: &[u8; 32]) {
        db.create_user_with_handle(guardian, "personal", "parent", None)
            .await
            .unwrap();
        db.create_user_with_handle(ward, "personal", "kid", Some(&guardian[..]))
            .await
            .unwrap();
    }

    /// **The guardian tier reaches the meet at all.**
    ///
    /// Its documents live in `guardian_policies`, not `feature_policies`, so
    /// `stored_tiers` cannot see them — before the fold-in, a guardian could set
    /// a feature limit through the shipped `fauna.family.policy.update` and the
    /// gate would never once consult it. That is a *silent* gate, which
    /// boundary 4 forbids as squarely as an unexplained refusal.
    ///
    /// Deleting the fold-in in `feature_policies_for` reds exactly this test and
    /// its sibling below.
    #[tokio::test]
    async fn a_guardian_feature_limit_binds_the_ward_through_the_gate() {
        let db = db().await;
        let guardian = actor(1);
        let ward = actor(2);
        ward_of(&db, &guardian, &ward).await;

        // The guardian bounds payments operations at one per day.
        let policy = bound_at(QuotaDimension::Operations, Window::Day, 1);
        db.update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            Some(&guardian_document(&[(GatedFeature::Payments, policy)])),
        )
        .await
        .unwrap();

        let authored = db
            .feature_policies_for(&ward, GatedFeature::Payments)
            .await
            .unwrap();
        assert!(
            authored.iter().any(|(t, _)| *t == RuleTier::Guardian),
            "the guardian tier must appear among the authored tiers; \
             got {:?}",
            authored.iter().map(|(t, _)| *t).collect::<Vec<_>>()
        );

        // And it binds: the first operation fits, the second does not.
        let entry = entry(GatedFeature::Payments);
        let effective = effective(&db, &ward, GatedFeature::Payments).await;
        let first = db
            .try_spend_feature_usage(
                &ward,
                entry,
                &effective,
                &op(GatedFeature::Payments, 0, 0),
                100,
            )
            .await
            .unwrap();
        assert_eq!(first, FeatureVerdict::Allow);
        let second = db
            .try_spend_feature_usage(
                &ward,
                entry,
                &effective,
                &op(GatedFeature::Payments, 0, 0),
                100,
            )
            .await
            .unwrap();
        assert!(
            matches!(second, FeatureVerdict::OverQuota { .. }),
            "the guardian's per-day bound must refuse the second operation, got {second:?}"
        );
    }

    /// An account with no guardian is unaffected by the fold-in — the read must
    /// not invent a tier, and an unsupervised adult has no `guardian_policies`
    /// row at all.
    #[tokio::test]
    async fn an_unsupervised_account_gains_no_guardian_tier() {
        let db = db().await;
        let adult = actor(9);
        let authored = db
            .feature_policies_for(&adult, GatedFeature::Payments)
            .await
            .unwrap();
        assert!(
            !authored.iter().any(|(t, _)| *t == RuleTier::Guardian),
            "an unsupervised account must have no guardian tier"
        );
    }

    /// A guardian document naming a feature this build does not know is carried,
    /// not rejected — and it binds nothing here. The additive-everywhere case:
    /// a newer nest's document must round-trip through an older one rather than
    /// costing the ward the limits it *can* read.
    #[tokio::test]
    async fn an_unknown_feature_key_in_the_guardian_document_is_ignored_not_fatal() {
        let db = db().await;
        let guardian = actor(1);
        let ward = actor(2);
        ward_of(&db, &guardian, &ward).await;

        let mut map: fauna_protocol::features::GuardianFeaturePolicies = Default::default();
        map.insert(
            "quantum-teleportation".to_string(),
            bound_at(QuotaDimension::Operations, Window::Day, 1),
        );
        map.insert(
            GatedFeature::Payments.as_str().to_string(),
            bound_at(QuotaDimension::Operations, Window::Day, 5),
        );
        let document = fauna_protocol::encode_canonical(&map).unwrap().to_vec();
        db.update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            Some(&document),
        )
        .await
        .unwrap();

        // The known feature still resolves through, unharmed by its neighbour.
        let authored = db
            .feature_policies_for(&ward, GatedFeature::Payments)
            .await
            .unwrap();
        let guardian_bound = authored
            .iter()
            .find(|(t, _)| *t == RuleTier::Guardian)
            .expect("the payments entry must survive an unknown sibling key");
        assert_eq!(
            guardian_bound.1.operations.get(Window::Day),
            Some(5),
            "the guardian's own bound must be the one that survives"
        );
    }

    #[tokio::test]
    async fn a_window_sums_only_the_buckets_inside_it() {
        let db = db().await;
        let a = actor(1);
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        // Spend one operation on each of three days: today, 6 days ago (inside
        // the trailing week) and 20 days ago (outside it, inside the month).
        for day in [100, 94, 80] {
            let v = db
                .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), day)
                .await
                .unwrap();
            assert!(v.is_allowed());
        }

        let counters = db
            .feature_usage_counters(&a, GatedFeature::Payments, 100)
            .await
            .unwrap();
        assert_eq!(counters.operations.day, 1, "today alone");
        assert_eq!(counters.operations.week, 2, "today + day 94");
        assert_eq!(counters.operations.month, 3, "all three");
    }

    #[tokio::test]
    async fn counters_are_per_account_and_per_feature() {
        let db = db().await;
        let (a, b) = (actor(1), actor(2));
        let payments = entry(GatedFeature::Payments);
        let zaps = entry(GatedFeature::Zaps);
        let policy_p = effective(&db, &a, GatedFeature::Payments).await;
        let policy_z = effective(&db, &a, GatedFeature::Zaps).await;

        db.try_spend_feature_usage(
            &a,
            payments,
            &policy_p,
            &op(GatedFeature::Payments, 0, 0),
            10,
        )
        .await
        .unwrap();
        db.try_spend_feature_usage(&a, zaps, &policy_z, &op(GatedFeature::Zaps, 0, 0), 10)
            .await
            .unwrap();

        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 10)
                .await
                .unwrap()
                .operations
                .day,
            1
        );
        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Zaps, 10)
                .await
                .unwrap()
                .operations
                .day,
            1,
            "the zap spend did not land in the payments bucket"
        );
        assert_eq!(
            db.feature_usage_counters(&b, GatedFeature::Payments, 10)
                .await
                .unwrap()
                .operations
                .day,
            0,
            "another account's usage is not this account's"
        );
    }

    #[tokio::test]
    async fn a_refused_operation_spends_nothing() {
        let db = db().await;
        let a = actor(1);
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        let first = db
            .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), 5)
            .await
            .unwrap();
        assert!(first.is_allowed());

        let second = db
            .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), 5)
            .await
            .unwrap();
        assert!(matches!(
            second,
            FeatureVerdict::OverQuota {
                dimension: QuotaDimension::Operations,
                limit: 1,
                observed: 1,
                tier: RuleTier::Admin,
                ..
            }
        ));

        // The refusal must not have charged the actor for the attempt: a client
        // retrying against a bound it cannot meet would otherwise inflate its
        // own counters and lengthen its own lockout.
        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 5)
                .await
                .unwrap()
                .operations
                .day,
            1
        );
    }

    /// The anti-cycling pin: the
    /// bucket is what holds the count, so an add→remove→re-add loop against a
    /// *single* counterparty spends one unit per cycle and trips the bound.
    ///
    /// This is the DB half — the loop drives the delta the caller resolved. The
    /// end-to-end half, where the delta is resolved from the feature's own
    /// mutable records, is `conformance_feature_gate.rs`.
    #[tokio::test]
    async fn a_removal_never_refunds_counterparty_quota() {
        let db = db().await;
        let a = actor(1);
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Counterparties, Window::Week, 3),
        )
        .await
        .unwrap();
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        // Three cycles fit. Each is the same single counterparty, re-introduced
        // after a removal — so the caller resolves `new_counterparties: 1` every
        // time, and nothing anywhere gives the previous unit back.
        for cycle in 0..3 {
            let v = db
                .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 1, 0), 50)
                .await
                .unwrap();
            assert!(
                v.is_allowed(),
                "cycle {cycle} should fit under a bound of 3"
            );
        }

        let fourth = db
            .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 1, 0), 50)
            .await
            .unwrap();
        assert!(
            matches!(
                fourth,
                FeatureVerdict::OverQuota {
                    dimension: QuotaDimension::Counterparties,
                    limit: 3,
                    observed: 3,
                    ..
                }
            ),
            "the fourth cycle must trip the bound, not run forever: {fourth:?}"
        );
    }

    /// The spouses control, green beside the pin above: an operation against a
    /// *standing* counterparty resolves a delta of `0`, so repeating it forever
    /// spends no counterparty quota at all.
    #[tokio::test]
    async fn repeat_operations_against_a_standing_counterparty_spend_nothing() {
        let db = db().await;
        let a = actor(1);
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Counterparties, Window::Week, 3),
        )
        .await
        .unwrap();
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        // The first operation introduces them; every one after that does not.
        db.try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 1, 0), 50)
            .await
            .unwrap();
        for round in 0..50 {
            let v = db
                .try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), 50)
                .await
                .unwrap();
            assert!(v.is_allowed(), "round {round} must not feel a gate");
        }
        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 50)
                .await
                .unwrap()
                .counterparties
                .week,
            1,
            "50 further operations added no counterparty"
        );
    }

    #[tokio::test]
    async fn spend_applies_exactly_what_the_verdict_measured() {
        let db = db().await;
        let a = actor(1);
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        db.try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 4, 7_000), 9)
            .await
            .unwrap();

        let counters = db
            .feature_usage_counters(&a, GatedFeature::Payments, 9)
            .await
            .unwrap();
        assert_eq!(counters.operations.day, 1, "one operation is one operation");
        assert_eq!(counters.counterparties.day, 4);
        assert_eq!(counters.volume.day, 7_000);
    }

    #[tokio::test]
    async fn pruning_drops_only_buckets_past_the_horizon() {
        let db = db().await;
        let a = actor(1);
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;

        // Day 71 is the oldest bucket a 30-day window ending at 100 still sums.
        for day in [100, 71, 70] {
            db.try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), day)
                .await
                .unwrap();
        }
        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 100)
                .await
                .unwrap()
                .operations
                .month,
            2,
            "day 70 is already outside the month window"
        );

        let dropped = db.prune_feature_usage(100).await.unwrap();
        assert_eq!(dropped, 1, "only the aged-out bucket");
        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 100)
                .await
                .unwrap()
                .operations
                .month,
            2,
            "pruning changed no window sum"
        );
    }

    #[tokio::test]
    async fn a_policy_round_trips_and_clears() {
        let db = db().await;
        let a = actor(1);
        let written = bound_at(QuotaDimension::Operations, Window::Month, 42);
        db.put_feature_policy(RuleTier::Admin, &a, GatedFeature::Payments, &written)
            .await
            .unwrap();

        let read = db
            .feature_policies_for(&a, GatedFeature::Payments)
            .await
            .unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, RuleTier::Admin);
        assert_eq!(read[0].1.operations.per_month, Some(42));

        assert!(
            db.clear_feature_policy(RuleTier::Admin, &a, GatedFeature::Payments)
                .await
                .unwrap()
        );
        assert!(
            db.feature_policies_for(&a, GatedFeature::Payments)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Deleting an account takes its own feature state with it — but **not** the
    /// nest-wide documents, which bind whoever remains. A re-registered actor id
    /// must not inherit a stranger's spent quota, and must not lose the admin's
    /// rules either.
    #[tokio::test]
    async fn deleting_an_account_drops_its_own_feature_state_only() {
        let db = db().await;
        let (a, b) = (actor(1), actor(2));
        db.create_user(&a, "free", "gone").await.unwrap();
        db.put_feature_policy(
            RuleTier::SelfImposed,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 9),
        )
        .await
        .unwrap();
        let entry = entry(GatedFeature::Payments);
        let policy = effective(&db, &a, GatedFeature::Payments).await;
        db.try_spend_feature_usage(&a, entry, &policy, &op(GatedFeature::Payments, 0, 0), 4)
            .await
            .unwrap();

        assert!(db.delete_user(&a).await.unwrap());

        assert_eq!(
            db.feature_usage_counters(&a, GatedFeature::Payments, 4)
                .await
                .unwrap()
                .operations
                .day,
            0,
            "the deleted account's buckets are gone"
        );
        let remaining = db
            .feature_policies_for(&b, GatedFeature::Payments)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1, "the nest-wide admin document survives");
        assert_eq!(remaining[0].0, RuleTier::Admin);
        assert!(
            db.feature_policies_for(&a, GatedFeature::Payments)
                .await
                .unwrap()
                .iter()
                .all(|(tier, _)| *tier != RuleTier::SelfImposed),
            "the deleted account's own document is gone"
        );
    }

    /// An admin document is nest-wide and a self document is not: the subject
    /// key is derived from the tier here, so one account's self-limit can never
    /// leak onto another's evaluation.
    #[tokio::test]
    async fn a_self_document_binds_only_its_own_account() {
        let db = db().await;
        let (a, b) = (actor(1), actor(2));
        db.put_feature_policy(
            RuleTier::SelfImposed,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 9),
        )
        .await
        .unwrap();

        let for_a = db
            .feature_policies_for(&a, GatedFeature::Payments)
            .await
            .unwrap();
        assert_eq!(for_a.len(), 2, "admin + self");

        let for_b = db
            .feature_policies_for(&b, GatedFeature::Payments)
            .await
            .unwrap();
        assert_eq!(for_b.len(), 1, "the admin document only");
        assert_eq!(for_b[0].0, RuleTier::Admin);
    }

    // ── § Fail posture: an authored document this nest cannot read ─────────

    /// Bytes that are well-formed canonical dag-cbor and are **not** a policy
    /// document — the shape a decoder tightening or a non-additive wire break
    /// leaves behind, which is more honest than random noise (random noise fails
    /// the canonical check first, several layers before the shape check that
    /// actually models the hazard).
    fn undecodable_document() -> Vec<u8> {
        fauna_protocol::encode_canonical(&"not a policy document".to_string())
            .unwrap()
            .to_vec()
    }

    /// Store raw bytes at one tier's key. Written through SQL on purpose: the
    /// public writer encodes a real `FeaturePolicy` by construction, so it
    /// cannot produce the row this clause is about.
    async fn put_raw_document(
        db: &CacheDb,
        tier: RuleTier,
        actor_id: &[u8; 32],
        feature: GatedFeature,
        bytes: &[u8],
    ) {
        let subject = subject_for(tier, actor_id);
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO feature_policies (tier, subject_id, feature, document, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tier, subject_id, feature)
             DO UPDATE SET document = excluded.document",
            rusqlite::params![
                tier_key(tier).unwrap(),
                subject,
                feature.as_str().to_string(),
                bytes.to_vec(),
                0i64
            ],
        )
        .unwrap();
    }

    /// **The guardian tier's fail direction**.
    ///
    /// A guardian sub-document this nest cannot decode used to be skipped, which
    /// is *byte-identical to "no guardian spoke"* — so a ward whose parent
    /// bounded a feature gained the feature the moment the document became
    /// unreadable, and nothing anywhere said so. The tier is nest-enforced
    /// (`dynamic-features.md:101`), so this is the dangerous direction: it lifts
    /// the restriction the mechanism exists to hold.
    #[tokio::test]
    async fn an_undecodable_guardian_document_denies_instead_of_lifting_the_limit() {
        let db = db().await;
        let guardian = actor(1);
        let ward = actor(2);
        ward_of(&db, &guardian, &ward).await;

        db.update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            Some(&undecodable_document()),
        )
        .await
        .unwrap();

        let authored = db
            .feature_policies_for(&ward, GatedFeature::Payments)
            .await
            .unwrap();
        assert_eq!(
            authored
                .iter()
                .find(|(t, _)| *t == RuleTier::Guardian)
                .map(|(_, p)| p.availability.clone()),
            Some(Availability::Deny),
            "an unreadable guardian document must reach the meet as a deny, not as an absence; \
             got {authored:?}"
        );

        // …and it binds at the gate, attributed to the guardian, so the ward is
        // told which tier refused (boundary 4 — no silent gates).
        let effective = effective(&db, &ward, GatedFeature::Payments).await;
        assert_eq!(effective.denied_by, Some(RuleTier::Guardian));
        let verdict = db
            .try_spend_feature_usage(
                &ward,
                entry(GatedFeature::Payments),
                &effective,
                &op(GatedFeature::Payments, 0, 0),
                100,
            )
            .await
            .unwrap();
        assert_eq!(
            verdict,
            FeatureVerdict::Deny {
                tier: RuleTier::Guardian
            },
            "the operation must be refused, naming the guardian tier"
        );
    }

    /// The same direction at **every stored tier** — region, admin and self all
    /// read through one loop, and one plane with two failure directions is worse
    /// than either direction chosen consistently.
    ///
    /// Each tier's deny is attributed to *itself*, which is what makes it
    /// fixable: the person who authored the unreadable document is the one the
    /// refusal names, and every writer here is a whole-document replace.
    #[tokio::test]
    async fn an_undecodable_stored_document_denies_at_its_own_tier() {
        for tier in [RuleTier::Region, RuleTier::Admin, RuleTier::SelfImposed] {
            let db = db().await;
            let a = actor(3);
            put_raw_document(
                &db,
                tier,
                &a,
                GatedFeature::Payments,
                &undecodable_document(),
            )
            .await;

            let effective = effective(&db, &a, GatedFeature::Payments).await;
            assert_eq!(
                effective.denied_by,
                Some(tier),
                "an unreadable {tier:?} document must deny at its own tier"
            );
        }
    }

    /// The beside-control, and the reason the clause says *authored*: an
    /// **absent** document is still "no opinion at this tier". Without this the
    /// test above would be satisfied by a nest that denies every feature nobody
    /// ever bounded — the works-out-of-the-box inversion.
    #[tokio::test]
    async fn an_absent_document_is_still_no_opinion() {
        let db = db().await;
        let a = actor(4);
        let effective = effective(&db, &a, GatedFeature::Payments).await;
        assert_eq!(
            effective.denied_by, None,
            "no authored document anywhere must leave the feature on at tier 1"
        );
    }

    /// A *decodable* document is unaffected — the deny is minted by the decode
    /// failure, never by the tier having spoken.
    #[tokio::test]
    async fn a_readable_document_at_the_same_tier_still_merely_bounds() {
        let db = db().await;
        let a = actor(5);
        db.put_feature_policy(
            RuleTier::Admin,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 3),
        )
        .await
        .unwrap();
        let effective = effective(&db, &a, GatedFeature::Payments).await;
        assert_eq!(effective.denied_by, None);
        assert_eq!(
            effective
                .bounds(QuotaDimension::Operations)
                .get(Window::Day)
                .map(|b| b.limit),
            Some(3)
        );
    }
}
