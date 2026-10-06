use crate::address::Rail;
use crate::thread::ThreadFlavor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DeliveryMode {
    Realtime,
    Async,
}

/// Per-thread transport-crypto level — the **render of the room's class**
/// (`conversation-rooms.md` § The three classes; [`crate::room::RoomClass`]).
/// Distinct from `fauna_core::EncryptionMode` (the nest *storage* mode chosen
/// at onboarding) — kept under a separate name so the two don't collide when
/// both UniFFI namespaces are concatenated into one Swift module
/// (`FaunaFFISwift`).
///
/// One variant per class, and nothing else: the `None` arm the ActivityPub
/// stub carried as a per-rail constant — a claim no roster produces — retired
/// with that stub (`conversations.md` § Where logic lives → *The `Bridged`
/// adapter*, ruling 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ThreadEncryption {
    /// End-to-end: the members read, nests relay opaque bytes.
    E2E,
    /// Transport-only: a bridge or mail transfer agent reads it by
    /// construction.
    TransportOnly,
    /// Community: the members and the room's home nest, which reads under
    /// its room-read key (`conversation-rooms.md` § The three classes →
    /// *Community*).
    NestReadable,
}

/// `#[serde(default)]` helper for the role-gated capability fields, which
/// default **open** wherever no policy governs.
fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ThreadCapabilities {
    pub supports_attachments: bool,
    pub supports_markdown: bool,
    pub supports_reactions: bool,
    pub supports_message_delete: bool,
    pub supports_per_message_reply: bool,
    pub supports_membership_change: bool,
    /// Whether replies carry an editable **recipient set** — the To/Cc of the
    /// message about to be sent, rendered as the always-visible "To" line in
    /// the compose bar (`conversations.md` § Participants vs reply recipients).
    /// Mail only: each reply picks its recipients (reply vs reply-all, then
    /// editable). On FaunaMls/social rails the recipients ARE the thread
    /// membership, so there's nothing per-reply to choose — `false`, and the
    /// To line is hidden. Distinct from `supports_membership_change` (whether
    /// the *thread's* historical participant set is mutable).
    pub supports_recipient_selection: bool,
    /// Rename is offered (an MLS group). On a **governed** room — one carrying
    /// a room policy — it is additionally the viewer's role that decides:
    /// owner and admin only ([`crate::room::RoomSnapshot::gate`]).
    pub supports_rename: bool,
    pub supports_subject: bool,
    pub delivery_mode: DeliveryMode,
    pub encryption: ThreadEncryption,
    /// The **role-gated** half (`conversation-rooms.md` § Roles and
    /// authorization), overlaid by [`crate::room::RoomSnapshot::gate`] from
    /// the viewer's effective role on a governed room and left **open**
    /// wherever no policy governs. Apps grey `thread-add-participant-button`
    /// on `!can_invite` and the member chip's remove on
    /// `!can_remove_members`, and never branch on a role themselves
    /// (`conversations.md` § Architectural rules 5). Default-valued across
    /// UniFFI so an app-side constructor written before these fields existed
    /// still builds.
    #[cfg_attr(feature = "uniffi", uniffi(default = true))]
    #[serde(default = "default_true")]
    pub can_invite: bool,
    #[cfg_attr(feature = "uniffi", uniffi(default = true))]
    #[serde(default = "default_true")]
    pub can_remove_members: bool,
    /// The policy editor (join rule, history policy): owner/admin of a
    /// governed room; `false` wherever there is no policy to edit.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub can_set_policy: bool,
    /// Appoint / demote admins: the owner of a governed room only.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub can_appoint_admins: bool,
    /// Hand the room to another member (`conversation-rooms.md` § Roles and
    /// authorization → *Ownership transfer*): the owner of a governed room
    /// only. Its own field rather than a reading of `can_appoint_admins`
    /// because the table lists the two rows separately and a nest-enforced
    /// class may answer them differently.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub can_transfer_ownership: bool,
    /// Walk out of this room — the roles table's *leave (remove self)* row
    /// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
    /// mechanism*): admin and member of a governed room, and any member of a
    /// policy-less one; **never the owner**, who transfers ownership first because
    /// a room is never owner-less. Its own field rather than a reading of
    /// `can_remove_members`, which answers the opposite question — an
    /// ordinary member may leave and may not remove.
    ///
    /// `false` on a 1:1: leaving the only other person in a DM is the
    /// ordinary thread delete, not a membership act
    /// ([`crate::manager::ConversationsManager::remove_participant`] draws the
    /// same line).
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub can_leave_room: bool,
}

