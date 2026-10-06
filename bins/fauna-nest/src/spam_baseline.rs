//! The deployment spam baseline's publish run — ONE code path behind the
//! admin's "publish now" (`fauna.bridges.publish_spam_baseline`) and the
//! nest's own standing-publish cadence (`mail-spam.md` § Cold start, Path 2).
//!
//! A run: read the opted-in contributors, the inclusion record and the run
//! state under one lock; drive the one granted holder over their sealed
//! models (register a pending run, poke the holder, await its merged half with
//! a bounded timeout — § Encrypted-mode interaction). Every per-user model
//! rests sealed, so the nest merges nothing itself: the holder's half IS the
//! baseline. Then apply the two floors in order:
//!
//! 1. **The contributor floor** (BASELINE-KANON): fewer than
//!    `BASELINE_MIN_CONTRIBUTORS` merged → WITHHOLD — serve an empty baseline,
//!    withdrawing any prior one.
//! 2. **The delta floor** (*The floor applies to every published DELTA*,
//!    ruled 2026-09-21): once a baseline has ever been served, a publish lands
//!    only when at least `BASELINE_MIN_CONTRIBUTORS` contributors' contributions
//!    changed since it — a join, a departure, or a model written since it was
//!    summed ([`changed_contributions`]). Below that the run is DEFERRED:
//!    nothing is written and the box keeps serving what it had. A deployment's
//!    first publish is bound by the contributor floor alone.
//!
//! A run that passes both lands the sum, the new inclusion record and the
//! reference in one transaction ([`crate::db::CacheDb::land_spam_baseline_publish`]).
//!
//! **Standing publish** is the Tier-2 spam-policy setting
//! `baseline_standing_publish` (default off): on, [`spawn_spam_baseline_cadence`]
//! runs this same publish every [`BASELINE_REPUBLISH_INTERVAL`]; off, nothing
//! runs and turning it off withdraws the baseline (the put handler). The
//! cadence never fires because of a departure.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use fauna_mail::spam::{BASELINE_MIN_CONTRIBUTORS, SpamModel};

use crate::db::spam_baseline::{BaselineLanding, Inclusion};
use crate::routes::AppState;

/// How often a standing publish runs. A Rust constant, not a knob — nobody
/// wants to choose it (`mail-spam.md` § Cold start Path 2 → *Standing
/// publish*; `principles.md` § One configuration surface).
pub const BASELINE_REPUBLISH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the cadence task checks whether a standing publish is due. The
/// due-ness is measured from the last run's recorded time, not from process
/// start, so a nest restarted more often than daily still publishes daily.
const CADENCE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long a run awaits the granted holder's merged half for the
/// client-sealed contributors (`mail-spam.md` § Encrypted-mode interaction,
/// ratified 2026-07-13: "the publish handler awaits the holder's submit against
/// a pending run with a bounded timeout, keeping the admin reply synchronous on
/// all 7 apps"). **Internal latency tuning, not a config surface** — no human
/// ever chooses this; a holder that doesn't answer in time simply means this
/// run merges nothing, with `skipped_contributors` reported honestly.
#[cfg(not(test))]
const SPAM_BASELINE_HOLDER_WAIT: Duration = Duration::from_secs(10);
/// Test builds shrink the holder wait so the no-holder timeout paths run in
/// test time; the in-process simulated holder answers well inside a second.
#[cfg(test)]
const SPAM_BASELINE_HOLDER_WAIT: Duration = Duration::from_secs(1);

/// What one run did. Aggregates only — never an actor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishOutcome {
    /// Contributors this run merged (the holder-named sealed contributors).
    pub contributors: u32,
    /// Training samples behind the baseline this run LANDED (`0` otherwise).
    pub sample_count: u32,
    /// A real baseline landed.
    pub published: bool,
    /// Opted-in contributors with a model row this run could not merge.
    pub skipped_contributors: u32,
    /// The delta floor deferred the run: nothing was written.
    pub deferred: bool,
}

