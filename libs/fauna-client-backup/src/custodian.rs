//! The **client-custodian custody policy** — what a device holding the owner's
//! backup corpus keeps, what it reclaims, and when it must stop pulling.
//!
//! This is the pure half of the third destination kind (`docs/goal/behavior/backup-destinations.md`
//! § Third destination kind — client device as custodian; pull-plane mechanics in
//! `docs/goal/architecture/message-segment-store.md` § Client-device custodian
//! (pull)). It sits beside [`crate::audit`] for the same reason that module does:
//! the *policy* is shared Rust every app rides, while the store it runs over and
//! the scheduler that wakes it are per-platform glue — the split the audit loop's
//! `state_path` precedent set.
//!
//! # The one bug this module exists to make unrepresentable
//!
//! **Silent eviction of a live path.** A capacity-capped custodian under pressure
//! has an obvious wrong answer available — drop something to make room — and it
//! is wrong because the dropped bytes may be the only copy the owner still has.
//! The ratified rule is the opposite: *"At cap, retained generations reclaim
//! first; then the custodian **stops pulling and reports cap-reached** rather
//! than silently evicting live paths"*. So [`plan_reclaim`] can only ever name
//! **retained** generations, and a cap it cannot satisfy becomes
//! [`CapState::Reached`] — a state the user sees — never a deletion.
//!
//! # Liveness is derived, never declared
//!
//! Callers pass the flat set of generations they hold; **liveness is computed
//! here** from `(path, stored_at)` — the newest generation at a path is live iff
//! it is not a delete tombstone, and every older one is retained. Nothing in
//! [`HeldGeneration`] says "I am live", so a store whose bookkeeping drifted
//! cannot talk this policy into reclaiming the wrong row: there is no field to
//! get wrong. The same derivation supplies each retained generation's *supersede
//! time* — the `stored_at` of the next-newer generation at its path — which is
//! what the grace window is measured from. Two facts that must agree therefore
//! cannot disagree, because there is only one of them.
//!
//! # Partial eviction by the OS is not a failure
//!
//! iOS/Android app storage carries no durability guarantee under pressure, and
//! the design tolerates that by construction: the store is content-addressed, so
//! a generation the OS reclaimed simply reappears in the next pull's diff. This
//! module therefore never treats "held less than last pass" as an error — it
//! plans over whatever it is handed.

use fauna_protocol::backup::{AUDIT_STATE_FAILED, AUDIT_STATE_OK, CAP_STATE_OK, CAP_STATE_REACHED};

/// The custodian's **local grace window** — the `T` analogue.
///
/// A superseded generation is retained for this long before it may be reclaimed,
/// so a source that overwrote a path with junk has not thereby destroyed the good
/// generation: the owner's audit loop has the whole window to notice, exactly as
/// it does against a nest destination's `T`.
///
/// `message-segment-store.md` § Client-device custodian (pull) delegates the value
/// to implementation time — *"a bucket-1 constant that may differ from the nest's
/// T"*. It is set here **equal to** the nest's `T = 30 d` and kept as a separate
/// named constant so it can diverge later without touching a call site. Equal,
/// because the window's job is identical on both custodians (bound loss-under-
/// attack to the audit cadence, which is ≪ 30 d by construction), and shortening
/// it on the client would weaken exactly that property to buy space that the
/// user's capacity cap already governs — with [`CapState::Reached`] as the honest
/// way to run out, rather than a quieter window.
pub const CUSTODIAN_GRACE_SECS: i64 = 30 * 24 * 60 * 60;

/// How long a custodian may go without checking in before it is **overdue**.
///
/// The client-custodian sibling of [`crate::audit::AUDIT_OVERDUE_SECS`], and
/// deliberately much longer: *"A device asleep for a week is normal; a device gone
/// for a month is a failure"* (`behavior/backup-destinations.md` § Intermittency semantics). Reusing
/// the 7-day audit threshold would alarm on every ordinary holiday.
///
/// Never user-set — the capacity cap is this kind's only knob.
pub const CUSTODIAN_OVERDUE_SECS: i64 = 30 * 24 * 60 * 60;

