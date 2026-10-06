//! Version-retention evaluation + automatic prune scheduling — the version
//! plane's twin of [`super::prune`] / [`super::retention`]'s armed path.
//!
//! `docs/goal/behavior/file-versions.md` § Retention (ratified 2026-08-17)
//! fixes the authoritative resting copy of a version-retention policy as the
//! per-set **`folders.version_retention`** column — the SIBLING of
//! `retention_policy`, never a re-map onto it — and rules the pipeline:
//!
//! 1. [`evaluate_version_retention_at`] selects per `(folder, path)` over the
//!    **listable, non-pipeline** population; the head is structurally never a
//!    candidate and [`VERSION_HARD_FLOOR`] (= 2: the head plus the newest
//!    prior version) is clamped onto the output, so a configured policy can
//!    thin history but can never silently turn the undo affordance off. The
//!    floor binds the *automatic* path only — the owner-driven M2
//!    `fauna.sync.changes.supersede` stays the explicit floorless instrument.
//! 2. [`schedule_version_auto_prune`] **schedules; it never deletes**: one
//!    `VersionBulkPrune` pending action per folder with the ratified **7-day**
//!    cancellable window, its targets marked `prune_pending` (idempotence —
//!    marked rows leave the population). The executor
//!    (`pending_actions::execute_action`, arm `"version.bulk_prune"`)
//!    *soft*-prunes on expiry, opening the 30-day `purge_after` window
//!    `fauna.files.versions.undelete` serves; only the GC-cycle purge step
//!    then stamps `superseded_at`, and the existing pin predicate releases the
//!    chunks one GC grace later.
//!
//! Shared-function discipline as the snapshot plane: both consumers of a
//! delete predicate share one implementation, because two copies drift and the
//! direction that drifts is deletion.

use std::sync::Arc;

use anyhow::Result;

use crate::db::{CacheDb, FolderRow, VersionPruneCandidate};
use fauna_protocol::folders::VersionRetention;

/// The **hard floor** of listable versions per path the automatic prune may
/// never breach (`file-versions.md` § Retention ruling 2): the head plus the
/// newest prior version always survive, so bounding history can never disable
/// the undo affordance. The [`super::retention::SNAPSHOT_HARD_FLOOR`] twin on
/// the version plane.
pub const VERSION_HARD_FLOOR: usize = 2;

/// What the nest could make of a folder's resting `version_retention` column —
/// the [`super::retention::FolderRetention`] triple on the version plane, kept
/// as three distinct arms for the same reason: `NotSet` is the overwhelmingly
/// common healthy state, `Unparseable` is a writer disagreeing with the
/// canonical shape and must be logged. Both non-policy arms mean **keep
/// everything**; the pruner must never guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderVersionRetention {
    /// No column value, or one that carries no binding rule.
    NotSet,
    /// A value the canonical shape could not read. Keep everything, loudly.
    Unparseable,
    /// A user-chosen policy, in the canonical 2-field shape.
    Policy(VersionRetention),
}

/// Read a folder's `version_retention` column.
///
/// The canonical serialization is the 2-field
/// `VersionRetention { max_versions_per_path, max_age_days }` JSON
/// (`fauna_protocol::folders`). A zero bound means *that bound is unset*; both
/// bounds zero ⇒ [`FolderVersionRetention::NotSet`], because a policy that
/// binds nothing is not a policy.
pub fn parse_folder_version_retention(raw: Option<&str>) -> FolderVersionRetention {
    let Some(raw) = raw else {
        return FolderVersionRetention::NotSet;
    };
    if raw.trim().is_empty() {
        return FolderVersionRetention::NotSet;
    }
    match serde_json::from_str::<VersionRetention>(raw) {
        Ok(p) if p.is_unset() => FolderVersionRetention::NotSet,
        Ok(p) => FolderVersionRetention::Policy(p),
        Err(_) => FolderVersionRetention::Unparseable,
    }
}

