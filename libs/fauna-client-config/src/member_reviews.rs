//! The post-succession sweep's **roster write** — the one step that turns
//! `SweepReport::unattested_members` from a rendered count into durable review
//! items (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS
//! groups*, the two-surface roster) — and the read/decide pair every
//! unattested-member surface renders from.
//!
//! # Where the items rest
//!
//! On the succession ledger's `member-item/…` rows
//! (`fauna.state.succession-ledger`, `config-dissolution.md` § Phases and
//! gates → *Bounded rows* → *The ledger*), reached through the
//! [`SuccessionLedgerStore`] seam.
//!
//! # Why the raise is a step of its own, run after store-ready
//!
//! The sweep runs **pre-switch, as the predecessor** — it must, since that is
//! the only moment both MLS engines are co-resident — and at that moment the
//! successor's account store does not exist yet. So the ceremony PARKS the
//! roster durably in the account registry (`PendingCeremony`, beside
//! `record_succession`) and the successor's post-store-ready pass
//! (`fauna_client_recovery::run_ledger_aftermath`) raises it here, clearing the
//! park only once the put landed (`succession-aftermath.md` § Re-key scope →
//! *Adjudicating what the aftermath carries across*, the 2026-09-30
//! paragraph).
//!
//! The raising predecessor is a parameter, supplied by the park that names the
//! ceremony: "which succession raised this" is a specific ceremony, never
//! "whichever predecessor key happens to open something".

use fauna_core::data::{MemberReview, MemberUnattestedReason, UnattestedVerdict};
use fauna_core::identity::ActorId;
use fauna_core::succession_ledger::SuccessionLedger;

use crate::store_seam::{StoreError, SuccessionLedgerStore};

/// What [`raise_succession_member_reviews`] did.
///
/// Three values because two of them mean "no write happened", and they are
/// not the same non-event: a ceremony with nothing to report is not a ceremony
/// whose report is already on file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberReviewRaise {
    /// The sweep vouched for everyone it saw — an empty roster. Costs no read.
    NothingToRaise,
    /// Every person on the roster is already on file for this raising event.
    /// The idempotent arm: a re-run of the post-store-ready pass. Costs one
    /// read and writes nothing.
    AlreadyRaised,
    /// Items were added. Carries how many people are newly on file, which is
    /// **not** the roster length on a resumed pass.
    Raised {
        /// People newly raised by this call.
        people: usize,
    },
}

/// Raise one open review item per unattested member the succession's group
/// sweep reported, attributed to the retired identity.
///
/// `predecessor` is the identity the ceremony retired — the raising event, and
/// the thing that makes a *later* succession legitimately re-raise a person a
/// first *Keep* closed. `people` is [`SweepReport::unattested_members`]'
/// deduplicated roster, as the ceremony parked it.
///
/// **Safe to call at every store-ready.** An empty roster short-circuits
/// before any read, and a roster already on file writes nothing. A refused
/// write (the door's transient no-tip refusal) surfaces as `Err`; the caller
/// keeps the park and the next store-ready re-runs it.
///
/// [`SweepReport::unattested_members`]: https://docs.rs/fauna-client-recovery
pub async fn raise_succession_member_reviews(
    ledger: &dyn SuccessionLedgerStore,
    predecessor: ActorId,
    people: &[ActorId],
) -> Result<MemberReviewRaise, StoreError> {
    if people.is_empty() {
        return Ok(MemberReviewRaise::NothingToRaise);
    }
    let mut current = ledger.load().await?;
    let raised = raise_into(&mut current, people, predecessor);
    if raised == 0 {
        return Ok(MemberReviewRaise::AlreadyRaised);
    }
    ledger.merge(current.marks_replica()).await?;
    Ok(MemberReviewRaise::Raised { people: raised })
}

/// Read the open review roster every unattested-member surface renders from.
///
/// A thin read on purpose: the *projection* is
/// [`SuccessionLedger::open_member_reviews`], which owns the one-row-per-person
/// collapse, and this is only the seam that fetches it. It exists so that no
/// app reaches past the seam into the member-item rows — the at-rest shape
/// carries decided items too, and a surface that filtered them itself would
/// have to re-derive [`UnattestedVerdict::is_open`]'s fail-visible rule (an
/// unrecognized verdict renders as still open).
///
/// Apps **cache** what this returns and answer per-row questions with
/// [`fauna_core::data::is_under_review`]: a member list paints far more often
/// than the ledger changes.
pub async fn load_member_reviews(
    ledger: &dyn SuccessionLedgerStore,
) -> Result<Vec<MemberReview>, StoreError> {
    Ok(ledger.load().await?.open_member_reviews())
}