/// One generation of one path in the custodian's local sealed store.
///
/// A "generation" is one sealed manifest for one path, the same unit a nest
/// destination's custody row covers — so the two custodians' retention stories
/// stay comparable (`behavior/backup-destinations.md` § Custodian contract, question 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldGeneration {
    /// Plaintext path within the custody set, e.g. `"{scope_hex}/seg-00000007.dat"`
    /// or the `manifest.<kind>` mirror. Liveness is per **path**.
    pub path: String,
    /// Hex-encoded 32-byte manifest hash — the generation's identity, and what a
    /// reclaim is executed against.
    pub manifest_hash: String,
    /// Bytes this generation occupies locally. A tombstone occupies none.
    pub size_bytes: u64,
    /// Unix seconds at which this device sealed and stored this generation.
    /// Ordering within a path is by this field.
    pub stored_at: i64,
    /// This generation records that the source reported the path **deleted**. It
    /// holds no bytes; its role is to end the path's liveness while still letting
    /// the generations beneath it age out through the normal grace window.
    pub deleted: bool,
}

/// Whether the custodian may keep pulling.
///
/// Mirrors the wire's [`CAP_STATE_OK`] / [`CAP_STATE_REACHED`] exactly — the
/// check-in carries this verbatim (`fauna.backup.custodian.checkin`), and the
/// nest renders a cap-reached custodian distinctly from an ordinary lagging one,
/// because the two look identical in `backlog_count` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapState {
    /// Room remains: pulling continues.
    Ok,
    /// The live corpus alone meets or exceeds the cap. Every retained generation
    /// has already been reclaimed and there is nothing left to give up without
    /// destroying the owner's only copy of something — so the custodian **stops
    /// pulling** and says so.
    Reached,
}

impl CapState {
    /// The wire string this state travels as.
    pub fn as_wire(self) -> &'static str {
        match self {
            CapState::Ok => CAP_STATE_OK,
            CapState::Reached => CAP_STATE_REACHED,
        }
    }
}

/// Whether the custodian can still produce what its own index says it holds.
///
/// The kind's answer to `docs/goal/behavior/backup-destinations.md` § Custodian contract question
/// 4 (*Audit answerability*). The owner cannot inclusion-sample a sleeping
/// device, so the custodian audits **itself** and its check-in carries the
/// verdict — that sentence is what this type makes true as built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditState {
    /// Every sampled live path was present, openable and content-address
    /// verified from local bytes.
    Ok,
    /// At least one path this store's own index calls live could not be
    /// produced. The store has rotted, and nothing else would say so: it keeps
    /// pulling, keeps checking in, and its `cap_state` stays [`CapState::Ok`].
    Failed,
}

impl AuditState {
    /// The wire string this state travels as.
    pub fn as_wire(self) -> &'static str {
        match self {
            AuditState::Ok => AUDIT_STATE_OK,
            AuditState::Failed => AUDIT_STATE_FAILED,
        }
    }
}

/// One self-audit verdict, as the check-in reports it.
///
/// The two fields are constructed together and never set independently, for the
/// same reason [`ReclaimPlan::check_in`] takes a plan rather than loose numbers:
/// the pair disagreeing is exactly the failure that matters. A `Failed` verdict
/// carrying a *fresh* timestamp would render as "verified moments ago" on the
/// owner's device — the rotting store looking healthiest right when it is worst.
///
/// So there is no field-wise constructor. [`SelfAudit::passed`] is the **only**
/// way a timestamp advances, and it can only produce `Ok`; [`SelfAudit::failed`]
/// takes the *previous* pass's timestamp and cannot be handed `now` by accident,
/// because `now` is not one of its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfAudit {
    state: AuditState,
    last_passed_at: Option<i64>,
}

impl SelfAudit {
    /// A passing audit at `now`. The only constructor that advances the clock.
    pub fn passed(now: i64) -> Self {
        Self {
            state: AuditState::Ok,
            last_passed_at: Some(now),
        }
    }

    /// A failing audit, carrying forward whatever the last **pass** was —
    /// `None` when none ever passed.
    ///
    /// Takes the previous pass rather than `now` on purpose: the signature is
    /// what makes "a failure never advances the clock" unrepresentable rather
    /// than merely documented.
    pub fn failed(previous_pass: Option<i64>) -> Self {
        Self {
            state: AuditState::Failed,
            last_passed_at: previous_pass,
        }
    }

