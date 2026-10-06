//! The post-succession **email-filter raise** — the step that turns the rules a
//! ceremony carried across from a silent inheritance into durable review items.
//!
//! # Why this plane needs a classifier where the member plane needs none
//!
//! [`crate::raise_succession_member_reviews`] is handed a *roster*: the group
//! sweep already knows exactly whom it could not vouch for, so the set is
//! carried, not derived. There is no roster here. `email_filters` is nest-side
//! SQL under a single authority and the ceremony re-points `owner` in place, so
//! after the succession the successor simply owns every rule the predecessor
//! owned, with nothing on the row to say which era it came from. The inherited
//! set therefore has to be **classified**, and the only discriminator on the row
//! is `EmailFilter::created_at`.
//!
//! # The bound is the nest's own stamp, and the reason is the adversary
//!
//! The bound is [`SuccessionTime`] — `actor_successions.succeeded_at`, the
//! moment the *nest* committed the ceremony, threaded from
//! `SuccessionSubmitReply` through `SuccessionHandoff::succeeded_at`.
//!
//! ⚠ **The statement's own `IdentitySuccession::created_at` is NOT an
//! acceptable substitute**, and not merely because it is the client's clock
//! (`fauna_client_recovery::SuccessionHandoff::succeeded_at` already says that).
//! It is stamped when the statement is *authored*, which is before the submit
//! that commits it — and the seed thief this whole ceremony answers keeps full
//! authority right up to that commit. A rule the thief adds in the window
//! between authoring and commit has `created_at` **after** the statement's
//! stamp, so a statement-clock bound would carry it across unmarked: precisely
//! the silently-hidden flagged row the adjudication ruling exists to prevent.
//! The nest's stamp closes that window exactly, and it is the same clock that
//! wrote every `created_at` being compared against.
//!
//! # The lost-reply path asks the nest, and only then gives up
//!
//! `succeeded_at` reaches a client in the submit reply. A ceremony whose reply
//! was lost finishes through `reconcile_succession`, which runs over an
//! **anonymous** connector by construction (the scenario is an owner with no
//! working session) and therefore returns no stamp at all. That used to end the
//! story: the raise reported [`FilterMarkRaise::SuccessionTimeUnknown`] and a
//! carried-across rule was never surfaced for review.
//!
//! It no longer does. This raise runs in the successor's post-store-ready pass,
//! which holds the successor's **signed-in** connection, so when the parked
//! ceremony carries no stamp it asks the nest for its own: `fauna.recovery.succession.status`
//! ([`fauna_protocol::recovery::SUCCESSION_STATUS_KIND`]), an authenticated,
//! self-scoped read of `actor_successions.succeeded_at`.
//!
//! ⚠ **Closed HERE rather than by threading the value back into
//! `SuccessionHandoff::succeeded_at`**, which is where a reader would look for
//! it first. Two reasons, and the second is the load-bearing one. The reconcile
//! is anonymous, so the authenticated read is unreachable from inside it without
//! handing that crate a second, signed-in connection it has no other use for.
//! And this is the *only* consumer of the stamp: closing it at the consumer
//! closes it for all seven apps at once, where threading it through the handoff
//! would close it for whichever app carries the value across its account switch
//! (tui today) and silently leave the other six reporting the residual —
//! per-app divergence bought for nothing (priority #1).
//!
//! ⚠ **The read is a best-effort *widening*, never a dependency.** Any unhappy
//! answer — a refusal, a dropped
//! connection — degrades to exactly the pre-existing behaviour
//! ([`FilterMarkRaise::SuccessionTimeUnknown`], nothing written), which is what
//! keeps the aftermath leg from failing on a read it does not depend on.
//!
//! When neither the device nor the nest can supply the bound, reporting the
//! non-event is honest rather than hidden, and it is the same shape as the
//! member plane's declared residual. The refused alternatives are both worse:
//! guessing the statement's clock re-opens the window above, and falling back to
//! "every filter I own right now" is indistinguishable from the correct answer
//! in the successor's first session and wrong on every later re-run, once the
//! successor has authored rules of their own — it would raise the successor's
//! own rules against their own predecessor, and the `(filter_id, predecessor)`
//! key would make that permanent.

use fauna_core::data::UnattestedVerdict;
use fauna_core::identity::ActorId;
use fauna_protocol::RpcRequester;
use fauna_protocol::email::EmailFilter;
use fauna_protocol::recovery::{
    SUCCESSION_STATUS_KIND, SuccessionStatusReply, SuccessionStatusRequest,
};