/// Is this `(rail, flavor)` a **bound FaunaMls group** — the one pairing whose
/// add-participant gesture reaches the wire?
///
/// Adding to a bound MLS group opens the add commit by fetching the newcomer's
/// key package (`fauna.conversations.keypackage.fetch`), so the gesture needs a
/// nest. Every other pairing is snapshot-only: a FaunaMls 1:1 **forks** a fresh
/// `MlsGroup` thread whose first *send* bootstraps it, and a non-FaunaMls rail
/// has no wire membership op at all.
///
/// One expression, two readers — [`crate::compose::AddParticipantState::
/// in_place_mls_group`] (what the paint gates on, and what every app inherits
/// through the snapshot) and `ConversationsManager::confirm_add_participant`
/// (the authority for the wire op). Splitting them is how the offline gate and
/// the commit would come to disagree.
///
/// ⚠ **Not the fork-vs-mutate test, which asks a different question and has a
/// different answer.** `add_participant_inner` forks only on `(FaunaMls,
/// OneToOne)` and mutates the thread in place for everything else — so an SMTP
/// 1:1 *does* add "in place" there while answering `false` here. The name says
/// `mls_group` rather than `in_place` for exactly that reason: what the gate
/// needs to know is whether a **key package fetch** happens, not whether a new
/// thread id appears.
pub fn is_in_place_mls_group(rail: Rail, flavor: ThreadFlavor) -> bool {
    matches!((rail, flavor), (Rail::FaunaMls, ThreadFlavor::MlsGroup))
}

/// The capability vector a bridge **declared** (`conversations.md` § Where
/// logic lives → *The `Bridged` adapter*, ruling 2 (b)) — the manifest's
/// `capabilities` map read member by member onto the bridged rail's
/// most-restrictive answer. `flag` answers a declared boolean member by the
/// record's own field name, `delivery_mode` is the declared spelling.
///
/// A member the bridge did not declare, a name this build does not know, and a
/// `delivery_mode` it cannot read all stay withheld: a client older than the
/// nest must not offer an affordance it could not confirm. `encryption` is not
/// a member a bridge declares — the room's class decides it — so it is the
/// rail's answer here whatever the map carries.
pub fn declared_bridge_capabilities(
    flag: impl Fn(&str) -> Option<bool>,
    delivery_mode: Option<&str>,
) -> ThreadCapabilities {
    let base = derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne);
    let on = |name: &str, withheld: bool| flag(name).unwrap_or(withheld);
    ThreadCapabilities {
        supports_attachments: on("supports_attachments", base.supports_attachments),
        supports_markdown: on("supports_markdown", base.supports_markdown),
        supports_reactions: on("supports_reactions", base.supports_reactions),
        supports_message_delete: on("supports_message_delete", base.supports_message_delete),
        supports_per_message_reply: on(
            "supports_per_message_reply",
            base.supports_per_message_reply,
        ),
        supports_membership_change: on(
            "supports_membership_change",
            base.supports_membership_change,
        ),
        supports_recipient_selection: on(
            "supports_recipient_selection",
            base.supports_recipient_selection,
        ),
        supports_rename: on("supports_rename", base.supports_rename),
        supports_subject: on("supports_subject", base.supports_subject),
        delivery_mode: match delivery_mode {
            Some("Realtime") => DeliveryMode::Realtime,
            _ => base.delivery_mode,
        },
        encryption: base.encryption,
        can_invite: on("can_invite", base.can_invite),
        can_remove_members: on("can_remove_members", base.can_remove_members),
        can_set_policy: on("can_set_policy", base.can_set_policy),
        can_appoint_admins: on("can_appoint_admins", base.can_appoint_admins),
        can_transfer_ownership: on("can_transfer_ownership", base.can_transfer_ownership),
        can_leave_room: on("can_leave_room", base.can_leave_room),
    }
}