    /// The verdict.
    pub fn state(&self) -> AuditState {
        self.state
    }

    /// Unix seconds of the last passing audit, if any.
    pub fn last_passed_at(&self) -> Option<i64> {
        self.last_passed_at
    }
}

/// What one reclaim pass should do, and what the check-in should then report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimPlan {
    /// Indices into the slice handed to [`plan_reclaim`], in the order they
    /// should be reclaimed: expired-by-grace first, then — only under cap
    /// pressure — the remaining retained generations oldest-supersede first.
    ///
    /// **Only ever retained generations.** A live path's index cannot appear
    /// here; that is the module's whole point.
    pub reclaim: Vec<usize>,
    /// Bytes still held once `reclaim` has been executed — the check-in's
    /// `held_bytes`.
    pub held_bytes: u64,
    /// The check-in's `cap_state`.
    pub cap_state: CapState,
}

impl ReclaimPlan {
    /// Whether a further `incoming_bytes` may be pulled after this plan runs.
    ///
    /// A cap-reached custodian admits nothing. An uncapped one admits everything.
    pub fn admits(&self, incoming_bytes: u64, cap_bytes: Option<u64>) -> bool {
        match cap_bytes {
            _ if self.cap_state == CapState::Reached => false,
            None => true,
            Some(cap) => self.held_bytes.saturating_add(incoming_bytes) <= cap,
        }
    }

    /// Report this pass to the source nest — `fauna.backup.custodian.checkin`.
    ///
    /// The check-in exists so the owner's *other* devices can see this custodian
    /// at all: the nest derives the destination's `backlog_count` from
    /// `high_water` and renders `held_bytes` / `cap_state` on the Backups page.
    ///
    /// It takes the plan rather than three loose numbers **on purpose**. The
    /// underlying verb ([`BackupClient::custodian_checkin`]) accepts a
    /// `held_bytes` and a `cap_state` that no type forces to agree, and the pair
    /// disagreeing is precisely the failure that matters: a custodian that has
    /// stopped pulling at its cap but reports `CAP_STATE_OK` renders as ordinary
    /// lag, and the user is never told their backup stopped. Reporting straight
    /// off the plan means both halves come from the one [`plan_reclaim`] call
    /// that decided them, in shared Rust, for all 7 apps.
    ///
    /// `high_water` is the pull loop's own — how far it has pulled *and sealed*,
    /// never how far it has merely listed.
    /// `audit` is this pass's self-audit verdict, or `None` when no audit ran
    /// this pass and none has ever run. It is a [`SelfAudit`] rather than a
    /// state plus a timestamp for the same reason this method takes the plan:
    /// the two halves must come from one decision, and a `Failed` state beside
    /// a fresh timestamp is the disagreement that would render a rotted store
    /// as just-verified.
    ///
    /// ⚠ A failing audit is reported, never withheld. Skipping the check-in on
    /// failure would silence the row, and silence reads as the 30-day
    /// intermittency case — the wrong alarm, thirty days late.
    pub async fn check_in<R: crate::RpcRequester>(
        &self,
        backup: &crate::BackupClient<R>,
        destination_id: &str,
        high_water: u64,
        audit: Option<SelfAudit>,
        device_id: Option<&str>,
    ) -> Result<fauna_protocol::backup::CustodianCheckinReply, R::Error> {
        backup
            .custodian_checkin(
                destination_id.to_string(),
                high_water,
                self.held_bytes,
                self.cap_state.as_wire().to_string(),
                audit.map(|a| a.state().as_wire().to_string()),
                audit
                    .and_then(|a| a.last_passed_at())
                    .map(|t| t.max(0) as u64),
                device_id.map(str::to_string),
            )
            .await
    }
}

