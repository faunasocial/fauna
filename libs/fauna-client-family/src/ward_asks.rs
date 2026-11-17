//! The supervised ward's OWN asks, read back for the refused-send surfaces
//! (`family-safety.md` § Child-initiated contact requests → *Ward
//! transparency*, § Feed-source approvals). Both lists ride the one
//! `fauna.family.status` reply (`contact_requests` / `feed_requests`); the
//! functions here answer the two questions every app's render asks of them —
//! "is an ask for this peer outstanding?" (`contact-request-pending`) and "what
//! state is the ask for this feed-source operation in?"
//! (`bridge-source-request-state`). Lifted from tui's `FamilyState` so no app
//! re-derives the id compare or the triple key.
//!
//! A third question rides the same reply: "does every knock waiting on me wait
//! on my guardian?" (`knock-guardian-pending`, § App surface — "pending approval
//! states render on the blocked surfaces themselves (the knock row …)").
//!
//! None of the functions tests supervision: every input is supervised-only by
//! construction (the caller folds it from a status read gated on
//! `supervised_by`), so deriving the answer from the data alone keeps the
//! surfaces in step with what the nest enforces.

use fauna_core::data::ContactStatus;
use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
use fauna_protocol::family::{FamilyContactRequestInfo, FamilyFeedRequestInfo, ReachPolicy};

/// The two live states a ward's feed-source ask can be in, as
/// `bridge-source-request-state` renders them (`family-safety.md` § Feed-source
/// approvals).
///
/// There is deliberately no `Lapsed`/`Expired` member: the nest lists only live
/// rows, so a dead ask is an ABSENT row, not a third state. Modelling one would
/// invite a surface that says "expired" while the gate has already gone back to
/// refusing outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedRequestState {
    /// Asked, no verdict yet.
    Pending,
    /// Approved — a single-use grant the ward redeems by RETRYING the original
    /// operation. A surface prompts the retry; it never retries on its own
    /// (§ Feed-source approvals: "an approval is a grant the ward redeems by
    /// retrying"). Auto-retrying would spend the grant on a navigation the user
    /// did not ask for, and a lapsed grant would then look like a silent
    /// failure.
    Approved,
}

impl FeedRequestState {
    /// The state one listed ask is in. `approved_at` carries the approval
    /// INSTANT rather than a state string, so the two states are exactly "has
    /// one" / "does not".
    pub fn of(ask: &FamilyFeedRequestInfo) -> Self {
        if ask.approved_at.is_some() {
            FeedRequestState::Approved
        } else {
            FeedRequestState::Pending
        }
    }
}

/// Whether `asks` holds an outstanding contact ask for `peer_actor_id_hex` —
/// what `contact-request-pending` renders from.
///
/// Compares the DECODED caller id against the wire bytes rather than encoding
/// each row, so the compare is case-insensitive (the caller's id is whatever a
/// Find User form resolved; the wire's is canonical bytes) and a caller id that
/// is not 32-byte hex matches nothing — the honest answer, needing no separate
/// guard.
pub fn contact_ask_pending(asks: &[FamilyContactRequestInfo], peer_actor_id_hex: &str) -> bool {
    let Ok(want) = fauna_core::hex32::decode(peer_actor_id_hex) else {
        return false;
    };
    asks.iter().any(|ask| ask.peer_actor_id.as_slice() == want)
}

/// The live ask state for one feed-source operation, or `None` when `asks`
/// holds no live ask for it — what `bridge-source-request-state` renders, and
/// what decides whether `bridge-source-request-button` is still offered.
///
/// Keyed on the whole `(bridge_id, operation, target)` triple because that is
/// what the grant is scoped to: a `follow` ask for one account must not light up
/// the row of a different follow on the same bridge, and a `link` ask (whose
/// `target` is empty by construction) must not match either.
pub fn feed_request_state(
    asks: &[FamilyFeedRequestInfo],
    bridge_id: &str,
    operation: &str,
    target: &str,
) -> Option<FeedRequestState> {
    asks.iter()
        .find(|r| r.bridge_id == bridge_id && r.operation == operation && r.target == target)
        .map(FeedRequestState::of)
}