pub fn derive_capabilities(rail: Rail, flavor: ThreadFlavor) -> ThreadCapabilities {
    use Rail::*;
    use ThreadFlavor::*;
    match (rail, flavor) {
        (FaunaMls, OneToOne) => ThreadCapabilities {
            supports_attachments: true,
            supports_markdown: true,
            supports_reactions: true,
            supports_message_delete: true,
            supports_per_message_reply: true,
            // The "+ add participant" affordance is exposed on 1:1 threads.
            // For FaunaMls the implementation forks into a fresh MLS group
            // thread (the original 1:1 stays intact) — see
            // ConversationsManager::add_participant; this fork is an MLS
            // necessity, not a general rule. Another rail declaring
            // supports_membership_change (a bridge's vector) adds in place.
            supports_membership_change: true,
            // Recipients ARE the MLS group membership — nothing per-reply to pick.
            supports_recipient_selection: false,
            supports_rename: false,
            supports_subject: true,
            delivery_mode: DeliveryMode::Realtime,
            encryption: ThreadEncryption::E2E,
            // Role gating is overlaid per room at read time
            // (`crate::room::RoomSnapshot::gate`); the rail answer is open.
            can_invite: true,
            can_remove_members: true,
            can_set_policy: false,
            can_appoint_admins: false,
            can_transfer_ownership: false,
            can_leave_room: false,
        },
        (FaunaMls, MlsGroup) => ThreadCapabilities {
            supports_membership_change: true,
            supports_rename: true,
            // The one shape with a leave door: a room you can walk out of.
            // Role gating is overlaid per room at read time
            // (`crate::room::RoomSnapshot::gate`) — the rail answer is open.
            can_leave_room: true,
            ..derive_capabilities(FaunaMls, OneToOne)
        },
        // A flavor a newer build wrote and this one does not name: shown, and
        // given the most restrictive answer — every affordance withheld, no
        // membership, role or policy door (`transport.md` § Schema and
        // forward-compat discipline → *Rule 3 in full*: an unknown arm never
        // grants). The rail's transport facts stand.
        (FaunaMls, Unknown { .. }) => ThreadCapabilities {
            supports_attachments: false,
            supports_markdown: false,
            supports_reactions: false,
            supports_message_delete: false,
            supports_per_message_reply: false,
            supports_membership_change: false,
            supports_recipient_selection: false,
            supports_rename: false,
            supports_subject: false,
            can_invite: false,
            can_remove_members: false,
            ..derive_capabilities(FaunaMls, OneToOne)
        },
        (FaunaMls, SubjectKeyed) => ThreadCapabilities {
            // Fauna native doesn't really key on subject — but if a thread
            // somehow ends up subject-keyed, treat as 1:1-shaped.
            ..derive_capabilities(FaunaMls, OneToOne)
        },
        (Smtp, _) => ThreadCapabilities {
            supports_attachments: true,
            // Email uses markdown as its interchange format both ways
            // (`docs/goal/behavior/html-mail.md`): inbound HTML is converted to
            // markdown and rendered through the shared markdown path, outbound
            // markdown is serialized to multipart/alternative. This `true`
            // un-gates the existing markdown compose toolbar (`markdown-bold-button`
            // etc.) for the mail rail on every app at once — each app's
            // compose bar gates the toolbar on this shared capability.
            supports_markdown: true,
            supports_reactions: false,
            supports_message_delete: false,
            supports_per_message_reply: true,
            // Mail has no mutable thread *membership*: a thread's participants are
            // the historical From/To/Cc set — you can't un-send who an email went
            // to, and clicking a participant to "remove" them was an incoherent
            // destructive action (conversations.md § Participants vs reply
            // recipients). Recipient choice happens per-reply (the editable To
            // line), not by mutating thread membership.
            supports_membership_change: false,
            // Each reply picks its recipients (reply vs reply-all, then
            // editable) — the editable "To" line. Mail-only.
            supports_recipient_selection: true,
            supports_rename: false,
            supports_subject: true,
            delivery_mode: DeliveryMode::Async,
            encryption: ThreadEncryption::TransportOnly,
            // Role gating is overlaid per room at read time
            // (`crate::room::RoomSnapshot::gate`); the rail answer is open.
            can_invite: true,
            can_remove_members: true,
            can_set_policy: false,
            can_appoint_admins: false,
            can_transfer_ownership: false,
            can_leave_room: false,
        },
        // Every bridge, before its declared vector is overlaid: the most
        // restrictive answer — every affordance withheld, no membership, role
        // or policy door, `Async` — because the per-bridge vector lives in the
        // bridge's manifest, not in this match (`conversations.md` § Where
        // logic lives → *The `Bridged` adapter*, ruling 2 (b)).
        // `BridgedBackend::capabilities` replaces it with the declared vector
        // for the thread's bridge; this answer stands only for a thread whose
        // bridge is not in the registry. `encryption` is the room's derived
        // class, overlaid at read time like every rail's
        // (`crate::room::RoomSnapshot::gate`).
        (Bridged, _) => ThreadCapabilities {
            supports_attachments: false,
            supports_markdown: false,
            supports_reactions: false,
            supports_message_delete: false,
            supports_per_message_reply: false,
            supports_membership_change: false,
            supports_recipient_selection: false,
            supports_rename: false,
            supports_subject: false,
            delivery_mode: DeliveryMode::Async,
            encryption: ThreadEncryption::TransportOnly,
            can_invite: false,
            can_remove_members: false,
            can_set_policy: false,
            can_appoint_admins: false,
            can_transfer_ownership: false,
            can_leave_room: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Rail;
    use crate::thread::ThreadFlavor;

    #[test]
    fn fauna_oneonone_enables_everything_except_rename() {
        let caps = derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne);
        assert!(caps.supports_attachments);
        assert!(caps.supports_markdown);
        assert!(caps.supports_reactions);
        assert!(caps.supports_per_message_reply);
        // Affordance is shown; implementation forks into MLS group on use.
        assert!(caps.supports_membership_change);
        // Recipients are the group membership — no editable To line.
        assert!(!caps.supports_recipient_selection);
        assert!(!caps.supports_rename);
        assert!(caps.supports_subject);
        assert_eq!(caps.delivery_mode, DeliveryMode::Realtime);
        assert_eq!(caps.encryption, ThreadEncryption::E2E);
    }

    #[test]
    fn fauna_mls_group_enables_everything() {
        let caps = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);
        assert!(caps.supports_membership_change);
        assert!(caps.supports_rename);
    }

    #[test]
    fn smtp_enables_markdown_disables_reactions_and_rename() {
        let caps = derive_capabilities(Rail::Smtp, ThreadFlavor::SubjectKeyed);
        assert!(caps.supports_attachments);
        // Mail composes + renders markdown (html-mail.md) — the toolbar is on.
        assert!(caps.supports_markdown);
        assert!(!caps.supports_reactions);
        // Mail has no mutable thread membership — participants are historical
        // (From/To/Cc); recipient choice is per-reply, not membership mutation.
        assert!(!caps.supports_membership_change);
        // Mail is the one rail with an editable per-reply recipient set.
        assert!(caps.supports_recipient_selection);
        assert!(!caps.supports_rename);
        assert!(caps.supports_subject);
        assert_eq!(caps.delivery_mode, DeliveryMode::Async);
    }

    /// A bridge's rail answer is the floor its declared vector is overlaid on:
    /// nothing offered, nothing a role could open, on every flavor.
    #[test]
    fn bridged_withholds_every_affordance_until_its_vector_is_overlaid() {
        for flavor in [
            ThreadFlavor::OneToOne,
            ThreadFlavor::MlsGroup,
            ThreadFlavor::SubjectKeyed,
        ] {
            let caps = derive_capabilities(Rail::Bridged, flavor.clone());
            assert!(!caps.supports_attachments, "{flavor:?}");
            assert!(!caps.supports_markdown);
            assert!(!caps.supports_reactions);
            assert!(!caps.supports_message_delete);
            assert!(!caps.supports_per_message_reply);
            assert!(!caps.supports_membership_change);
            assert!(!caps.supports_recipient_selection);
            assert!(!caps.supports_rename);
            assert!(!caps.supports_subject);
            assert!(!caps.can_invite);
            assert!(!caps.can_remove_members);
            assert!(!caps.can_set_policy);
            assert!(!caps.can_appoint_admins);
            assert!(!caps.can_transfer_ownership);
            assert!(!caps.can_leave_room);
            assert_eq!(caps.delivery_mode, DeliveryMode::Async);
        }
    }

    #[test]
    fn fauna_supports_message_delete_others_dont() {
        assert!(
            derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne).supports_message_delete
        );
        assert!(
            derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup).supports_message_delete
        );
        assert!(
            !derive_capabilities(Rail::Smtp, ThreadFlavor::SubjectKeyed).supports_message_delete
        );
        assert!(
            !derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne).supports_message_delete
        );
    }

    /// The offline gate's discriminant, over the WHOLE matrix rather than a
    /// hand-picked pair: exactly one of the nine `(rail, flavor)` pairings
    /// reaches the wire, so a new rail or flavor cannot quietly acquire — or
    /// lose — a key-package fetch without this test moving. A hand-listed
    /// "check FaunaMls/MlsGroup is true and SMTP is false" would have passed
    /// on every wrong widening of the predicate.
    #[test]
    fn only_a_bound_mls_group_adds_a_participant_over_the_wire() {
        let rails = [Rail::FaunaMls, Rail::Smtp, Rail::Bridged];
        let flavors = [
            ThreadFlavor::OneToOne,
            ThreadFlavor::MlsGroup,
            ThreadFlavor::SubjectKeyed,
        ];
        let mut reaching = Vec::new();
        for rail in rails {
            for flavor in flavors.clone() {
                if is_in_place_mls_group(rail, flavor.clone()) {
                    reaching.push((rail, flavor));
                }
            }
        }
        assert_eq!(
            reaching,
            vec![(Rail::FaunaMls, ThreadFlavor::MlsGroup)],
            "exactly one pairing may gate add-participant on \
             fauna.conversations.keypackage.fetch; anything wider greys a \
             gesture that works offline (account-data-plane.md § The \
             offline-mutation contract, ruling on over-claiming)"
        );
    }

    /// ⚠ The predicate this one is NOT. `add_participant_inner` forks only on
    /// `(FaunaMls, OneToOne)` and mutates in place for all eight other
    /// pairings, so "adds in place" there is TRUE for an SMTP 1:1 while the
    /// gate discriminant is false. Pinned because the two read alike in prose
    /// and conflating them is what would gate SMTP's offline-capable add.
    #[test]
    fn the_gate_discriminant_is_not_the_fork_discriminant() {
        let forks =
            |rail, flavor| matches!((rail, flavor), (Rail::FaunaMls, ThreadFlavor::OneToOne));
        // SMTP 1:1: mutates the thread in place, and issues nothing.
        assert!(!forks(Rail::Smtp, ThreadFlavor::OneToOne));
        assert!(!is_in_place_mls_group(Rail::Smtp, ThreadFlavor::OneToOne));
        // FaunaMls 1:1: forks a new group, and issues nothing (the fork's
        // first *send* bootstraps it).
        assert!(forks(Rail::FaunaMls, ThreadFlavor::OneToOne));
        assert!(!is_in_place_mls_group(
            Rail::FaunaMls,
            ThreadFlavor::OneToOne
        ));
    }
}