use crate::store_seam::{StoreError, SuccessionLedgerStore};

/// The succession's **recorded time**, in the unit the rows are compared in.
///
/// A newtype rather than a bare `i64` because the two values this sits between
/// are stored in *different units* and the mistake is silent: the nest writes
/// `actor_successions.succeeded_at` in epoch **seconds** (`now_epoch_secs`) and
/// `email_filters.created_at` in epoch **milliseconds** (`now_epoch_millis`).
/// A raw comparison of the two classifies every rule ever written as *not*
/// inherited, and nothing about the resulting empty review surface looks wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuccessionTime(i64);

impl SuccessionTime {
    /// Build the bound from the nest's `succeeded_at`, in epoch **seconds**.
    ///
    /// ⚠ **Rounded up to the end of that second, deliberately.** A second-grained
    /// stamp cannot say where inside its second the commit fell, and the two
    /// roundings are not symmetric. Rounding *down* under-marks: a rule the
    /// thief created at `S.500s`, before a commit the nest stamped `S`, would
    /// compare as created after the succession and be carried across unflagged
    /// — the hiding direction. Rounding up can only over-mark a rule created
    /// later in the same second, which no one can do: the thief has lost
    /// authority the instant the commit lands, and the successor is not signed
    /// in yet. Re-asking is harmless; hiding is not.
    pub fn from_nest_seconds(secs: i64) -> Self {
        Self(secs.saturating_mul(1000).saturating_add(999))
    }

    /// The bound as epoch milliseconds — the unit `EmailFilter::created_at` is
    /// stored and sent in.
    pub fn as_millis(self) -> i64 {
        self.0
    }
}

