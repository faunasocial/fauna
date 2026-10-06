//! Retention policy evaluation for backup snapshots.
//!
//! Inspired by restic's `forget` command: snapshots are bucketed into time
//! slots (hourly, daily, weekly, monthly, yearly) and only the newest in
//! each bucket is kept. Tagged snapshots are always preserved.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Retention policy configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Keep the N most recent snapshots.
    #[serde(default)]
    pub keep_last: Option<u32>,
    /// Keep the newest snapshot in each of the last N hours.
    #[serde(default)]
    pub keep_hourly: Option<u32>,
    /// Keep the newest snapshot in each of the last N days.
    #[serde(default)]
    pub keep_daily: Option<u32>,
    /// Keep the newest snapshot in each of the last N weeks.
    #[serde(default)]
    pub keep_weekly: Option<u32>,
    /// Keep the newest snapshot in each of the last N months.
    #[serde(default)]
    pub keep_monthly: Option<u32>,
    /// Keep the newest snapshot in each of the last N years.
    #[serde(default)]
    pub keep_yearly: Option<u32>,
    /// Never prune snapshots that have ANY tag.
    #[serde(default)]
    pub keep_tags: Vec<String>,
    /// Keep all snapshots newer than this many seconds.
    #[serde(default)]
    pub keep_within_secs: Option<i64>,
}

/// Minimal snapshot metadata needed for retention evaluation.
///
/// Tags are carried as **hashes**, never plaintext (path-sealing S1,
/// `docs/goal/behavior/file-sync.md` § Sealed names & paths): the pruner is a
/// server-side equality match, and a snapshot's tags never rest in plaintext.
/// The policy's own `keep_tags` hash through the same derivation at compare
/// time, so both sides of the compare are digests. Build one with
/// [`SnapshotMeta::from_tag_hashes`].
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub id: i64,
    pub created_at: i64, // epoch seconds
    /// Per-tag `fauna_core::path_crypto::snapshot_tag_hash` digests. Empty means
    /// an untagged snapshot.
    pub tag_hashes: Vec<[u8; 32]>,
}