/// Plan what to reclaim from `held`, and what the resulting cap state is.
///
/// `cap_bytes` is the user-set capacity cap (`BackupDestination::capacity_cap_bytes`);
/// `None` means no cap was recorded, which never reports [`CapState::Reached`] —
/// but still reclaims generations that outlived the grace window, since that is
/// ordinary garbage collection rather than cap pressure.
///
/// The three ratified rules, in the order they apply:
///
/// 1. **Latest non-deleted generation per path stays live**, always.
/// 2. **Retained generations past [`CUSTODIAN_GRACE_SECS`] reclaim**, cap or no cap.
/// 3. **Under cap pressure the remaining retained generations reclaim too**,
///    oldest-supersede first — and if the live corpus alone still meets the cap,
///    the answer is [`CapState::Reached`], never an eviction.
pub fn plan_reclaim(held: &[HeldGeneration], cap_bytes: Option<u64>, now: i64) -> ReclaimPlan {
    // Order each path's generations newest-first so the live one and every
    // retained one's supersede time fall out of the same single pass.
    let mut by_path: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (idx, entry) in held.iter().enumerate() {
        by_path.entry(entry.path.as_str()).or_default().push(idx);
    }

    let mut live_bytes: u64 = 0;
    // (index, supersede_at) for every generation that is not live.
    let mut retained: Vec<(usize, i64)> = Vec::new();

    for indices in by_path.values_mut() {
        indices.sort_by_key(|&i| std::cmp::Reverse((held[i].stored_at, i)));
        let mut iter = indices.iter().copied();
        let Some(newest) = iter.next() else {
            continue;
        };
        if held[newest].deleted {
            // The path is deleted: nothing here is live. The tombstone itself
            // supersedes what it covers, and ages out on the same clock.
            retained.push((newest, held[newest].stored_at));
        } else {
            live_bytes = live_bytes.saturating_add(held[newest].size_bytes);
        }
        // Every older generation was superseded by the one stored after it.
        let mut superseded_by = held[newest].stored_at;
        for idx in iter {
            retained.push((idx, superseded_by));
            superseded_by = held[idx].stored_at;
        }
    }

    // Rule 2 — expired retained generations go regardless of cap pressure.
    // Oldest supersede first, so a reclaim that is interrupted has still done
    // the most-owed work.
    retained.sort_by_key(|&(idx, superseded_at)| (superseded_at, idx));
    let mut reclaim: Vec<usize> = Vec::new();
    let mut surviving: Vec<(usize, u64)> = Vec::new();
    for &(idx, superseded_at) in &retained {
        if now.saturating_sub(superseded_at) >= CUSTODIAN_GRACE_SECS {
            reclaim.push(idx);
        } else {
            surviving.push((idx, held[idx].size_bytes));
        }
    }

    let mut held_bytes =
        live_bytes.saturating_add(surviving.iter().map(|&(_, size)| size).sum::<u64>());

    let Some(cap) = cap_bytes else {
        return ReclaimPlan {
            reclaim,
            held_bytes,
            cap_state: CapState::Ok,
        };
    };

    // Rule 3 — cap pressure eats the still-in-grace retained generations,
    // oldest-supersede first (`surviving` already carries that order).
    for &(idx, size) in &surviving {
        if held_bytes <= cap {
            break;
        }
        reclaim.push(idx);
        held_bytes = held_bytes.saturating_sub(size);
    }

    // Rule 1 — what is left is live, and live is never evicted. If it alone
    // meets the cap, that is the state the user is told about.
    let cap_state = if held_bytes >= cap {
        CapState::Reached
    } else {
        CapState::Ok
    };

    ReclaimPlan {
        reclaim,
        held_bytes,
        cap_state,
    }
}

