//! The supervised caller's OWN pending asks — `status.contact_requests` and
//! `status.feed_requests` off `fauna.family.status` — held for the three
//! refused-send surfaces that read them back (`family-safety.md`
//! § Child-initiated contact requests → *Ward transparency*, § Feed-source
//! approvals): the contacts page's Find User result and the profile page
//! (`contact-request-pending`), and the bridge card
//! (`bridge-source-request-state`). The tui twin is `FamilyState::
//! own_contact_requests` / `own_feed_requests` (`apps/fauna-tui/src/family.rs`).
//!
//! **Durable, not local**: this is what makes a pending ask survive navigation
//! and a restart; each surface's own just-asked flag only makes it answer
//! immediately. Filled by every successful status read (the post-auth
//! `FamilyStatusLoaded` and the family page's own load), **gated on
//! `supervised_by`** — a graduated account has no guardian to be waiting on, so
//! a stale ask from an earlier read must not keep painting "asked — waiting".
//! An ask's own re-read replaces the list too, unless it came back empty (a
//! failed re-read degrades to empty, and dropping what we hold on that would be
//! strictly worse than keeping it).
//!
//! The render questions themselves (the id compare, the triple key, the
//! pending/approved split) are shared Rust — `fauna_client_family::ward_asks` —
//! so this module only holds the lists. GTK-main-thread only.

use std::cell::RefCell;
use std::collections::BTreeSet;

use fauna_client_family::FeedRequestState;
use fauna_client_family::family::{FamilyContactRequestInfo, FamilyFeedRequestInfo, ReachPolicy};

#[derive(Default)]
struct Asks {
    contact: Vec<FamilyContactRequestInfo>,
    feed: Vec<FamilyFeedRequestInfo>,
    /// The `(bridge_id, operation, target)` triples this session saw refused
    /// by the guardian gate — the LOCAL half of the feed-source ask surface
    /// (what turns a refusal the user just hit into a visible
    /// `bridge-source-request-button`); tui's `BridgesState::guardian_refused`.
    /// Filled only on the nest's TYPED refusal, so it is supervised-only by
    /// construction.
    refused_feed: BTreeSet<(String, String, String)>,
    /// Whether a `fauna.family.status` read of this run has succeeded — what
    /// separates "no policy: unsupervised" from "nothing known yet" for the
    /// Contacts page's knock and accept predicates (`ui/README.md` § Offer
    /// only what is possible, rule 5; tui's `FamilyState::status_read`).
    status_read: bool,
    /// The caller's own active policy off that read, held only while the read
    /// named a guardian (tui's `FamilyState::own_policy`). What the shared
    /// `fauna_client_family::{knocks_await_guardian, knock_possibility,
    /// accept_possibility, guardian_decides_contact}` answer from.
    own_policy: Option<ReachPolicy>,
}

impl Asks {
    fn own_supervision(&self) -> fauna_client_family::OwnSupervision<'_> {
        if self.status_read {
            fauna_client_family::OwnSupervision::Read(self.own_policy.as_ref())
        } else {
            fauna_client_family::OwnSupervision::Unread
        }
    }
}

thread_local! {
    static ASKS: RefCell<Asks> = RefCell::new(Asks::default());
}

/// Fold one successful `fauna.family.status` read. `supervised` is whether it
/// named a guardian — the gate that drops a graduated account's leftovers;
/// `own_policy` is the reply's own `policy`.
pub fn set_from_status(
    supervised: bool,
    own_policy: Option<&ReachPolicy>,
    contact: Vec<FamilyContactRequestInfo>,
    feed: Vec<FamilyFeedRequestInfo>,
) {
    ASKS.with(|a| {
        let mut a = a.borrow_mut();
        a.status_read = true;
        if supervised {
            a.contact = contact;
            a.feed = feed;
            a.own_policy = own_policy.cloned();
        } else {
            a.contact.clear();
            a.feed.clear();
            a.own_policy = None;
        }
    });
}

