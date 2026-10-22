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

    #[test]
    fn an_unapproved_feed_ask_is_pending() {
        let asks = vec![feed_ask("activitypub", "link", "", false)];
        assert_eq!(
            feed_request_state(&asks, "activitypub", "link", ""),
            Some(FeedRequestState::Pending)
        );
    }
}