/// How many contributors' contributions differ between the inclusion record
/// (the last served publish) and `summed` (what this run merged, actor →
/// the `spam_models.updated_at` it read), plus the inclusion rows an account
/// deletion purged since. Each contributor counts at most once:
///
/// - a row marked departed counts, whether or not the actor is back (leaving
///   and rejoining before the next publish counts once);
/// - a summed-before actor absent from this run counts (their contribution
///   left the sum);
/// - a summed-before actor whose model was written since counts;
/// - an actor summed now and never before counts (a join).
pub fn changed_contributions(
    inclusions: &HashMap<[u8; 32], Inclusion>,
    summed: &HashMap<[u8; 32], i64>,
    purged_since_publish: u32,
) -> u32 {
    let mut changed = purged_since_publish;
    for (actor, inclusion) in inclusions {
        let changed_here = inclusion.departed
            || summed
                .get(actor)
                .is_none_or(|updated_at| *updated_at != inclusion.model_updated_at);
        if changed_here {
            changed = changed.saturating_add(1);
        }
    }
    let joins = summed
        .keys()
        .filter(|a| !inclusions.contains_key(*a))
        .count();
    changed.saturating_add(u32::try_from(joins).unwrap_or(u32::MAX))
}

/// Run one publish — the admin's click and the cadence alike.
pub async fn run_spam_baseline_publish(state: &Arc<AppState>) -> anyhow::Result<PublishOutcome> {
    let snapshot = state.db.snapshot_spam_baseline_run().await?;

    // Every opted-in model row is a candidate for the holder drain below: the
    // per-user model rests only sealed (`put_spam_model` refuses anything
    // else), so the nest can merge none of them itself. A row that is not
    // sealed cannot have been written by this binary and counts nowhere.
    let mut baseline = SpamModel::new();
    let sealed: Vec<([u8; 32], i64)> = snapshot
        .contributors
        .iter()
        .filter(|c| crate::spam_model_seal::is_sealed_model_blob(&c.model))
        .map(|c| (c.actor, c.updated_at))
        .collect();
    let sealed_candidates = u32::try_from(sealed.len()).unwrap_or(u32::MAX);

    // Drive the granted holder over the sealed candidates. There is no
    // nest→bridge request/response channel (only push events + holder-
    // initiated RPCs), so: register a pending run, poke the holder, and await
    // its `submit_spam_baseline` with a bounded timeout. No submission (holder
    // disconnected / slow / no reaching grants) ⇒ the run merges nothing and
    // the erosion is reported via `skipped_contributors`.
    //
    // The run is bound to the ONE holder the copies are sealed to, and only
    // that holder is poked: the drain's role gate also admits the MDA a
    // standard box always runs, which holds no reaching grant, so a broadcast
    // would have it answer with an empty half it computed without merging
    // anything — beating the real holder to the oneshot and dropping the
    // sealed half every run.
    let mut submission: Option<crate::routes::SpamBaselineSubmission> = None;
    let holder = crate::bridge_imap_handlers::resolve_content_processor_holder(state)
        .await?
        .map(|(holder, _target)| holder);
    if let (true, Some(holder)) = (sealed_candidates > 0, holder) {
        let run_id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.spam_baseline_runs.lock().await.insert(
            run_id.clone(),
            crate::routes::SpamBaselineRun { holder, tx },
        );
        crate::bridge_routing_handlers::notify_bridges_spam_baseline_publish(
            state, &run_id, &holder,
        )
        .await;
        // Timeout or channel drop ⇒ no submission this run.
        if let Ok(Ok(sub)) = tokio::time::timeout(SPAM_BASELINE_HOLDER_WAIT, rx).await {
            submission = Some(sub);
        }
        // The submit handler removes the entry on success; this covers the
        // timeout path (and is an idempotent no-op after a successful submit).
        state.spam_baseline_runs.lock().await.remove(&run_id);
    }

    // Fold the holder's merged half. The holder NAMES the contributors it
    // merged (`merged_contributors`), and those names, intersected with this
    // run's sealed candidates, are both the count toward the floors and the
    // set recorded as summed — so a skipped candidate never holds an inclusion
    // row (`mail-spam.md` § Cold start Path 2 → *A contributor's departure
    // withdraws the baseline*: "a sealed contributor that publish skipped …
    // withdraws nothing"). A name outside the candidates is dropped, so a
    // claim can never fake the floors. Nothing named means nothing counted:
    // a half that names no contributor merges nothing, as does an empty or
    // undecodable one.
    let mut named_sealed: HashSet<[u8; 32]> = HashSet::new();
    if let Some(sub) = submission
        && !sub.merged_model.is_empty()
        && !sub.merged_contributors.is_empty()
        && let Some(half) = SpamModel::from_bytes(&sub.merged_model)
    {
        baseline.merge(&half);
        let candidates: HashSet<[u8; 32]> = sealed.iter().map(|(actor, _)| *actor).collect();
        named_sealed = sub
            .merged_contributors
            .iter()
            .filter(|actor| candidates.contains(*actor))
            .copied()
            .collect();
    }
    let contributors = u32::try_from(named_sealed.len()).unwrap_or(u32::MAX);
    // Opted-in contributors with a model row that could not be merged this
    // run — the silent-erosion fix (§ Encrypted-mode interaction: "no longer
    // silent").
    let skipped_contributors = sealed_candidates - contributors;

    // 1. The contributor floor: never publish a baseline derived from fewer
    // than `BASELINE_MIN_CONTRIBUTORS` contributors — too few and the aggregate
    // approximates an individual's model. Withhold: serve an empty baseline,
    // withdrawing any prior one, and fall back to rspamd-only cold start.
    if contributors < BASELINE_MIN_CONTRIBUTORS {
        state
            .db
            .withhold_spam_baseline(contributors as i64, skipped_contributors)
            .await?;
        return Ok(PublishOutcome {
            contributors,
            sample_count: 0,
            published: false,
            skipped_contributors,
            deferred: false,
        });
    }

    // Who this run summed: exactly the sealed candidates the holder named.
    let summed: Vec<([u8; 32], i64)> = sealed
        .into_iter()
        .filter(|(actor, _)| named_sealed.contains(actor))
        .collect();

    // 2. The delta floor, once a baseline has ever been served.
    if snapshot.state.last_served_at.is_some() {
        let summed_map: HashMap<[u8; 32], i64> = summed.iter().copied().collect();
        let changed = changed_contributions(
            &snapshot.inclusions,
            &summed_map,
            snapshot.state.purged_inclusions_since_publish,
        );
        if changed < BASELINE_MIN_CONTRIBUTORS {
            state
                .db
                .record_deferred_spam_baseline_run(skipped_contributors)
                .await?;
            return Ok(deferred(contributors, skipped_contributors));
        }
    }

    //
    // The merged baseline is the union of every contributor's n-grams and is
    // then sealed + scored on every cold-start fetch + delivered message. Cap
    // it to the same `model_max_bytes` budget a per-user model is bounded by
    // (§ Bounded size + § Cold start Path 2), dropping the least-informative
    // n-grams. The message counters (and thus `sample_count`) are never
    // evicted.
    baseline.cap_to_bytes(fauna_mail::spam::MODEL_MAX_BYTES_DEFAULT);
    let sample_count = baseline.sample_count();
    let model_json = baseline.to_bytes();
    let landed = state
        .db
        .land_spam_baseline_publish(
            &BaselineLanding {
                model_json: &model_json,
                ham_count: baseline.ham_messages as i64,
                spam_count: baseline.spam_messages as i64,
                contributors: contributors as i64,
                skipped_contributors,
                summed: &summed,
            },
            snapshot.state.departures,
        )
        .await?;
    if !landed {
        // A contributor departed while this run merged: landing would serve
        // the counts that departure withdrew. The next run rebuilds without
        // them.
        return Ok(deferred(contributors, skipped_contributors));
    }
    Ok(PublishOutcome {
        contributors,
        sample_count,
        published: true,
        skipped_contributors,
        deferred: false,
    })
}