/// Record the owner's verdict on every open item for one person — what **Keep**
/// and **Remove** both do (`identity-succession.md` § Propagation → *MLS
/// groups*).
///
/// Returns whether anything was actually open, so a caller can tell a real
/// adjudication from a no-op. **A no-op is not an error:** the owner may press
/// Keep on a row a concurrent device already answered, and the honest outcome is
/// the same either way — the person is no longer raised.
///
/// Two devices racing converge without a CAS: each item row's arm is the
/// verdict-precedence minimum, so a decided verdict beats a still-`Open` one
/// whichever lands first — a *Keep* on the laptop is never undone by an
/// ordinary pass on the phone (the 2026-08-06 verdict-at-rest ruling).
///
/// ⚠ **The skip is a real property, not an optimization.** A press on an
/// already-answered person writes nothing, so a repeated press stays cheap and
/// never puts a row a concurrent device's verdict already moved.
pub async fn decide_member_review(
    ledger: &dyn SuccessionLedgerStore,
    person: &ActorId,
    verdict: UnattestedVerdict,
) -> Result<bool, StoreError> {
    let mut current = ledger.load().await?;
    if !current.decide_member_reviews_for(person, verdict) {
        return Ok(false);
    }
    ledger.merge(current.marks_replica()).await?;
    Ok(true)
}