/// The rules a succession carried across: those created at or before the
/// ceremony's recorded time.
///
/// Pure and total — the one classifier, so no app re-derives the bound. Returns
/// ids in ascending order, so the raise is a function of its input rather than
/// of the order the nest happened to list rows in.
pub fn inherited_filter_ids(filters: &[EmailFilter], at: SuccessionTime) -> Vec<i64> {
    let mut ids: Vec<i64> = filters
        .iter()
        .filter(|f| f.created_at <= at.as_millis())
        .map(|f| f.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// What [`raise_succession_filter_marks`] did.
///
/// Four values, for the reason [`crate::MemberReviewRaise`] has three: most of
/// them mean "no write happened", and they are not the same non-event. A caller
/// that collapsed them could not tell "this account had no inherited rules"
/// from "this ceremony's rules were never adjudicable because its stamp was
/// lost".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMarkRaise {
    /// No rule predates the succession — the ordinary state for an account that
    /// never used filters. Costs no round trip.
    NothingToRaise,
    /// Every inherited rule is already on file for this raising event. The
    /// idempotent arm: a re-run of the post-store-ready pass. Costs one read
    /// and writes nothing.
    AlreadyRaised,
    /// Marks were added. Carries how many rules are newly on file, which is
    /// **not** the inherited-set size on a resumed pass.
    Raised {
        /// Rules newly raised by this call.
        filters: usize,
    },
    /// The ceremony's nest-recorded time is not available **from the caller or
    /// from the nest**, so the inherited set cannot be classified and **nothing
    /// is raised**. See the module docs for why neither fallback is acceptable.
    ///
    /// ⚠ Narrower than it used to be: a lost submit reply alone no longer
    /// reaches this arm, because the raise asks
    /// `fauna.recovery.succession.status` for the stamp it lacks. What is left
    /// is a nest that cannot answer — a refusal or a
    /// connection that dropped mid-pass — plus the genuine "this actor is not a
    /// successor" case, where there is nothing to classify against anyway.
    SuccessionTimeUnknown,
}

/// Raise one open mark per email filter rule the succession carried across,
/// attributed to the retired identity.
///
/// `predecessor` is the identity the ceremony retired — the raising event, and
/// what makes a *later* succession legitimately re-raise a rule a first *Keep*
/// closed. `filters` is the successor's current filter list, as
/// `fauna.email.filters.list` returns it; `succeeded_at` is the ceremony's
/// recorded time as the *caller* knows it, `None` when this device never
/// learned it — in which case this asks the nest over `nest` before giving up
/// (module docs § The lost-reply path). Passing `Some` is therefore an
/// optimisation, not a requirement: it saves one round trip on the ordinary
/// path where the submit reply did arrive.
///
/// The marks rest on the succession ledger's `filter-mark/…` rows, written
/// through `ledger` — the successor's own account store.
///
/// **Safe to call at every store-ready.** An empty inherited set
/// short-circuits before any read, and a set already on file writes nothing.
/// A refused write (the door's transient no-tip refusal) surfaces as `Err`;
/// the caller keeps its park and the next store-ready re-runs it.
pub async fn raise_succession_filter_marks<R: RpcRequester>(
    nest: &R,
    ledger: &dyn SuccessionLedgerStore,
    predecessor: ActorId,
    filters: &[EmailFilter],
    succeeded_at: Option<SuccessionTime>,
) -> Result<FilterMarkRaise, StoreError> {
    // Ordered before the emptiness check on purpose: "we could not classify" is
    // a different answer from "there was nothing to classify", and an account
    // whose list happens to be empty must not report the reassuring one on a
    // device that could not have told the difference anyway.
    let at = match succeeded_at {
        Some(at) => at,
        // The lost-reply path. This connection is the successor's own, so the
        // nest can be asked for the stamp the reply never delivered — and a
        // nest that cannot answer leaves the outcome exactly as it was.
        None => match fetch_succession_time(nest).await {
            Some(at) => at,
            None => return Ok(FilterMarkRaise::SuccessionTimeUnknown),
        },
    };
    let inherited = inherited_filter_ids(filters, at);
    if inherited.is_empty() {
        return Ok(FilterMarkRaise::NothingToRaise);
    }
    let mut current = ledger.load().await?;
    let raised = current.raise_filter_marks(inherited, predecessor);
    if raised == 0 {
        return Ok(FilterMarkRaise::AlreadyRaised);
    }
    ledger.merge(current.marks_replica()).await?;
    Ok(FilterMarkRaise::Raised { filters: raised })
}

/// Ask the nest when the succession that produced *this* caller committed —
/// `fauna.recovery.succession.status`.
///
/// Deliberately swallows every failure into `None`. The shapes it can
/// take are a refusal (`unknown_kind` included) and a
/// transport error, and not one of them is this pass's business: the caller's
/// contract is to classify when it can and report the non-event when it cannot,
/// so a raise that *failed* on an unreachable nest would turn a widening into a
/// regression for every client newer than its box.
///
/// ⚠ **`None` here is not "no succession"** — it is "no bound available", which
/// is also what an actor that never succeeded returns. The two collapse safely
/// because the caller only reaches this with a `predecessor` in hand, and a
/// nest that has no row for a ceremony this device ran cannot classify it
/// either.
async fn fetch_succession_time<R: RpcRequester>(nest: &R) -> Option<SuccessionTime> {
    fetch_succession_status_secs(nest)
        .await
        .map(SuccessionTime::from_nest_seconds)
}

/// The raw round trip behind [`fetch_succession_time`], in the reply's own
/// **epoch-seconds** unit — `pub(crate)` so
/// [`crate::nostr_npub_confirm`] can share the one `fauna.recovery.succession.status`
/// call rather than re-issuing it. That plane consumes the raw seconds
/// directly (never through [`SuccessionTime`]'s millis rounding, which exists
/// only because `email_filters.created_at` is stored in milliseconds — both
/// sides of the npub-confirm comparison are already seconds).
///
/// Same swallow-every-failure-into-`None` contract as [`fetch_succession_time`]
/// — see its docs.
pub(crate) async fn fetch_succession_status_secs<R: RpcRequester>(nest: &R) -> Option<i64> {
    let reply: SuccessionStatusReply = nest
        .request(SUCCESSION_STATUS_KIND, SuccessionStatusRequest::default())
        .await
        .ok()?;
    reply.succeeded_at
}

/// Read the ids of every filter rule still awaiting the owner's verdict — what
/// the filter list marks and what the aftermath review item counts.
///
/// A thin read on purpose, the twin of [`crate::load_member_reviews`]: the
/// *projection* is
/// [`fauna_core::succession_ledger::SuccessionLedger::open_filter_reviews`],
/// which owns the dedup-across-raising-events collapse and the fail-visible
/// reading of an unrecognized verdict. No app reaches past this seam into the
/// filter-mark rows.
///
/// Apps **cache** what this returns and answer per-row questions against it: a
/// filter list paints far more often than the ledger changes.
pub async fn load_filter_marks(ledger: &dyn SuccessionLedgerStore) -> Result<Vec<i64>, StoreError> {
    Ok(ledger.load().await?.open_filter_reviews())
}

/// Record the owner's verdict on every open mark for one filter rule — what
/// **Keep** and **Remove** both do.
///
/// Returns whether anything was actually open, so a caller can tell a real
/// adjudication from a no-op. A no-op is not an error: the owner may press Keep
/// on a row a concurrent device already answered, and the honest outcome is the
/// same either way.
///
/// ⚠ **`Removed` here is RECORDED, not enforced** — this plane's one deliberate
/// divergence from the destination plane. The rule itself lives in nest-side
/// SQL, so a caller passing [`UnattestedVerdict::Removed`] owes the deletion
/// through the filter list's existing `fauna.email.filters.delete`; recording
/// the verdict alone leaves an armed rule running under a list that now reads
/// clean. There is deliberately no second removal mechanism.
///
/// Two devices racing converge without a CAS: each mark row's arm is the
/// verdict-precedence minimum, so a decided verdict beats a still-`Open` one
/// whichever lands first — the property the verdict-at-rest encoding was
/// ratified for.
pub async fn decide_filter_mark(
    ledger: &dyn SuccessionLedgerStore,
    filter_id: i64,
    verdict: UnattestedVerdict,
) -> Result<bool, StoreError> {
    let mut current = ledger.load().await?;
    if !current.decide_filter_marks_for(filter_id, verdict) {
        return Ok(false);
    }
    ledger.merge(current.marks_replica()).await?;
    Ok(true)
}

/// The pure half of the raise — what an inherited set does to a ledger, with no
/// store. Split out for the reason the member plane's twin is: the keying rule
/// stays testable without one.
#[cfg(test)]
fn raise_into(
    ledger: &mut fauna_core::succession_ledger::SuccessionLedger,
    filters: &[EmailFilter],
    at: SuccessionTime,
    by: ActorId,
) -> usize {
    ledger.raise_filter_marks(inherited_filter_ids(filters, at), by)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeSuccessionLedgerStore;
    use crate::test_nest::{FakeConfigNest, block_on};
    use fauna_core::succession_ledger::SuccessionLedger;
    use std::sync::Arc;

    fn ledger() -> FakeSuccessionLedgerStore {
        FakeSuccessionLedgerStore::empty(ActorId([1u8; 32]))
    }

    fn filter(id: i64, created_at: i64) -> EmailFilter {
        EmailFilter {
            id,
            name: format!("rule-{id}"),
            rules: Vec::new(),
            combination: "all".into(),
            action: fauna_protocol::email::EmailFilterAction::Discard,
            priority: 0,
            continue_on_match: false,
            created_at,
            extra: Default::default(),
        }
    }

    /// The classification the whole plane rests on: a rule created strictly
    /// **after** the succession is the successor's own and is never marked; one
    /// created at or before it is inherited.
    #[test]
    fn the_inherited_set_is_bounded_by_the_succession_time() {
        let at = SuccessionTime::from_nest_seconds(1_000);
        let rules = [
            filter(1, 500_000),   // long before
            filter(2, 1_000_999), // the last instant of the succession's second
            filter(3, 1_001_000), // the successor's own, one ms later
            filter(4, 9_000_000), // clearly the successor's own
        ];
        assert_eq!(inherited_filter_ids(&rules, at), vec![1, 2]);
    }

    /// **The second-grained stamp rounds UP, and the direction is the whole
    /// safety property.** A thief's rule written part-way through the second the
    /// nest committed the ceremony compares as *later* than a truncated bound,
    /// so rounding down would carry it across unflagged — the hiding direction
    /// the adjudication ruling exists to prevent.
    #[test]
    fn a_rule_written_inside_the_succession_second_is_still_inherited() {
        let at = SuccessionTime::from_nest_seconds(1_000);
        let thiefs_last_rule = filter(9, 1_000_500);
        assert_eq!(
            inherited_filter_ids(&[thiefs_last_rule], at),
            vec![9],
            "truncating the stamp to whole seconds hides the thief's last rule"
        );
    }

    /// The unit conversion, pinned on its own: the two stamps this compares are
    /// stored in different units, and a raw comparison would classify every rule
    /// ever written as the successor's own — an empty review surface that looks
    /// exactly like a clean account.
    #[test]
    fn the_bound_is_carried_in_the_unit_the_rows_use() {
        assert_eq!(
            SuccessionTime::from_nest_seconds(1_753_400_000).as_millis(),
            1_753_400_000_999
        );
        let at = SuccessionTime::from_nest_seconds(1_753_400_000);
        let inherited_row = filter(1, 1_753_399_000_000);
        assert_eq!(
            inherited_filter_ids(&[inherited_row], at),
            vec![1],
            "seconds compared against milliseconds classifies everything as not-inherited"
        );
    }

    /// The `(filter_id, raising event)` keying, end to end through the pure
    /// half: a re-run of the same succession adds nothing, a later one re-raises.
    #[test]
    fn a_re_run_adds_nothing_but_a_later_succession_re_raises() {
        let at = SuccessionTime::from_nest_seconds(1_000);
        let rules = [filter(1, 500_000), filter(2, 600_000)];
        let first = ActorId([4u8; 32]);
        let second = ActorId([5u8; 32]);

        let mut cfg = SuccessionLedger::empty(ActorId([1u8; 32]));
        assert_eq!(raise_into(&mut cfg, &rules, at, first), 2);
        assert_eq!(
            raise_into(&mut cfg, &rules, at, first),
            0,
            "the re-run is silent"
        );
        assert_eq!(
            raise_into(&mut cfg, &rules, at, second),
            2,
            "a later event re-raises"
        );
    }

    /// **A lost submit reply must not be reported as a clean account.** The
    /// unknown-stamp arm is checked before the emptiness one, so a device that
    /// could not have classified anything says so rather than reporting the
    /// reassuring `NothingToRaise`.
    ///
    /// The nest here answers "not a successor", which is the only way this arm
    /// is still reachable on a current box — see the two tests below for the
    /// path that now closes instead.
    #[test]
    fn an_unknown_succession_time_raises_nothing_and_says_which_non_event_it_is() {
        let nest = Arc::new(FakeConfigNest::default());
        let raised = block_on(raise_succession_filter_marks(
            &nest,
            &ledger(),
            ActorId([4u8; 32]),
            &[],
            None,
        ))
        .expect("the raise");
        assert_eq!(raised, FilterMarkRaise::SuccessionTimeUnknown);
        assert!(
            nest.calls
                .lock()
                .unwrap()
                .contains(&fauna_protocol::recovery::SUCCESSION_STATUS_KIND),
            "the nest must actually be ASKED before the residual is reported — a \
             raise that reports `SuccessionTimeUnknown` without the round trip is \
             the pre-fix behaviour wearing the new docs"
        );
    }

    /// **The lost-reply path raises the same set the submit path raises.** The
    /// residual's closure, stated as the equivalence that matters: two identical
    /// ceremonies, one whose reply arrived (`Some(at)`) and one whose did not
    /// (`None`, stamp fetched from the nest), must mark the same rules.
    ///
    /// Two separate ledgers because each raise writes and the second would
    /// otherwise meet its own marks and report `AlreadyRaised`.
    #[test]
    fn the_reconciled_path_raises_the_same_marks_as_the_submit_path() {
        let predecessor = ActorId([4u8; 32]);
        // Rules 1 and 2 predate the ceremony; 3 is the successor's own.
        let rules = [
            filter(1, 500_000),
            filter(2, 1_000_999),
            filter(3, 5_000_000),
        ];
        let committed_at = 1_000;

        // The submit path: the reply carried the stamp.
        let submit_nest = Arc::new(FakeConfigNest::default());
        let submit_store = ledger();
        let submitted = block_on(raise_succession_filter_marks(
            &submit_nest,
            &submit_store,
            predecessor,
            &rules,
            Some(SuccessionTime::from_nest_seconds(committed_at)),
        ))
        .expect("the submit-path raise");

        // The reconcile path: no stamp in hand, the nest holds the row.
        let reconciled_nest = Arc::new(FakeConfigNest::default());
        *reconciled_nest.succession_committed_at.lock().unwrap() = Some(committed_at);
        let reconciled_store = ledger();
        let reconciled = block_on(raise_succession_filter_marks(
            &reconciled_nest,
            &reconciled_store,
            predecessor,
            &rules,
            None,
        ))
        .expect("the reconcile-path raise");

        assert_eq!(submitted, FilterMarkRaise::Raised { filters: 2 });
        assert_eq!(
            reconciled, submitted,
            "a ceremony whose reply was lost must classify exactly as one whose \
             reply arrived — that equivalence IS the residual's closure"
        );
        assert_eq!(
            block_on(load_filter_marks(&reconciled_store)).expect("read"),
            block_on(load_filter_marks(&submit_store)).expect("read"),
            "and it must be the same RULES, not merely the same count"
        );
        assert_eq!(
            block_on(load_filter_marks(&reconciled_store)).expect("read"),
            vec![1, 2],
            "rule 2 sits at the last millisecond of the commit's own second, which \
             the deliberate round-up marks; rule 3 is the successor's own"
        );
        // The submit path must not pay for the widening: it has the stamp, so
        // it never asks.
        assert!(
            !submit_nest
                .calls
                .lock()
                .unwrap()
                .contains(&fauna_protocol::recovery::SUCCESSION_STATUS_KIND),
            "a caller that already holds the stamp must cost no extra round trip"
        );
    }

    /// **A nest that cannot answer leaves the outcome exactly as it was.** A
    /// refusal or a transport fault on the read must degrade the raise to the
    /// residual rather than fail the whole aftermath leg.
    #[test]
    fn a_nest_that_cannot_serve_the_stamp_degrades_to_the_residual() {
        let nest = Arc::new(FakeConfigNest::default());
        *nest.succession_status_fails.lock().unwrap() = true;
        let raised = block_on(raise_succession_filter_marks(
            &nest,
            &ledger(),
            ActorId([4u8; 32]),
            &[filter(1, 500_000)],
            None,
        ))
        .expect("the raise must not surface the refusal as an error");
        assert_eq!(raised, FilterMarkRaise::SuccessionTimeUnknown);
    }

    /// The raise writes through the seam, and the surfaces' read sees only what
    /// is still open — the join a test over `open_filter_reviews` alone cannot
    /// make.
    #[test]
    fn the_raise_lands_and_the_read_returns_only_what_is_still_open() {
        let nest = Arc::new(FakeConfigNest::default());
        let store = ledger();
        let at = SuccessionTime::from_nest_seconds(1_000);
        let rules = [filter(1, 500_000), filter(2, 600_000), filter(3, 5_000_000)];

        let raised = block_on(raise_succession_filter_marks(
            &nest,
            &store,
            ActorId([4u8; 32]),
            &rules,
            Some(at),
        ))
        .expect("the raise");
        assert_eq!(
            raised,
            FilterMarkRaise::Raised { filters: 2 },
            "the successor's own rule 3 must not be raised against their predecessor"
        );

        assert_eq!(
            block_on(load_filter_marks(&store)).expect("read"),
            vec![1, 2]
        );
        assert!(block_on(decide_filter_mark(&store, 1, UnattestedVerdict::Kept)).expect("keep"));
        assert_eq!(
            block_on(load_filter_marks(&store)).expect("re-read"),
            vec![2],
            "an answered rule must not come back on the next paint"
        );
        let merges = store.merges();
        assert!(
            !block_on(decide_filter_mark(&store, 1, UnattestedVerdict::Removed)).expect("again"),
            "nothing was open, so the second gesture is a no-op rather than an overwrite"
        );
        assert_eq!(store.merges(), merges, "and a no-op writes nothing");
    }

    /// The idempotent arm costs a read and writes nothing — so a second
    /// store-ready re-runs the raise for free.
    #[test]
    fn a_second_pass_over_the_same_ceremony_reports_already_raised() {
        let nest = Arc::new(FakeConfigNest::default());
        let store = ledger();
        let at = SuccessionTime::from_nest_seconds(1_000);
        let rules = [filter(1, 500_000)];
        let raise = || {
            block_on(raise_succession_filter_marks(
                &nest,
                &store,
                ActorId([4u8; 32]),
                &rules,
                Some(at),
            ))
            .expect("the raise")
        };
        assert_eq!(raise(), FilterMarkRaise::Raised { filters: 1 });
        assert_eq!(raise(), FilterMarkRaise::AlreadyRaised);
        assert_eq!(store.merges(), 1);
    }

    /// A refused write (the door's transient no-tip refusal) is an error, never
    /// a quiet non-event: the caller's park must survive it.
    #[test]
    fn a_refused_put_surfaces_as_an_error() {
        let nest = Arc::new(FakeConfigNest::default());
        let store = ledger();
        store.refuse_next_merges(1);
        assert!(
            block_on(raise_succession_filter_marks(
                &nest,
                &store,
                ActorId([4u8; 32]),
                &[filter(1, 500_000)],
                Some(SuccessionTime::from_nest_seconds(1_000)),
            ))
            .is_err()
        );
        assert!(store.current().unattested_filter_marks.is_empty());
    }
}