fn deferred(contributors: u32, skipped_contributors: u32) -> PublishOutcome {
    PublishOutcome {
        contributors,
        sample_count: 0,
        published: false,
        skipped_contributors,
        deferred: true,
    }
}

/// Whether a standing publish is due at `now_ms`: standing is on and no run
/// (click or cadence) has finished within [`BASELINE_REPUBLISH_INTERVAL`].
pub fn standing_publish_due(standing: bool, last_run_at: Option<i64>, now_ms: i64) -> bool {
    let interval_ms = BASELINE_REPUBLISH_INTERVAL.as_millis() as i64;
    standing && last_run_at.is_none_or(|last| now_ms.saturating_sub(last) >= interval_ms)
}

/// One cadence tick: run the publish when standing publish is on and due.
/// `None` when nothing was due. The cadence task calls this with the clock;
/// tests call it directly with the time they mean (convention 14).
pub async fn run_standing_publish_if_due(
    state: &Arc<AppState>,
    now_ms: i64,
) -> anyhow::Result<Option<PublishOutcome>> {
    let standing = state
        .db
        .get_spam_policy()
        .await?
        .effective()
        .baseline_standing_publish;
    let (_, run_state) = state.db.get_spam_baseline_state().await?;
    if !standing_publish_due(standing, run_state.last_run_at, now_ms) {
        return Ok(None);
    }
    run_spam_baseline_publish(state).await.map(Some)
}