/// Whether every knock in the caller's own list is waiting on their guardian —
/// what `knock-guardian-pending` renders from, on each knock row.
///
/// `own_policy` is the caller's OWN active policy off `fauna.family.status`
/// (`FamilyStatusReply::policy`), present only while the caller is supervised.
/// With `contact_approval` on, acceptance authority for every inbound knock has
/// moved to the guardian: the ward's own accept is refused approval-pending and
/// the knock sits in the guardian's queue (`family-safety.md` § Guardian policy,
/// the `contact_approval` row; § Reach approvals — "the ward still *sees* the
/// knock with an approval-pending state"). So the state is derived here from
/// what the ward already holds, with no per-knock wire field: `KnockItem`
/// carries none, and none is needed while the knob decides for all knocks at
/// once.
pub fn knocks_await_guardian(own_policy: Option<&ReachPolicy>) -> bool {
    own_policy.is_some_and(|p| p.contact_approval)
}

/// What the caller knows of its OWN supervision, as the two Contacts-page
/// predicates below need it: whether this run's `fauna.family.status` read has
/// succeeded yet, and the policy it carried.
///
/// Two states, not an `Option<ReachPolicy>`, because "no policy" means two
/// different things: an unsupervised account (the read came back without one —
/// the account decides its own contacts) and a read that has not resolved yet
/// (nothing is known). `ui/README.md` § Offer only what is possible, rule 5:
/// unknown is not possible, so the second must never read as the first.
#[derive(Debug, Clone, Copy)]
pub enum OwnSupervision<'a> {
    /// No `fauna.family.status` read of this run has succeeded yet.
    Unread,
    /// The latest successful read: the caller's own active policy, present
    /// only while the caller is supervised.
    Read(Option<&'a ReachPolicy>),
}

/// The reason a ward's knock is not offered while its guardian decides new
/// contacts — the same words the nest's typed refusal is painted with, so the
/// entry and a refusal that slips through (rule 7) say one thing.
const GUARDIAN_APPROVAL_REQUIRED: &str = "contacts.guardian_approval_required";
/// The reason a ward's accept is not offered while its guardian decides
/// knocks — the words `knock-guardian-pending` already paints on the row.
const KNOCK_GUARDIAN_PENDING: &str = "contacts.knock_guardian_pending";
/// The reason either affordance waits while the status read is unresolved.
const LOADING: &str = "common.loading";

fn approval_required(own: OwnSupervision<'_>) -> Option<bool> {
    match own {
        OwnSupervision::Unread => None,
        OwnSupervision::Read(policy) => Some(knocks_await_guardian(policy)),
    }
}

/// Whether the caller may knock (send a contact request to) a resolved peer
/// now — what `contacts-add-button` paints from, and what the send asks first
/// (`ui/README.md` § Offer only what is possible, rules 4–6;
/// `family-safety.md` § Guardian policy, the `contact_approval` row, outbound
/// arm; `contacts.md` § Errors & edge cases).
///
/// `peer_status` is the caller's contact edge to that peer, as the roster
/// carries it (`ContactItem::status`), `None` with no edge. The answer mirrors
/// the nest's outbound reach gate: with `contact_approval` on, a send to a peer
/// that is not already `accepted`/`confirmed` is refused with the typed
/// guardian-approval error, so the knock is blocked with that reason and the
/// page offers the ask (`contact-request-guardian-button`) as its one step.
/// An existing accepted/confirmed edge passes, as it does at the nest.
///
/// Blocked, never absent: the ward can make the precondition true from this
/// app — by asking, and the guardian's approval makes the peer a contact.
pub fn knock_possibility(own: OwnSupervision<'_>, peer_status: Option<&str>) -> Possibility {
    let already_contact = matches!(
        peer_status.and_then(ContactStatus::from_wire),
        Some(ContactStatus::Accepted | ContactStatus::Confirmed)
    );
    if already_contact {
        return Possibility::Possible;
    }
    match approval_required(own) {
        None => Possibility::blocked(LocalizedText::key(LOADING)),
        Some(true) => Possibility::blocked(LocalizedText::key(GUARDIAN_APPROVAL_REQUIRED)),
        Some(false) => Possibility::Possible,
    }
}