/// Select the versions of **one path** an automatic prune may retire, under
/// the per-set bounds policy.
///
/// `versions` must be the path's **listable, non-pipeline** rows (the
/// [`CacheDb::list_version_prune_population`] filter), sorted **oldest-first**
/// by `seq`. `now_millis` is compared against `created_at` (epoch millis).
///
/// The bounds **intersect** (a version survives only if it is among the newest
/// `max_versions_per_path` versions AND younger than `max_age_days`), so a row
/// prunes when it is over-count OR too old — the
/// [`super::retention::evaluate_folder_retention_at`] semantics with the
/// version plane's floor:
///
/// 1. The **head** (newest row) is structurally never a candidate — it is
///    excluded before the bounds are even consulted, not merely floored.
/// 2. At least [`VERSION_HARD_FLOOR`] rows always remain; when the bounds
///    would breach it, the **newest** candidates are given back.
pub fn evaluate_version_retention_at(
    policy: &VersionRetention,
    versions: &[VersionPruneCandidate],
    now_millis: i64,
) -> Vec<i64> {
    if versions.len() <= VERSION_HARD_FLOOR {
        return vec![]; // Guarantee 2, the cheap way.
    }

    let count_bound =
        (policy.max_versions_per_path > 0).then_some(policy.max_versions_per_path as usize);
    let age_cutoff =
        (policy.max_age_days > 0).then(|| now_millis - (policy.max_age_days as i64) * 86_400_000);
    if count_bound.is_none() && age_cutoff.is_none() {
        return vec![]; // A policy binding nothing prunes nothing.
    }

    // Oldest-first, so a version's rank from the newest end is len-1-i; the
    // head is rank 0 and never a candidate (guarantee 1).
    let total = versions.len();
    let mut prunable: Vec<i64> = Vec::new();
    for (i, v) in versions.iter().enumerate() {
        let rank_from_newest = total - 1 - i;
        if rank_from_newest == 0 {
            continue; // The head — structurally excluded.
        }
        let over_count = count_bound.is_some_and(|n| rank_from_newest >= n);
        let too_old = age_cutoff.is_some_and(|cutoff| v.created_at < cutoff);
        if over_count || too_old {
            prunable.push(v.seq);
        }
    }

    // Guarantee 2: hand back the newest candidates until the floor is met.
    // `prunable` is oldest-first, so truncation retires the newest candidates
    // from the prune list (they survive).
    let keep_at_least = VERSION_HARD_FLOOR.saturating_sub(total - prunable.len());
    if keep_at_least > 0 {
        prunable.truncate(prunable.len().saturating_sub(keep_at_least));
    }
    prunable
}