/// Spawn the standing-publish cadence on the shared periodic-sweeper
/// primitive. The first tick is **not** skipped: a nest restarted after its
/// last run fell due publishes now rather than an hour later, and with
/// standing publish off the tick reads one row and returns.
pub fn spawn_spam_baseline_cadence(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        CADENCE_CHECK_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                match run_standing_publish_if_due(&state, crate::db::now_epoch_millis()).await {
                    Ok(Some(outcome)) => tracing::info!(
                        published = outcome.published,
                        deferred = outcome.deferred,
                        contributors = outcome.contributors,
                        skipped = outcome.skipped_contributors,
                        "spam baseline: standing publish ran"
                    ),
                    Ok(None) => {}
                    Err(e) => tracing::error!("spam baseline: standing publish failed: {e}"),
                }
            }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];
    const C: [u8; 32] = [3; 32];
    const D: [u8; 32] = [4; 32];

    fn included(rows: &[([u8; 32], i64, bool)]) -> HashMap<[u8; 32], Inclusion> {
        rows.iter()
            .map(|(a, t, departed)| {
                (
                    *a,
                    Inclusion {
                        model_updated_at: *t,
                        departed: *departed,
                    },
                )
            })
            .collect()
    }

    fn summed(rows: &[([u8; 32], i64)]) -> HashMap<[u8; 32], i64> {
        rows.iter().copied().collect()
    }

    #[test]
    fn nothing_changed_counts_zero() {
        let inc = included(&[(A, 1, false), (B, 2, false), (C, 3, false)]);
        assert_eq!(
            changed_contributions(&inc, &summed(&[(A, 1), (B, 2), (C, 3)]), 0),
            0
        );
    }

    #[test]
    fn a_join_a_retrain_and_a_leave_each_count_once() {
        let inc = included(&[(A, 1, false), (B, 2, false), (C, 3, false)]);
        // D joins, B retrained, C left without a mark (e.g. the holder did not
        // answer for a sealed contributor).
        assert_eq!(
            changed_contributions(&inc, &summed(&[(A, 1), (B, 9), (D, 4)]), 0),
            3
        );
    }

    #[test]
    fn leaving_and_rejoining_counts_once() {
        let inc = included(&[(A, 1, false), (B, 2, true), (C, 3, false)]);
        // B left (marked) and came back with the very model that was summed.
        assert_eq!(
            changed_contributions(&inc, &summed(&[(A, 1), (B, 2), (C, 3)]), 0),
            1
        );
    }

    #[test]
    fn purged_inclusions_count_as_departures() {
        let inc = included(&[(A, 1, false), (B, 2, false)]);
        assert_eq!(
            changed_contributions(&inc, &summed(&[(A, 1), (B, 2)]), 2),
            2
        );
    }

    #[test]
    fn no_inclusion_record_counts_every_contributor_as_a_join() {
        assert_eq!(
            changed_contributions(&HashMap::new(), &summed(&[(A, 1), (B, 2), (C, 3)]), 0),
            3
        );
    }

    #[test]
    fn standing_publish_is_due_only_when_on_and_a_day_has_passed() {
        let day = BASELINE_REPUBLISH_INTERVAL.as_millis() as i64;
        assert!(!standing_publish_due(false, None, 0), "off never runs");
        assert!(standing_publish_due(true, None, 0), "on and never run");
        assert!(!standing_publish_due(true, Some(1_000), 1_000 + day - 1));
        assert!(standing_publish_due(true, Some(1_000), 1_000 + day));
    }
}