/// The pure half — what the roster does to a ledger, with no store.
///
/// Split out so the reason-and-attribution rule is testable without a store,
/// and so the one place that chooses [`MemberUnattestedReason::CompromiseWindow`]
/// is named rather than inlined at a call site.
fn raise_into(ledger: &mut SuccessionLedger, people: &[ActorId], predecessor: ActorId) -> usize {
    ledger.raise_member_reviews(
        people.iter().copied(),
        predecessor,
        // The sweep's roster has exactly one meaning: these people sat in a
        // group across the compromise window the succession closed. The witness's
        // *unverifiable* arm is the foreseen second producer and will pass its
        // own reason (`MemberUnattestedReason`'s docs).
        MemberUnattestedReason::CompromiseWindow,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeSuccessionLedgerStore;
    use crate::test_nest::block_on;

    fn actor(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    /// The surfaces' read is the *open* projection: a person the owner has
    /// already answered must not come back on the next paint.
    #[test]
    fn the_roster_read_returns_only_what_is_still_open() {
        let kept = actor(7);
        let still_open = actor(8);
        let mut seeded = SuccessionLedger::empty(actor(1));
        raise_into(&mut seeded, &[kept, still_open], actor(2));
        seeded.decide_member_reviews_for(&kept, UnattestedVerdict::Kept);
        let store = FakeSuccessionLedgerStore::with(seeded);

        let roster = block_on(load_member_reviews(&store)).expect("the surfaces' read");
        assert_eq!(
            roster.iter().map(|r| r.person).collect::<Vec<_>>(),
            vec![still_open],
            "an answered person must not be re-asked: {roster:?}"
        );
    }

    /// **Keep** writes the verdict through the seam and the next read drops the
    /// row — the whole observable a member list's mark has.
    ///
    /// The second press is the concurrent-device case: a person another device
    /// already answered is a no-op, reported as `false` rather than as an error,
    /// and it must not write.
    #[test]
    fn a_verdict_lands_at_rest_and_a_repeat_press_writes_nothing() {
        let person = actor(7);
        let store = FakeSuccessionLedgerStore::empty(actor(1));
        block_on(raise_succession_member_reviews(&store, actor(2), &[person])).expect("raise");

        assert!(
            block_on(decide_member_review(
                &store,
                &person,
                UnattestedVerdict::Kept
            ))
            .expect("the Keep"),
            "the first press adjudicates a really-open item"
        );
        assert!(
            block_on(load_member_reviews(&store))
                .expect("re-read")
                .is_empty(),
            "the mark must clear on the next paint"
        );
        // The verdict is at REST, not deleted — which is what stops a re-run of
        // the raising pass from re-asking (`UnattestedVerdict`'s two failures).
        assert_eq!(
            store
                .current()
                .unattested_member_items
                .iter()
                .map(|item| item.verdict.clone())
                .collect::<Vec<_>>(),
            vec![UnattestedVerdict::Kept],
        );

        let merges = store.merges();
        assert!(
            !block_on(decide_member_review(
                &store,
                &person,
                UnattestedVerdict::Kept
            ))
            .expect("the repeat press"),
            "a person already answered is a no-op, not an error"
        );
        assert_eq!(store.merges(), merges, "and it must not write");
    }

    /// A person nobody raised is not an error either — the arm a surface reaches
    /// when its cached roster is one paint behind a peer device's verdict.
    #[test]
    fn deciding_an_unraised_person_is_a_silent_no_op() {
        let store = FakeSuccessionLedgerStore::empty(actor(1));
        assert!(
            !block_on(decide_member_review(
                &store,
                &actor(9),
                UnattestedVerdict::Removed
            ))
            .expect("the stale press"),
            "nothing was open, so nothing was adjudicated"
        );
        assert_eq!(store.merges(), 0);
    }

    /// The raise reaches the ledger as open items attributed to the retired
    /// identity, a re-run writes nothing, and a refused put surfaces as `Err`
    /// (the caller keeps its park) with nothing at rest.
    #[test]
    fn the_roster_lands_once_and_a_refused_put_is_an_error() {
        let store = FakeSuccessionLedgerStore::empty(actor(1));
        store.refuse_next_merges(1);
        assert!(
            block_on(raise_succession_member_reviews(
                &store,
                actor(2),
                &[actor(7)]
            ))
            .is_err(),
            "a door refusal must surface, or the park would be cleared over nothing"
        );
        assert!(store.current().unattested_member_items.is_empty());

        let roster = [actor(7), actor(8)];
        assert_eq!(
            block_on(raise_succession_member_reviews(&store, actor(2), &roster))
                .expect("the raise"),
            MemberReviewRaise::Raised { people: 2 }
        );
        assert!(
            store
                .current()
                .unattested_member_items
                .iter()
                .all(|item| item.predecessor == actor(2)),
            "attributed to the identity the ceremony retired"
        );
        assert_eq!(
            block_on(raise_succession_member_reviews(&store, actor(2), &roster))
                .expect("the resumed raise"),
            MemberReviewRaise::AlreadyRaised,
            "a resumed pass must write nothing"
        );
    }

    /// An empty roster costs no read at all — the state every ordinary
    /// store-ready is in.
    #[test]
    fn an_empty_roster_touches_no_store() {
        let store = FakeSuccessionLedgerStore::empty(actor(1));
        store.refuse_next_merges(1);
        assert_eq!(
            block_on(raise_succession_member_reviews(&store, actor(2), &[])).expect("the raise"),
            MemberReviewRaise::NothingToRaise
        );
    }

    /// The roster reaches the ledger as one open item per person, attributed to
    /// the retired identity — and a second pass over the same roster adds
    /// nothing.
    #[test]
    fn the_roster_lands_as_open_items_attributed_to_the_predecessor() {
        let mut ledger = SuccessionLedger::empty(actor(1));
        let roster = [actor(7), actor(8)];

        assert_eq!(raise_into(&mut ledger, &roster, actor(2)), 2);
        let reviews = ledger.open_member_reviews();
        assert_eq!(reviews.len(), 2, "one row per person: {reviews:?}");
        assert!(
            ledger
                .unattested_member_items
                .iter()
                .all(|item| item.reason == MemberUnattestedReason::CompromiseWindow),
            "the sweep's roster means exactly one thing"
        );
        assert_eq!(
            raise_into(&mut ledger, &roster, actor(2)),
            0,
            "a resumed sweep re-reports the same roster and must add nothing"
        );
    }

    /// A *second* succession is a different raising event, so it legitimately
    /// asks about the same person again — which is the whole reason the items
    /// are keyed on `(person, raising event)` rather than on the person.
    #[test]
    fn a_later_succession_raises_the_same_person_again() {
        let mut ledger = SuccessionLedger::empty(actor(1));
        let roster = [actor(7)];
        assert_eq!(raise_into(&mut ledger, &roster, actor(2)), 1);
        ledger.decide_member_reviews_for(&actor(7), UnattestedVerdict::Kept);
        assert!(
            ledger.open_member_reviews().is_empty(),
            "the Keep closed it"
        );

        assert_eq!(
            raise_into(&mut ledger, &roster, actor(3)),
            1,
            "a different predecessor is a different raising event"
        );
        assert_eq!(ledger.open_member_reviews().len(), 1);
    }
}