/// Schedule an automatic version prune for one folder, per its resting
/// `version_retention` column. Returns the number of versions scheduled
/// (0 when the set has no policy, an unreadable one, or nothing out of
/// bounds). **Never deletes** — one 7-day cancellable `VersionBulkPrune`
/// pending action per folder, targets marked `prune_pending`.
pub async fn schedule_version_auto_prune(db: &Arc<CacheDb>, fs: &FolderRow) -> Result<usize> {
    let policy = match parse_folder_version_retention(fs.version_retention.as_deref()) {
        FolderVersionRetention::Policy(p) => p,
        // The healthy default on every set that has never been given a policy:
        // keep everything, silently.
        FolderVersionRetention::NotSet => return Ok(0),
        // A writer disagreeing with the canonical 2-field shape. Refuse and
        // say so; a guess would prune against a number the user never chose.
        FolderVersionRetention::Unparseable => {
            tracing::warn!(
                folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                "version retention policy is not the canonical \
                 {{max_versions_per_path, max_age_days}} shape — keeping every \
                 version (file-versions.md § Retention)"
            );
            return Ok(0);
        }
    };

    // The evaluation population: listable, non-pipeline rows, ordered
    // (path_hash, seq) — grouped per path below, oldest-first within each.
    let population = db.list_version_prune_population(fs.id).await?;
    let now_millis = crate::db::now_epoch_millis();

    let mut prunable_seqs: Vec<i64> = Vec::new();
    let mut i = 0;
    while i < population.len() {
        let path = population[i].path_hash.clone();
        let mut j = i;
        while j < population.len() && population[j].path_hash == path {
            j += 1;
        }
        prunable_seqs.extend(evaluate_version_retention_at(
            &policy,
            &population[i..j],
            now_millis,
        ));
        i = j;
    }
    if prunable_seqs.is_empty() {
        return Ok(0);
    }

    // Layer 2: the cancellable window. One action for the whole folder — the
    // user's cancel gesture is "keep my versions", not a per-row decision.
    let payload = serde_json::json!({ "version_seqs": prunable_seqs }).to_string();
    let action_id = db
        .create_pending_action(
            &crate::pending_actions::ActionType::VersionBulkPrune,
            &fs.actor_id,
            Some(&fs.id.to_string()),
            Some(&payload),
            None,
        )
        .await?;

    for seq in &prunable_seqs {
        db.mark_version_prune_pending(*seq).await?;
    }

    tracing::info!(
        folder = %fauna_core::log_redact::log_folder_name(&fs.name),
        action_id,
        scheduled = prunable_seqs.len(),
        floor = VERSION_HARD_FLOOR,
        "version auto-prune scheduled: versions enter the cancellable window \
         (file-versions.md § Retention (3))"
    );
    Ok(prunable_seqs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_versions_per_path: u32, max_age_days: u32) -> VersionRetention {
        VersionRetention {
            max_versions_per_path,
            max_age_days,
            extra: Default::default(),
        }
    }

    fn v(seq: i64, created_at: i64) -> VersionPruneCandidate {
        VersionPruneCandidate {
            path_hash: vec![0xAA; 32],
            seq,
            created_at,
        }
    }

    fn versions_oldest_first(n: usize) -> Vec<VersionPruneCandidate> {
        (0..n)
            .map(|i| v(i as i64 + 1, 1_000 + i as i64 * 86_400_000))
            .collect()
    }

    // ── parse: the no-user-data-loss arms ─────────────────────────────────

    /// The load-bearing half: **an absent policy deletes nothing.** Every
    /// folder on every deployment rests in this state until a user sets
    /// bounds, so a regression here is nest-wide silent data loss.
    #[test]
    fn an_absent_or_empty_version_retention_column_is_not_a_policy() {
        assert_eq!(
            parse_folder_version_retention(None),
            FolderVersionRetention::NotSet
        );
        assert_eq!(
            parse_folder_version_retention(Some("")),
            FolderVersionRetention::NotSet
        );
        assert_eq!(
            parse_folder_version_retention(Some("   ")),
            FolderVersionRetention::NotSet
        );
        // Present, canonical, but binding nothing — the shape the editor's
        // "clear" gesture would produce if the handler ever rested it.
        assert_eq!(
            parse_folder_version_retention(Some(r#"{"max_versions_per_path":0,"max_age_days":0}"#)),
            FolderVersionRetention::NotSet
        );
    }

    /// A writer disagreeing with the canonical shape is REFUSED, never
    /// guessed at (§ Retention ruling 1: unparseable ⇒ refuse + log).
    #[test]
    fn an_off_shape_version_retention_column_is_unparseable_not_a_guess() {
        for raw in [
            r#"{"max_versions_per_path":"3"}"#, // right name, wrong type
            "not json at all",
            "[]",
        ] {
            assert_eq!(
                parse_folder_version_retention(Some(raw)),
                FolderVersionRetention::Unparseable,
                "{raw} must not read as a policy"
            );
        }
    }

    /// The canonical 2-field shape round-trips through serde — the same JSON
    /// the handler rests in the column.
    #[test]
    fn the_canonical_shape_parses() {
        let p = policy(5, 30);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(
            parse_folder_version_retention(Some(&json)),
            FolderVersionRetention::Policy(p)
        );
    }

    // ── evaluator: what the automatic pruner is forbidden to do ───────────

    /// The count bound is an intersection-of-limits bound: 20 recent versions
    /// under "keep 5 per path" must leave 5, oldest 15 pruned.
    #[test]
    fn the_count_bound_binds_even_when_every_version_is_recent() {
        let p = policy(5, 30);
        // 20 versions, one per hour, all inside the 30-day age bound.
        let versions: Vec<_> = (0..20)
            .map(|i| v(i as i64 + 1, 1_000_000_000 + i as i64 * 3_600_000))
            .collect();
        let now = 1_000_000_000 + 20 * 3_600_000;
        let prunable = evaluate_version_retention_at(&p, &versions, now);
        assert_eq!(prunable.len(), 15, "20 versions under a bound of 5");
        assert_eq!(
            prunable,
            (1..=15).collect::<Vec<i64>>(),
            "the OLDEST 15 prune; the 5 newest survive"
        );
    }

    /// The age bound binds on its own, in days against epoch-millis stamps.
    #[test]
    fn the_age_bound_binds_on_its_own() {
        let p = policy(0, 10); // count unset
        let day = 86_400_000i64;
        let now = 100 * day;
        // Five versions at 80d, 60d, 40d, 5d, 1d old.
        let versions = vec![
            v(1, now - 80 * day),
            v(2, now - 60 * day),
            v(3, now - 40 * day),
            v(4, now - 5 * day),
            v(5, now - day),
        ];
        assert_eq!(
            evaluate_version_retention_at(&p, &versions, now),
            vec![1, 2, 3],
            "the three out-of-age versions prune; the floor pair survives"
        );
    }

    /// Ruling 2's floor: `VERSION_HARD_FLOOR = 2` — the head plus the newest
    /// prior version always survive, whatever the policy asked for.
    #[test]
    fn the_hard_floor_survives_a_policy_that_would_prune_everything() {
        let p = policy(1, 1);
        // Ten versions, all ancient and far beyond the count bound.
        let versions = versions_oldest_first(10);
        let now = 1_000 + 10_000i64 * 86_400_000;
        let prunable = evaluate_version_retention_at(&p, &versions, now);
        assert_eq!(
            versions.len() - prunable.len(),
            VERSION_HARD_FLOOR,
            "exactly the floor survives"
        );
        assert_eq!(
            prunable,
            (1..=8).collect::<Vec<i64>>(),
            "the survivors are the two NEWEST (head + newest prior)"
        );

        // At or below the floor, nothing is ever a candidate.
        for n in 0..=VERSION_HARD_FLOOR {
            assert!(
                evaluate_version_retention_at(&p, &versions_oldest_first(n), now).is_empty(),
                "{n} versions is at or below the floor"
            );
        }
    }

    /// Ruling 2's structural half, pinned independently of the floor: the head
    /// is excluded before the bounds are consulted, so even a
    /// nothing-but-the-head population under a hostile policy keeps it.
    #[test]
    fn the_head_is_never_a_candidate() {
        let p = policy(1, 1);
        let versions = versions_oldest_first(9);
        let head = versions.last().unwrap().seq;
        let now = 1_000 + 10_000i64 * 86_400_000;
        assert!(
            !evaluate_version_retention_at(&p, &versions, now).contains(&head),
            "the head must survive any automatic policy"
        );
    }

    /// A policy binding nothing prunes nothing — the evaluator's own guard,
    /// independent of the parser's NotSet mapping.
    #[test]
    fn a_binds_nothing_policy_prunes_nothing() {
        let p = policy(0, 0);
        let now = 1_000 + 10_000i64 * 86_400_000;
        assert!(evaluate_version_retention_at(&p, &versions_oldest_first(10), now).is_empty());
    }
}
