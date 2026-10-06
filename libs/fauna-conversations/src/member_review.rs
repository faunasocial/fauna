//! The unattested-member review's **list-form** join, for the surfaces that
//! render a whole member roster at once (`succession-aftermath.md` § Propagation
//! → *MLS groups*, item 3a).
//!
//! [`fauna_core::data::is_under_review`] is the per-person projection, and it
//! stays the only predicate: a native member list paints in a loop and asks it
//! once per row (tui's `member_review_elements`). This module is that same
//! question asked for a whole participant list in one call, which is what a
//! surface across a serialization boundary needs — web's chip loop lives in
//! TypeScript, where `ActorId(pub [u8; 32])` arrives as a 32-number array, so
//! answering per row over there would mean re-deriving hex encoding *and* the
//! Fauna-rail-vs-null mapping in the view layer. The join belongs here for the
//! reason [`crate::state_json`]'s own doc comment gives about row assembly: a
//! caller that hand-rebuilds it is re-deriving a contract this crate already
//! computes byte-for-byte.

use fauna_core::data::{MemberReview, is_under_review};

use crate::address::TypedAddress;

/// Which participants carry an open review — **index-parallel with
/// `ThreadDetail::participant_displays`**, hence with the
/// `thread-member-chip[i]` those displays render.
///
/// ⚠ **Index-parallel is the whole contract.** The pair renders only on
/// *flagged* members, so a caller that collapsed this to "the flagged ones"
/// would index marks `0..n` over chips `0..m`: `thread-member-unattested-mark[0]`
/// would be the first *flagged* member while `thread-member-chip[0]` is the
/// first member, and the two would silently disagree about who is being asked
/// about. A participant on a rail with no actor id holds its slot as `false`
/// for the same reason [`crate::state_json`] holds it as `null` — dropping it
/// slides every later answer onto the wrong chip, which presents as the *wrong
/// person* rather than as a missing field.
///
/// `roster` is the caller's **cached** open-review list, never a fresh read: a
/// member list paints far more often than the account store changes, and
/// `succession-aftermath.md` § Propagation fixes the two refresh points (behind
/// the aftermath's raise, and after every adjudication).
pub fn member_review_flags(participants: &[TypedAddress], roster: &[MemberReview]) -> Vec<bool> {
    participants
        .iter()
        .map(|addr| {
            addr.person_actor_id()
                .is_some_and(|person| is_under_review(roster, &person))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorId;

    fn person(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    fn fauna(byte: u8) -> TypedAddress {
        TypedAddress::Fauna {
            handle: format!("member-{byte}"),
            actor_id: person(byte),
        }
    }

    fn review(byte: u8) -> MemberReview {
        MemberReview {
            person: person(byte),
            reasons: Vec::new(),
        }
    }

    #[test]
    fn a_flagged_member_is_answered_at_its_own_index() {
        let participants = vec![fauna(1), fauna(2), fauna(3)];
        assert_eq!(
            member_review_flags(&participants, &[review(2)]),
            vec![false, true, false],
            "the flag must land on the chip of the person it names, not on the \
             first chip"
        );
    }

    #[test]
    fn a_non_fauna_participant_holds_its_slot_rather_than_being_dropped() {
        let participants = vec![
            TypedAddress::Email {
                email_address: "someone@example.test".to_string(),
            },
            fauna(7),
        ];
        let flags = member_review_flags(&participants, &[review(7)]);
        assert_eq!(
            flags,
            vec![false, true],
            "a rail with no actor id must keep its index — dropping it slides \
             every later answer onto the wrong chip"
        );
    }

    #[test]
    fn an_empty_roster_flags_nobody() {
        let participants = vec![fauna(1), fauna(2)];
        assert_eq!(
            member_review_flags(&participants, &[]),
            vec![false, false],
            "the ordinary case — no succession has raised anything — must paint \
             no marks at all"
        );
    }

    #[test]
    fn a_roster_entry_nobody_in_this_thread_matches_flags_nobody() {
        // The roster is account-wide (every group of the owner's), while a
        // thread renders one group: most rows are about somebody who is not on
        // this member list, and that must not smear onto whoever is.
        let participants = vec![fauna(1)];
        assert_eq!(
            member_review_flags(&participants, &[review(9)]),
            vec![false],
            "a review about a member of ANOTHER group must not flag this thread's"
        );
    }

    #[test]
    fn no_participants_is_no_flags_rather_than_a_panic() {
        assert!(member_review_flags(&[], &[review(1)]).is_empty());
    }
}