/// Whether the knock rows carry `knock-guardian-pending` (`family-safety.md`
/// § App surface — the knock row is one of the blocked surfaces a pending
/// approval renders on).
pub fn knocks_await_guardian() -> bool {
    ASKS.with(|a| fauna_client_family::knocks_await_guardian(a.borrow().own_policy.as_ref()))
}

/// The knock's answer for a resolved peer — what `contacts-add-button` paints
/// from on the Find User result and what the knock asks before it sends
/// (`contacts.md` § Errors & edge cases). `peer_status` is the caller's roster
/// edge to the peer, `None` with none. The shared
/// `fauna_client_family::knock_possibility`.
pub fn knock_possibility(peer_status: Option<&str>) -> fauna_core::possibility::Possibility {
    ASKS.with(|a| fauna_client_family::knock_possibility(a.borrow().own_supervision(), peer_status))
}

/// The accept's answer — what each `contacts-accept-button[i]` paints from and
/// what the accept asks first. The shared `fauna_client_family::accept_possibility`.
pub fn accept_possibility() -> fauna_core::possibility::Possibility {
    ASKS.with(|a| fauna_client_family::accept_possibility(a.borrow().own_supervision()))
}

/// Whether the guardian decides contact with this peer — when
/// `contact-request-guardian-button` is the result's one step, offered before
/// anything was refused. The shared `fauna_client_family::guardian_decides_contact`.
pub fn guardian_decides(peer_status: Option<&str>) -> bool {
    ASKS.with(|a| {
        fauna_client_family::guardian_decides_contact(a.borrow().own_supervision(), peer_status)
    })
}

/// A landed contact ask's re-read. Empty = the re-read failed: keep what we
/// hold (the surface's local flag carries the render).
pub fn replace_contact_requests(requests: Vec<FamilyContactRequestInfo>) {
    if requests.is_empty() {
        return;
    }
    ASKS.with(|a| a.borrow_mut().contact = requests);
}

/// A landed feed-source ask's re-read — same empty-means-failed rule.
pub fn replace_feed_requests(requests: Vec<FamilyFeedRequestInfo>) {
    if requests.is_empty() {
        return;
    }
    ASKS.with(|a| a.borrow_mut().feed = requests);
}

/// Whether an ask for `peer_actor_id_hex` is outstanding (the shared compare).
pub fn contact_ask_pending(peer_actor_id_hex: &str) -> bool {
    ASKS.with(|a| fauna_client_family::contact_ask_pending(&a.borrow().contact, peer_actor_id_hex))
}

/// The live state of the ask for one `(bridge, operation, target)` triple.
pub fn feed_request_state(
    bridge_id: &str,
    operation: &str,
    target: &str,
) -> Option<FeedRequestState> {
    ASKS.with(|a| {
        fauna_client_family::feed_request_state(&a.borrow().feed, bridge_id, operation, target)
    })
}

/// Every live feed-source ask on `bridge_id`, in the nest's order — one
/// `bridge-source-request-state` row each.
pub fn feed_requests_for(bridge_id: &str) -> Vec<FamilyFeedRequestInfo> {
    ASKS.with(|a| {
        a.borrow()
            .feed
            .iter()
            .filter(|r| r.bridge_id == bridge_id)
            .cloned()
            .collect()
    })
}

/// Record a typed guardian refusal of one feed-source operation.
pub fn note_feed_refusal(bridge_id: &str, operation: &str, target: &str) {
    ASKS.with(|a| {
        a.borrow_mut().refused_feed.insert((
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
        ))
    });
}

/// The `(operation, target)` pairs refused on `bridge_id` this session, in a
/// stable order.
pub fn feed_refusals_for(bridge_id: &str) -> Vec<(String, String)> {
    ASKS.with(|a| {
        a.borrow()
            .refused_feed
            .iter()
            .filter(|(b, _, _)| b == bridge_id)
            .map(|(_, op, target)| (op.clone(), target.clone()))
            .collect()
    })
}