/// How long this custodian has been overdue, if it is.
///
/// `last_checkin_at` is `None` for a custodian that has never checked in — which
/// is **not** overdue, for the reason [`crate::audit::evaluate_overdue`] treats a
/// never-audited destination the same way: a device enrolled minutes ago has done
/// nothing wrong, and alarming on it would fire for every enrollment.
pub fn checkin_overdue(last_checkin_at: Option<i64>, now: i64) -> Option<i64> {
    let last = last_checkin_at?;
    let elapsed = now.saturating_sub(last);
    (elapsed > CUSTODIAN_OVERDUE_SECS).then_some(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;

    fn held_gen(path: &str, hash: &str, size: u64, stored_at: i64) -> HeldGeneration {
        HeldGeneration {
            path: path.into(),
            manifest_hash: hash.into(),
            size_bytes: size,
            stored_at,
            deleted: false,
        }
    }

    fn tombstone(path: &str, stored_at: i64) -> HeldGeneration {
        HeldGeneration {
            path: path.into(),
            manifest_hash: "deadbeef".into(),
            size_bytes: 0,
            stored_at,
            deleted: true,
        }
    }

    /// The rule the module exists for: a live path is never reclaimed, even when
    /// it alone blows the cap. The custodian stops pulling instead.
    #[test]
    fn a_live_generation_is_never_reclaimed_even_over_cap() {
        let held = vec![
            held_gen("a/seg-1.dat", "aa", 900, 0),
            held_gen("b/seg-1.dat", "bb", 900, 0),
        ];
        let plan = plan_reclaim(&held, Some(1000), 10 * DAY);

        assert!(
            plan.reclaim.is_empty(),
            "live generations must never be reclaimed, got {:?}",
            plan.reclaim
        );
        assert_eq!(plan.held_bytes, 1800);
        assert_eq!(plan.cap_state, CapState::Reached);
        assert!(
            !plan.admits(1, Some(1000)),
            "a cap-reached custodian admits nothing"
        );
    }

    /// Ordinary garbage collection: past the grace window a superseded generation
    /// goes, with no cap pressure needed.
    #[test]
    fn a_retained_generation_past_the_grace_window_reclaims_without_cap_pressure() {
        let held = vec![
            held_gen("a/seg-1.dat", "old", 100, 0),
            held_gen("a/seg-1.dat", "new", 100, DAY),
        ];
        // Superseded at 1d, so the window is measured from there — not from the
        // old generation's own `stored_at`.
        let plan = plan_reclaim(&held, Some(1_000_000), 32 * DAY);

        assert_eq!(
            plan.reclaim,
            vec![0],
            "superseded 31d ago, past the 30d window"
        );
        assert_eq!(plan.held_bytes, 100, "only the live generation remains");
        assert_eq!(plan.cap_state, CapState::Ok);
    }

    /// Inside the window it stays — that retention *is* the rogue-source
    /// mitigation, so space alone must not be what ends it.
    #[test]
    fn a_retained_generation_inside_the_grace_window_survives_when_there_is_room() {
        let held = vec![
            held_gen("a/seg-1.dat", "old", 100, 0),
            held_gen("a/seg-1.dat", "new", 100, 5 * DAY),
        ];
        let plan = plan_reclaim(&held, Some(1_000_000), 5 * DAY);

        assert!(plan.reclaim.is_empty());
        assert_eq!(plan.held_bytes, 200, "both generations are still held");
        assert_eq!(plan.cap_state, CapState::Ok);
    }

    /// Cap pressure reclaims in-grace retained generations *before* reporting
    /// cap-reached — the ratified ordering.
    #[test]
    fn cap_pressure_reclaims_in_grace_retained_before_reporting_reached() {
        let held = vec![
            held_gen("a/seg-1.dat", "old", 400, 0),
            held_gen("a/seg-1.dat", "new", 400, 5 * DAY),
        ];
        let plan = plan_reclaim(&held, Some(500), 5 * DAY);

        assert_eq!(
            plan.reclaim,
            vec![0],
            "the in-grace retained copy gives way"
        );
        assert_eq!(plan.held_bytes, 400);
        assert_eq!(
            plan.cap_state,
            CapState::Ok,
            "reclaiming got us under the cap, so pulling continues"
        );
    }

    /// Under pressure the oldest supersede goes first.
    #[test]
    fn cap_pressure_reclaims_oldest_supersede_first() {
        let held = vec![
            held_gen("a/seg-1.dat", "a-old", 100, 0),
            held_gen("a/seg-1.dat", "a-new", 100, 9 * DAY),
            held_gen("b/seg-1.dat", "b-old", 100, 0),
            held_gen("b/seg-1.dat", "b-new", 100, 3 * DAY),
        ];
        // Live = 200. One retained (100) must go to get under 350.
        let plan = plan_reclaim(&held, Some(350), 9 * DAY);

        assert_eq!(
            plan.reclaim,
            vec![2],
            "b's copy was superseded at 3d, a's at 9d — the older supersede goes first"
        );
        assert_eq!(plan.held_bytes, 300);
        assert_eq!(plan.cap_state, CapState::Ok);
    }

    /// A deleted path ends liveness, and the generations beneath it age out on
    /// the tombstone's clock rather than being dropped on sight.
    #[test]
    fn a_tombstoned_path_has_no_live_generation_and_ages_out_on_the_tombstone() {
        let held = vec![
            held_gen("a/seg-1.dat", "body", 100, 0),
            tombstone("a/seg-1.dat", 5 * DAY),
        ];

        let inside = plan_reclaim(&held, Some(1_000_000), 5 * DAY);
        assert!(
            inside.reclaim.is_empty(),
            "the deleted body is still inside its grace window"
        );
        assert_eq!(
            inside.held_bytes, 100,
            "no live bytes, but the retained body is held"
        );

        let outside = plan_reclaim(&held, Some(1_000_000), 36 * DAY);
        let mut reclaimed = outside.reclaim.clone();
        reclaimed.sort_unstable();
        assert_eq!(
            reclaimed,
            vec![0, 1],
            "past the window both the tombstone and the body it covers reclaim"
        );
        assert_eq!(outside.held_bytes, 0);
    }

    /// No cap recorded is not the same as a cap of zero: it never reports
    /// cap-reached, but it still collects garbage.
    #[test]
    fn an_uncapped_custodian_never_reports_reached_but_still_collects() {
        let held = vec![
            held_gen("a/seg-1.dat", "old", 10_000, 0),
            held_gen("a/seg-1.dat", "new", 10_000, DAY),
        ];
        let plan = plan_reclaim(&held, None, 32 * DAY);

        assert_eq!(plan.reclaim, vec![0], "expired retention still goes");
        assert_eq!(plan.held_bytes, 10_000);
        assert_eq!(plan.cap_state, CapState::Ok);
        assert!(plan.admits(u64::MAX, None));
    }

    /// `held_bytes` is what the check-in reports, so it must count exactly the
    /// generations that survive the plan.
    #[test]
    fn held_bytes_counts_live_plus_surviving_retained() {
        let held = vec![
            held_gen("a/seg-1.dat", "a-live", 50, 2 * DAY),
            held_gen("a/seg-1.dat", "a-kept", 30, DAY),
            held_gen("a/seg-1.dat", "a-gone", 20, 0),
            held_gen("b/seg-1.dat", "b-live", 70, 0),
        ];
        // `a-gone` was superseded at 1d (30d ago); `a-kept` at 2d (29d ago), so
        // exactly one of them crosses the window.
        let plan = plan_reclaim(&held, Some(1_000_000), 31 * DAY);

        assert_eq!(plan.reclaim, vec![2]);
        assert_eq!(plan.held_bytes, 50 + 30 + 70);
    }

    /// The check-in carries the wire strings verbatim.
    #[test]
    fn cap_state_maps_to_the_wire_constants() {
        assert_eq!(CapState::Ok.as_wire(), CAP_STATE_OK);
        assert_eq!(CapState::Reached.as_wire(), CAP_STATE_REACHED);
    }

    /// Admission is the pull loop's gate: room left means room for what fits.
    #[test]
    fn admission_respects_the_remaining_headroom() {
        let held = vec![held_gen("a/seg-1.dat", "aa", 400, 0)];
        let plan = plan_reclaim(&held, Some(1000), 0);

        assert_eq!(plan.cap_state, CapState::Ok);
        assert!(
            plan.admits(600, Some(1000)),
            "exactly filling the cap is allowed"
        );
        assert!(!plan.admits(601, Some(1000)));
    }

    /// A device asleep for a week is normal; gone for a month is a failure.
    #[test]
    fn overdue_is_a_month_not_the_audit_loops_week() {
        // A custodian must tolerate longer absence than an audit pass.
        const { assert!(CUSTODIAN_OVERDUE_SECS > crate::audit::AUDIT_OVERDUE_SECS) };
        assert_eq!(
            checkin_overdue(Some(0), 7 * DAY),
            None,
            "a week asleep is normal"
        );
        assert_eq!(checkin_overdue(Some(0), 31 * DAY), Some(31 * DAY));
    }

    /// A custodian that has never checked in is not overdue — same rule the
    /// audit loop applies to a never-audited destination.
    #[test]
    fn a_custodian_that_never_checked_in_is_not_overdue() {
        assert_eq!(checkin_overdue(None, 365 * DAY), None);
    }
}