/// Whether the guardian decides contact with this peer: the caller's policy
/// has `contact_approval` on and the peer is not already an
/// `accepted`/`confirmed` contact. That is when the ask
/// (`contact-request-guardian-button`) is the Find User result's one step,
/// offered at the entry before anything was refused (`ui/README.md` § Offer
/// only what is possible, rule 3; `contacts.md` § Errors & edge cases). False
/// while the status read is unresolved: the knock then waits as loading, and
/// no ask is offered on a guess.
pub fn guardian_decides_contact(own: OwnSupervision<'_>, peer_status: Option<&str>) -> bool {
    approval_required(own) == Some(true) && !knock_possibility(own, peer_status).is_possible()
}

/// Whether the caller may accept an incoming knock now — what each
/// `contacts-accept-button[i]` paints from, and what the accept asks first.
///
/// With `contact_approval` on, acceptance authority for every inbound knock
/// is the guardian's and the nest refuses the ward's own accept
/// (`family-safety.md` § Guardian policy, the `contact_approval` row, inbound
/// arm), so the accept is blocked with the words the row already shows
/// (`knock-guardian-pending`). Block and dismiss stay the ward's.
pub fn accept_possibility(own: OwnSupervision<'_>) -> Possibility {
    match approval_required(own) {
        None => Possibility::blocked(LocalizedText::key(LOADING)),
        Some(true) => Possibility::blocked(LocalizedText::key(KNOCK_GUARDIAN_PENDING)),
        Some(false) => Possibility::Possible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;

    fn contact_ask(byte: u8) -> FamilyContactRequestInfo {
        FamilyContactRequestInfo {
            peer_actor_id: ByteBuf::from(vec![byte; 32]),
            peer_handle: String::new(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    fn feed_ask(bridge: &str, op: &str, target: &str, approved: bool) -> FamilyFeedRequestInfo {
        FamilyFeedRequestInfo {
            bridge_id: bridge.to_string(),
            operation: op.to_string(),
            target: target.to_string(),
            label: String::new(),
            created_at: 1,
            approved_at: approved.then_some(2),
            extra: Default::default(),
        }
    }

    #[test]
    fn a_contact_ask_matches_its_own_peer_in_either_hex_case() {
        let asks = vec![contact_ask(0xab)];
        assert!(contact_ask_pending(&asks, &"ab".repeat(32)));
        assert!(contact_ask_pending(&asks, &"AB".repeat(32)));
        assert!(
            !contact_ask_pending(&asks, &"cd".repeat(32)),
            "another peer's ask is not this one's"
        );
    }

    #[test]
    fn a_malformed_peer_id_matches_nothing() {
        let asks = vec![contact_ask(0xab)];
        assert!(!contact_ask_pending(&asks, "not-hex"));
        assert!(!contact_ask_pending(&asks, ""));
    }

    #[test]
    fn the_feed_ask_state_is_keyed_on_the_whole_triple() {
        let asks = vec![feed_ask("activitypub", "follow", "npub1abc", true)];
        assert_eq!(
            feed_request_state(&asks, "activitypub", "follow", "npub1abc"),
            Some(FeedRequestState::Approved)
        );
        assert_eq!(
            feed_request_state(&asks, "activitypub", "follow", "npub1other"),
            None,
            "one follow's grant must not light up another follow's row"
        );
        assert_eq!(
            feed_request_state(&asks, "activitypub", "link", ""),
            None,
            "a follow grant must not satisfy the bridge's LINK gate"
        );
    }

    #[test]
    fn knocks_await_the_guardian_only_under_contact_approval() {
        assert!(
            !knocks_await_guardian(None),
            "an unsupervised caller carries no policy, and decides its own knocks"
        );
        assert!(
            !knocks_await_guardian(Some(&ReachPolicy::default())),
            "a supervised caller with contact approval off decides its own knocks"
        );
        let approval_on = ReachPolicy {
            contact_approval: true,
            ..ReachPolicy::default()
        };
        assert!(knocks_await_guardian(Some(&approval_on)));
    }

    fn approval_on() -> ReachPolicy {
        ReachPolicy {
            contact_approval: true,
            ..ReachPolicy::default()
        }
    }

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    #[test]
    fn a_knock_waits_for_the_status_read() {
        assert_eq!(
            knock_possibility(OwnSupervision::Unread, None),
            blocked(LOADING),
            "unknown is not possible: the knock is never enabled on a guess"
        );
    }

    #[test]
    fn an_unsupervised_or_free_caller_may_knock() {
        assert_eq!(
            knock_possibility(OwnSupervision::Read(None), None),
            Possibility::Possible
        );
        let off = ReachPolicy::default();
        assert_eq!(
            knock_possibility(OwnSupervision::Read(Some(&off)), None),
            Possibility::Possible
        );
    }

    #[test]
    fn under_contact_approval_a_knock_to_a_stranger_is_the_guardians() {
        let on = approval_on();
        for status in [None, Some("pending"), Some("blocked")] {
            assert_eq!(
                knock_possibility(OwnSupervision::Read(Some(&on)), status),
                blocked(GUARDIAN_APPROVAL_REQUIRED),
                "peer status {status:?}"
            );
        }
    }

    #[test]
    fn an_accepted_or_confirmed_peer_passes_as_at_the_nest() {
        let on = approval_on();
        for status in ["accepted", "confirmed"] {
            assert_eq!(
                knock_possibility(OwnSupervision::Read(Some(&on)), Some(status)),
                Possibility::Possible
            );
            assert_eq!(
                knock_possibility(OwnSupervision::Unread, Some(status)),
                Possibility::Possible,
                "an existing edge passes the nest's gate whatever the policy"
            );
        }
    }

    #[test]
    fn the_guardian_decides_a_stranger_only_once_the_read_says_so() {
        let on = approval_on();
        let off = ReachPolicy::default();
        assert!(guardian_decides_contact(
            OwnSupervision::Read(Some(&on)),
            None
        ));
        assert!(guardian_decides_contact(
            OwnSupervision::Read(Some(&on)),
            Some("pending")
        ));
        assert!(!guardian_decides_contact(
            OwnSupervision::Read(Some(&on)),
            Some("accepted")
        ));
        assert!(!guardian_decides_contact(
            OwnSupervision::Read(Some(&off)),
            None
        ));
        assert!(!guardian_decides_contact(OwnSupervision::Read(None), None));
        assert!(
            !guardian_decides_contact(OwnSupervision::Unread, None),
            "no ask is offered on a guess"
        );
    }

    #[test]
    fn the_accept_answers_per_supervision_state() {
        assert_eq!(accept_possibility(OwnSupervision::Unread), blocked(LOADING));
        assert_eq!(
            accept_possibility(OwnSupervision::Read(None)),
            Possibility::Possible
        );
        let off = ReachPolicy::default();
        assert_eq!(
            accept_possibility(OwnSupervision::Read(Some(&off))),
            Possibility::Possible
        );
        let on = approval_on();
        assert_eq!(
            accept_possibility(OwnSupervision::Read(Some(&on))),
            blocked(KNOCK_GUARDIAN_PENDING)
        );
    }

    #[test]
    fn an_unapproved_feed_ask_is_pending() {
        let asks = vec![feed_ask("activitypub", "link", "", false)];
        assert_eq!(
            feed_request_state(&asks, "activitypub", "link", ""),
            Some(FeedRequestState::Pending)
        );
    }
}