impl SnapshotMeta {
    /// Read a snapshot's tag digests from its stored `tag_hashes` companion —
    /// the JSON array of hex digests `create_snapshot_v2` writes. `None` is an
    /// untagged snapshot.
    pub fn from_tag_hashes(id: i64, created_at: i64, tag_hashes_json: Option<&str>) -> Self {
        let tag_hashes = tag_hashes_json
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
            .map(|hexes| {
                hexes
                    .iter()
                    .filter_map(|h| {
                        let raw = hex::decode(h).ok()?;
                        <[u8; 32]>::try_from(raw.as_slice()).ok()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            id,
            created_at,
            tag_hashes,
        }
    }
}

/// Evaluate a retention policy and return the IDs of snapshots that should be pruned.
///
/// Snapshots must be sorted oldest-first by created_at.
pub fn evaluate_retention(policy: &RetentionPolicy, snapshots: &[SnapshotMeta]) -> Vec<i64> {
    let now = fauna_core::data::Timestamp::now_secs();
    evaluate_retention_at(policy, snapshots, now)
}

/// Evaluate retention with an explicit "now" timestamp (for testing).
pub fn evaluate_retention_at(
    policy: &RetentionPolicy,
    snapshots: &[SnapshotMeta],
    now: i64,
) -> Vec<i64> {
    if policy.is_empty() {
        return vec![]; // No policy = keep everything
    }

    let mut keep: HashSet<i64> = HashSet::new();

    // 1. Tagged snapshots: keep any snapshot that has a tag matching keep_tags,
    //    or if keep_tags is empty, keep any snapshot that has ANY tag.
    //    The compare is digest-to-digest: the policy's plaintext tags hash here
    //    through the same derivation the snapshot's `tag_hashes` used, so the
    //    pruner never needs to read a tag (path-sealing S1).
    let keep_tag_hashes: Vec<[u8; 32]> = policy
        .keep_tags
        .iter()
        .map(|t| fauna_core::path_crypto::snapshot_tag_hash(t))
        .collect();
    for snap in snapshots {
        if !snap.tag_hashes.is_empty() {
            if keep_tag_hashes.is_empty() {
                // No specific tags filter — keep all tagged snapshots
                keep.insert(snap.id);
            } else if snap.tag_hashes.iter().any(|t| keep_tag_hashes.contains(t)) {
                keep.insert(snap.id);
            }
        }
    }

    // 2. keep_within: keep everything newer than threshold
    if let Some(within_secs) = policy.keep_within_secs {
        let cutoff = now - within_secs;
        for snap in snapshots {
            if snap.created_at >= cutoff {
                keep.insert(snap.id);
            }
        }
    }

    // 3. keep_last: keep the N most recent
    if let Some(n) = policy.keep_last {
        // snapshots are oldest-first, so take from the end
        for snap in snapshots.iter().rev().take(n as usize) {
            keep.insert(snap.id);
        }
    }

    // 4. Time-bucket policies: keep the newest snapshot in each bucket
    if let Some(n) = policy.keep_hourly {
        keep_by_bucket(snapshots, now, 3600, n, &mut keep);
    }
    if let Some(n) = policy.keep_daily {
        keep_by_bucket(snapshots, now, 86400, n, &mut keep);
    }
    if let Some(n) = policy.keep_weekly {
        keep_by_bucket(snapshots, now, 7 * 86400, n, &mut keep);
    }
    if let Some(n) = policy.keep_monthly {
        keep_by_bucket(snapshots, now, 30 * 86400, n, &mut keep);
    }
    if let Some(n) = policy.keep_yearly {
        keep_by_bucket(snapshots, now, 365 * 86400, n, &mut keep);
    }

    // Return IDs not in keep set
    snapshots
        .iter()
        .filter(|s| !keep.contains(&s.id))
        .map(|s| s.id)
        .collect()
}

/// Bucket snapshots by time interval and keep the newest in each of the last N buckets.
fn keep_by_bucket(
    snapshots: &[SnapshotMeta],
    now: i64,
    interval_secs: i64,
    keep_count: u32,
    keep: &mut HashSet<i64>,
) {
    // Assign each snapshot to a bucket number (0 = current interval, 1 = previous, etc.)
    // and keep the newest snapshot in each of the last `keep_count` buckets.
    let mut buckets: std::collections::HashMap<i64, &SnapshotMeta> =
        std::collections::HashMap::new();

    for snap in snapshots {
        let bucket = (now - snap.created_at) / interval_secs;
        if bucket < 0 || bucket >= keep_count as i64 {
            continue;
        }
        match buckets.get(&bucket) {
            Some(existing) if existing.created_at >= snap.created_at => {}
            _ => {
                buckets.insert(bucket, snap);
            }
        }
    }

    for snap in buckets.values() {
        keep.insert(snap.id);
    }
}

// `retention_from_user_config` lived here until 2026-08-01 and is DELETED, not
// deprecated. It read a `RetentionPolicy` out of the client-sealed `__config`
// blob (the rail retired at closure step (6); see
// `docs/goal/architecture/config-dissolution.md`) for both auto-prune
// consumers, and could not work in production: that blob rested sealed under
// the CLIENT's BackupKey, which the nest never holds, so the read always came
// back empty. Scheduled auto-pruning therefore never ran on any deployment.
// The authoritative resting copy is the per-set `folders.retention_policy` column (RULING 2026-08-01,
// `backup-restore.md` § 8); [`parse_folder_retention`] below reads it.
//
// **Do not reintroduce a client-sealed read here.** The dead end is not a
// missing writer — giving it one would change nothing — and the step after
// that, handing the nest a key that opens client-sealed blobs, inverts the
// sealing model.

// ==================== The per-set resting policy (the armed path) ====================

/// The **hard floor** of active snapshots an automatic prune may never breach
/// (`docs/goal/behavior/backup-restore.md` § 7 Layer 1 + § 8 Algorithm: *"The
/// hard floor (3 active snapshots) is always enforced, even if the policy would
/// prune below it."*). Re-exported from `fauna-protocol`, where it lives so
/// that the nest and every app read one number: the interactive
/// `fauna.filesync.snapshot.delete` path enforces it through
/// [`CacheDb::check_snapshot_delete_allowed`](crate::db::CacheDb::check_snapshot_delete_allowed),
/// the pruner clamps to it, and an app uses it to say a snapshot cannot be
/// deleted yet instead of offering a delete the nest would refuse.
pub use fauna_protocol::filesync::SNAPSHOT_HARD_FLOOR;

/// What the nest could make of a folder's resting `retention_policy` column.
///
/// The three arms are kept distinct rather than collapsed into `Option` because
/// they are *not* the same event: `NotSet` is the overwhelmingly common healthy
/// state, while `Unparseable` is a writer disagreeing with the canonical shape
/// and must be logged (obligation 5 of the § 8 RULING — an off-shape value in
/// this column is logged and keeps everything: `backup-restore.md` § 8 → *The off-shape
/// at-rest residual*). Both non-policy arms mean **keep everything**; the
/// pruner must never guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderRetention {
    /// No column value, or one that carries no binding rule.
    NotSet,
    /// A value the canonical shape could not read. Keep everything, loudly.
    Unparseable,
    /// A user-chosen policy, in the canonical 2-field shape.
    Policy(fauna_folders_machine::state::RetentionPolicy),
}

/// Read a folder's `retention_policy` column.
///
/// The canonical serialization is the 2-field
/// `RetentionPolicy { max_snapshots, max_age_days }` owned by
/// `libs/fauna-folders-machine/src/state.rs` — the shape the shared creation
/// wizard writes (`backup-restore.md` § 8 RULING, consequence (ii)).
///
/// A zero in either field means *that bound is unset*, matching the retired
/// `retention_from_user_config`'s `n > 0` convention: a UI that submits an empty
/// box as `0` must not thereby be asking for "keep zero snapshots". Both bounds
/// zero ⇒ [`FolderRetention::NotSet`], because a policy that binds nothing is
/// not a policy.
pub fn parse_folder_retention(raw: Option<&str>) -> FolderRetention {
    let Some(raw) = raw else {
        return FolderRetention::NotSet;
    };
    if raw.trim().is_empty() {
        return FolderRetention::NotSet;
    }
    match serde_json::from_str::<fauna_folders_machine::state::RetentionPolicy>(raw) {
        Ok(p) if p.max_snapshots == 0 && p.max_age_days == 0 => FolderRetention::NotSet,
        Ok(p) => FolderRetention::Policy(p),
        Err(_) => FolderRetention::Unparseable,
    }
}

/// Select the snapshots an automatic prune may retire, under the per-set
/// **bounds** policy.
///
/// `snapshots` must be the folder's **active** snapshots (neither
/// `soft_deleted` nor `deletion_pending`), sorted **oldest-first** — the same
/// population [`CacheDb::count_active_snapshots`](crate::db::CacheDb::count_active_snapshots)
/// counts, so this function's floor arithmetic and Layer 1's agree.
///
/// ## Why this is not `evaluate_retention`
///
/// [`evaluate_retention`] is a restic-style **union of keep-rules**: a snapshot
/// survives if *any* rule keeps it. The per-set policy is a pair of **bounds** —
/// the wizard's "Max snapshots retention" / "Max age in days for retention"
/// (`ui.yaml` `wizard-retention-snapshots` / `wizard-retention-days`) — and a
/// user who sets both is asking for *neither* to be exceeded. Expressed through
/// the union engine as `keep_last` + `keep_within_secs`, twenty snapshots taken
/// inside one week would all survive "Keep snapshots: 7", and the storage bound
/// the wizard sold would never bind. So the bounds are evaluated directly here;
/// § 8's eight-field borg vocabulary stays the *target* rule set, and grows on
/// the same column additively when it is built.
///
/// ## Guarantees (each pinned by its own test)
///
/// 1. A **tagged** snapshot is never returned. The union engine already keeps
///    every tagged snapshot when `keep_tags` is empty, and the 2-field policy has
///    no tag vocabulary at all — so omitting the protection would make the
///    automatic path strictly more destructive than the equivalent explicit
///    `fauna.filesync.snapshot.prune` request.
/// 2. At least [`SNAPSHOT_HARD_FLOOR`] active snapshots always remain. When the
///    bounds would breach it, the **newest** survivors are given back.
/// 3. The newest snapshot is never returned (it is the restore point of record),
///    which follows from (2) but is pinned independently.
pub fn evaluate_folder_retention_at(
    policy: &fauna_folders_machine::state::RetentionPolicy,
    snapshots: &[SnapshotMeta],
    now: i64,
) -> Vec<i64> {
    if snapshots.len() <= SNAPSHOT_HARD_FLOOR {
        return vec![]; // Guarantee 2, the cheap way.
    }

    let count_bound = (policy.max_snapshots > 0).then_some(policy.max_snapshots as usize);
    let age_cutoff = (policy.max_age_days > 0).then(|| now - (policy.max_age_days as i64) * 86_400);
    if count_bound.is_none() && age_cutoff.is_none() {
        return vec![]; // A policy binding nothing prunes nothing.
    }

    // Oldest-first, so a snapshot's rank from the newest end is len-1-i.
    let total = snapshots.len();
    let mut prunable: Vec<i64> = Vec::new();
    for (i, snap) in snapshots.iter().enumerate() {
        if !snap.tag_hashes.is_empty() {
            continue; // Guarantee 1.
        }
        let rank_from_newest = total - 1 - i;
        let over_count = count_bound.is_some_and(|n| rank_from_newest >= n);
        let too_old = age_cutoff.is_some_and(|cutoff| snap.created_at < cutoff);
        if over_count || too_old {
            prunable.push(snap.id);
        }
    }

    // Guarantee 2: hand back the newest candidates until the floor is met.
    // `prunable` is oldest-first, so truncation retires the oldest.
    let keep_at_least = SNAPSHOT_HARD_FLOOR.saturating_sub(total - prunable.len());
    if keep_at_least > 0 {
        prunable.truncate(prunable.len().saturating_sub(keep_at_least));
    }
    prunable
}

impl RetentionPolicy {
    /// Returns true if no retention rules are set (keep everything).
    pub fn is_empty(&self) -> bool {
        self.keep_last.is_none()
            && self.keep_hourly.is_none()
            && self.keep_daily.is_none()
            && self.keep_weekly.is_none()
            && self.keep_monthly.is_none()
            && self.keep_yearly.is_none()
            && self.keep_within_secs.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: i64, created_at: i64) -> SnapshotMeta {
        SnapshotMeta {
            id,
            created_at,
            tag_hashes: vec![],
        }
    }

    fn snap_tagged(id: i64, created_at: i64, tags: Vec<String>) -> SnapshotMeta {
        SnapshotMeta {
            id,
            created_at,
            tag_hashes: tags
                .iter()
                .map(|t| fauna_core::path_crypto::snapshot_tag_hash(t))
                .collect(),
        }
    }

    /// The companion the pruner reads on every row decodes to the digests
    /// the create wrote, and an absent companion is an untagged snapshot.
    #[test]
    fn tag_hashes_companion_decodes_to_the_create_s_digests() {
        let stored = serde_json::to_string(&vec![
            hex::encode(fauna_core::path_crypto::snapshot_tag_hash("preserve")),
            hex::encode(fauna_core::path_crypto::snapshot_tag_hash("quarterly")),
        ])
        .unwrap();
        let meta = SnapshotMeta::from_tag_hashes(1, 0, Some(&stored));
        assert_eq!(
            meta.tag_hashes,
            vec![
                fauna_core::path_crypto::snapshot_tag_hash("preserve"),
                fauna_core::path_crypto::snapshot_tag_hash("quarterly"),
            ]
        );
        assert!(
            SnapshotMeta::from_tag_hashes(1, 0, None)
                .tag_hashes
                .is_empty()
        );
    }

    /// The policy's `keep_tags` are matched as digests, so a policy written in
    /// plaintext still protects a snapshot whose tags rest only as hashes.
    #[test]
    fn keep_tags_matches_a_hash_only_snapshot() {
        let policy = RetentionPolicy {
            keep_last: Some(1),
            keep_tags: vec!["preserve".into()],
            ..Default::default()
        };
        let protected = SnapshotMeta {
            id: 1,
            created_at: 100,
            tag_hashes: vec![fauna_core::path_crypto::snapshot_tag_hash("preserve")],
        };
        let prunable =
            evaluate_retention_at(&policy, &[protected, snap(2, 200), snap(3, 300)], 300);
        assert!(!prunable.contains(&1), "keep_tags must protect id 1");
        assert!(prunable.contains(&2), "untagged mid snapshot prunes");
    }

    // ── The per-set resting policy (the armed path) ───────────────────────
    //
    // These pin the arming gate of `backup-restore.md` § 8 RULING consequence
    // (iii). Every one of them is a no-user-data-loss assertion: read them as
    // "what the automatic pruner is forbidden to do", not as parser trivia.

    use fauna_folders_machine::state::RetentionPolicy as SetPolicy;

    fn snaps_oldest_first(n: usize) -> Vec<SnapshotMeta> {
        (0..n)
            .map(|i| snap(i as i64 + 1, 1_000 + i as i64 * 86_400))
            .collect()
    }

    /// Obligation 1, the load-bearing half: **an absent policy deletes nothing.**
    /// Every folder on every deployment is in this state until a user sets a
    /// policy, so a regression here is nest-wide silent data loss.
    #[test]
    fn an_absent_or_empty_retention_column_is_not_a_policy() {
        assert_eq!(parse_folder_retention(None), FolderRetention::NotSet);
        assert_eq!(parse_folder_retention(Some("")), FolderRetention::NotSet);
        assert_eq!(parse_folder_retention(Some("   ")), FolderRetention::NotSet);
        // Present, canonical, but binding nothing.
        assert_eq!(
            parse_folder_retention(Some(r#"{"max_snapshots":0,"max_age_days":0}"#)),
            FolderRetention::NotSet
        );
    }

    /// Obligation 5: a writer disagreeing with the canonical shape is REFUSED,
    /// never guessed at. A `keep_*` JSON written into
    /// this column is deliberately left at rest
    /// (`backup-restore.md` § 8 → *The off-shape at-rest residual*); misreading
    /// one as a policy would prune against a number the user never chose.
    ///
    /// The bare `{}` vector is that writer's *all-nil* edit, not a filler case:
    /// its `RetentionConfig` held five `Int?` fields and Swift's `JSONEncoder`
    /// omits nil optionals, so an Apply with every box empty encoded to exactly
    /// `{}`. It must stay `Unparseable` — the arm that logs. Giving
    /// [`fauna_folders_machine::state::RetentionPolicy`] a `#[serde(default)]`
    /// would quietly demote it to `NotSet`: still keep-everything, so no data
    /// loss, but the warning naming the folder would vanish and the residual
    /// would go silent.
    #[test]
    fn an_off_shape_retention_column_is_unparseable_not_a_guess() {
        for raw in [
            r#"{"keep_last":3,"keep_daily":7}"#, // an incompatible shape
            "{}",                                // ...the same writer, all boxes empty
            r#"{"max_snapshots":"7"}"#,          // right names, wrong types
            "not json at all",
            "[]",
        ] {
            assert_eq!(
                parse_folder_retention(Some(raw)),
                FolderRetention::Unparseable,
                "{raw} must not read as a policy"
            );
        }
    }

    /// The canonical 2-field shape the shared wizard (the Photo Library preset) writes round-trips.
    #[test]
    fn the_canonical_wizard_shape_parses() {
        let parsed = parse_folder_retention(Some(
            &serde_json::to_string(&fauna_folders_machine::state::PHOTO_LIBRARY_RETENTION).unwrap(),
        ));
        assert_eq!(
            parsed,
            FolderRetention::Policy(fauna_folders_machine::state::PHOTO_LIBRARY_RETENTION)
        );
    }

    /// The bounds are an **intersection of limits**, not a union of keep-rules:
    /// twenty snapshots inside one week under "Keep snapshots: 7" must leave 7,
    /// not 20. This is the whole reason the armed path does not reuse
    /// `evaluate_retention` — see that function's doc comment.
    #[test]
    fn the_count_bound_binds_even_when_every_snapshot_is_recent() {
        let policy = SetPolicy {
            max_snapshots: 7,
            max_age_days: 30,
        };
        // 20 snapshots, one per hour, all well inside the 30-day age bound.
        let snapshots: Vec<SnapshotMeta> = (0..20)
            .map(|i| snap(i as i64 + 1, 1_000_000 + i as i64 * 3_600))
            .collect();
        let now = 1_000_000 + 20 * 3_600;
        let prunable = evaluate_folder_retention_at(&policy, &snapshots, now);
        assert_eq!(prunable.len(), 13, "20 snapshots under a bound of 7");
        assert_eq!(
            prunable,
            (1..=13).collect::<Vec<i64>>(),
            "the OLDEST 13 prune; the 7 newest survive"
        );

        // The union engine is what this test exists to rule out: the same
        // numbers routed through `evaluate_retention` keep all 20.
        let as_union = RetentionPolicy {
            keep_last: Some(7),
            keep_within_secs: Some(30 * 86_400),
            ..Default::default()
        };
        assert!(
            evaluate_retention_at(&as_union, &snapshots, now).is_empty(),
            "the union engine would prune NOTHING here — that is the bug this shape avoids"
        );
    }

    /// The age bound binds independently of the count bound.
    #[test]
    fn the_age_bound_binds_on_its_own() {
        let policy = SetPolicy {
            max_snapshots: 0, // unset
            max_age_days: 10,
        };
        let day = 86_400;
        let now = 100 * day;
        // Five snapshots at 80d, 60d, 40d, 5d, 1d old.
        let snapshots = vec![
            snap(1, now - 80 * day),
            snap(2, now - 60 * day),
            snap(3, now - 40 * day),
            snap(4, now - 5 * day),
            snap(5, now - day),
        ];
        assert_eq!(
            evaluate_folder_retention_at(&policy, &snapshots, now),
            vec![1, 2],
            "only the two oldest may go — the floor gives back the third"
        );
    }

    /// Guarantee 2 — the § 8 hard floor, which `evaluate_retention` does NOT
    /// enforce (`the_gc_sweep_prunes_to_policy_and_retains_the_kept_snapshot`
    /// prunes 3 → 1 through the union engine). An automatic prune must never
    /// leave a user with fewer than three restore points, whatever they asked
    /// for.
    #[test]
    fn the_hard_floor_survives_a_policy_that_would_prune_below_it() {
        let policy = SetPolicy {
            max_snapshots: 1,
            max_age_days: 1,
        };
        // Ten snapshots, all far older than the age bound and far beyond the
        // count bound: the policy alone would retire every one.
        let snapshots = snaps_oldest_first(10);
        let now = 1_000 + 10_000 * 86_400;
        let prunable = evaluate_folder_retention_at(&policy, &snapshots, now);
        assert_eq!(
            snapshots.len() - prunable.len(),
            SNAPSHOT_HARD_FLOOR,
            "exactly the floor survives"
        );
        assert_eq!(
            prunable,
            (1..=7).collect::<Vec<i64>>(),
            "the survivors are the three NEWEST, not an arbitrary three"
        );

        // At or below the floor, nothing is ever a candidate.
        for n in 0..=SNAPSHOT_HARD_FLOOR {
            assert!(
                evaluate_folder_retention_at(&policy, &snaps_oldest_first(n), now).is_empty(),
                "{n} snapshots is at or below the floor"
            );
        }
    }

    /// Guarantee 3, pinned independently of the floor: the newest snapshot is
    /// the restore point of record and is never an automatic-prune candidate.
    #[test]
    fn the_newest_snapshot_is_never_auto_pruned() {
        let policy = SetPolicy {
            max_snapshots: 1,
            max_age_days: 1,
        };
        let snapshots = snaps_oldest_first(9);
        let newest = snapshots.last().unwrap().id;
        let now = 1_000 + 10_000 * 86_400;
        assert!(
            !evaluate_folder_retention_at(&policy, &snapshots, now).contains(&newest),
            "the newest snapshot must survive any automatic policy"
        );
    }

    /// Guarantee 1: a tagged snapshot is never automatically pruned. The 2-field
    /// policy has no tag vocabulary, and the union engine keeps every tagged
    /// snapshot when `keep_tags` is empty — so dropping this would make the
    /// automatic path more destructive than the explicit request it automates.
    #[test]
    fn a_tagged_snapshot_is_never_auto_pruned() {
        let policy = SetPolicy {
            max_snapshots: 2,
            max_age_days: 1,
        };
        let mut snapshots = snaps_oldest_first(8);
        snapshots[0] = snap_tagged(1, snapshots[0].created_at, vec!["quarterly".into()]);
        let now = 1_000 + 10_000 * 86_400;
        let prunable = evaluate_folder_retention_at(&policy, &snapshots, now);
        assert!(
            !prunable.contains(&1),
            "the tagged snapshot is protected even though it is the oldest and out of bounds"
        );
        assert!(
            prunable.contains(&2),
            "its untagged neighbour is still a candidate — the protection is per-snapshot"
        );
    }

    #[test]
    fn keep_last_n() {
        let policy = RetentionPolicy {
            keep_last: Some(2),
            ..Default::default()
        };
        let snaps = vec![snap(1, 100), snap(2, 200), snap(3, 300)];
        let prunable = evaluate_retention(&policy, &snaps);
        assert_eq!(prunable, vec![1]); // oldest pruned
    }

    #[test]
    fn keep_daily() {
        let day = 86400;
        let now = 1_700_000_000i64 + 2 * day + 100; // slightly after the 3rd snapshot
        let policy = RetentionPolicy {
            keep_daily: Some(2),
            ..Default::default()
        };
        // 3 snapshots across 3 days
        let snaps = vec![
            snap(1, 1_700_000_000),
            snap(2, 1_700_000_000 + day),
            snap(3, 1_700_000_000 + 2 * day),
        ];
        let prunable = evaluate_retention_at(&policy, &snaps, now);
        assert_eq!(prunable, vec![1]); // day 0 pruned, days 1 and 2 kept
    }

    #[test]
    fn tagged_snapshots_never_pruned() {
        let policy = RetentionPolicy {
            keep_last: Some(1),
            ..Default::default()
        };
        let snaps = vec![snap_tagged(1, 100, vec!["important".into()]), snap(2, 200)];
        let prunable = evaluate_retention(&policy, &snaps);
        assert!(prunable.is_empty()); // id=1 kept because tagged, id=2 kept by keep_last
    }

    #[test]
    fn keep_tags_filter() {
        let policy = RetentionPolicy {
            keep_last: Some(1),
            keep_tags: vec!["preserve".into()],
            ..Default::default()
        };
        let snaps = vec![
            snap_tagged(1, 100, vec!["preserve".into()]),
            snap_tagged(2, 200, vec!["random".into()]),
            snap(3, 300),
        ];
        let prunable = evaluate_retention(&policy, &snaps);
        // id=3 kept by keep_last, id=1 kept by keep_tags, id=2 pruned
        assert_eq!(prunable, vec![2]);
    }

    #[test]
    fn keep_within_duration() {
        let now = 1_700_100_000i64;
        let policy = RetentionPolicy {
            keep_within_secs: Some(3600), // keep last hour
            ..Default::default()
        };
        let snaps = vec![
            snap(1, now - 7200), // 2 hours ago
            snap(2, now - 1800), // 30 min ago
            snap(3, now - 60),   // 1 min ago
        ];
        let prunable = evaluate_retention_at(&policy, &snaps, now);
        assert_eq!(prunable, vec![1]);
    }

    #[test]
    fn empty_policy_keeps_all() {
        let policy = RetentionPolicy::default();
        let snaps = vec![snap(1, 100), snap(2, 200)];
        let prunable = evaluate_retention(&policy, &snaps);
        assert!(prunable.is_empty());
    }
}