/// Drop both lists and the refusal set on an actor change (`crate::actor_scope`'s canonical list):
/// they are one account's asks and refusals, and the incoming account's first
/// status read has not landed yet.
pub fn clear_for_identity_change() {
    ASKS.with(|a| *a.borrow_mut() = Asks::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(byte: u8) -> FamilyContactRequestInfo {
        FamilyContactRequestInfo {
            peer_actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            peer_handle: String::new(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    fn feed(approved: bool) -> FamilyFeedRequestInfo {
        FamilyFeedRequestInfo {
            bridge_id: "activitypub".into(),
            operation: "follow".into(),
            target: "npub1abc".into(),
            label: String::new(),
            created_at: 1,
            approved_at: approved.then_some(2),
            extra: Default::default(),
        }
    }

    /// A graduated account's read drops the leftovers: no guardian to be
    /// waiting on (the client half of the nest's own graduation drop).
    #[test]
    fn an_unsupervised_read_drops_stale_asks() {
        set_from_status(true, None, vec![contact(0xab)], vec![feed(true)]);
        assert!(contact_ask_pending(&"ab".repeat(32)));
        set_from_status(false, None, vec![contact(0xab)], vec![feed(true)]);
        assert!(!contact_ask_pending(&"ab".repeat(32)));
        assert_eq!(
            feed_request_state("activitypub", "follow", "npub1abc"),
            None
        );
    }

    /// An empty re-read is a FAILED re-read, not "no asks": keep what we hold.
    #[test]
    fn an_empty_reread_keeps_the_held_asks() {
        set_from_status(true, None, vec![contact(0xab)], vec![feed(false)]);
        replace_contact_requests(Vec::new());
        replace_feed_requests(Vec::new());
        assert!(contact_ask_pending(&"ab".repeat(32)));
        assert_eq!(
            feed_request_state("activitypub", "follow", "npub1abc"),
            Some(FeedRequestState::Pending)
        );
        clear_for_identity_change();
        assert!(!contact_ask_pending(&"ab".repeat(32)));
    }

    /// Contact approval on the caller's own policy marks every knock; a
    /// graduated read (no guardian) clears the mark whatever policy it carries.
    #[test]
    fn contact_approval_marks_the_knocks_until_graduation() {
        let approval_on = ReachPolicy {
            contact_approval: true,
            ..ReachPolicy::default()
        };
        set_from_status(true, Some(&ReachPolicy::default()), Vec::new(), Vec::new());
        assert!(!knocks_await_guardian());
        set_from_status(true, Some(&approval_on), Vec::new(), Vec::new());
        assert!(knocks_await_guardian());
        set_from_status(false, Some(&approval_on), Vec::new(), Vec::new());
        assert!(!knocks_await_guardian());
        set_from_status(true, Some(&approval_on), Vec::new(), Vec::new());
        clear_for_identity_change();
        assert!(!knocks_await_guardian());
    }

    /// The entry gate (`contacts.md` § Errors & edge cases): unread → the
    /// knock and accept wait as loading; approval on → the knock to a
    /// stranger and every accept are blocked and the ask is the one step; an
    /// accepted peer still passes.
    #[test]
    fn the_entry_gate_follows_the_status_read() {
        use fauna_core::possibility::Possibility;
        clear_for_identity_change();
        assert!(!knock_possibility(None).is_possible());
        assert!(!accept_possibility().is_possible());
        assert!(!guardian_decides(None), "no ask on a guess");

        let approval_on = ReachPolicy {
            contact_approval: true,
            ..ReachPolicy::default()
        };
        set_from_status(true, Some(&approval_on), Vec::new(), Vec::new());
        assert_eq!(
            knock_possibility(None),
            Possibility::blocked(fauna_core::localized::LocalizedText::key(
                "contacts.guardian_approval_required"
            ))
        );
        assert!(!accept_possibility().is_possible());
        assert!(guardian_decides(None));
        assert_eq!(knock_possibility(Some("accepted")), Possibility::Possible);
        assert!(!guardian_decides(Some("accepted")));

        set_from_status(false, Some(&approval_on), Vec::new(), Vec::new());
        assert_eq!(knock_possibility(None), Possibility::Possible);
        assert_eq!(accept_possibility(), Possibility::Possible);
        clear_for_identity_change();
    }
}
