//! The possibility answer of every thread and room control gated by the
//! viewer's role or by the rail (`docs/goal/ui/README.md` § Offer only what is
//! possible, rules 2 and 6; the page's ruling is `docs/goal/ui/conversations.md`
//! § Errors & edge cases → *Controls this person can never use*).
//!
//! [`ThreadCapabilities`] keeps its booleans — the room's gate
//! ([`crate::room::RoomSnapshot::gate`]) still writes them, and the manager's
//! own refusals still read them. What a bare `false` cannot say is *why*, and
//! the why decides the paint: a control this person can never use here is not
//! shown at all, while one they can unblock from this app is shown disabled
//! with its reason. [`ThreadCapabilities::possibility`] is that one answer, so
//! no app tells the two apart for itself.
//!
//! Only one control here is ever *blocked* rather than absent: the owner's
//! walk-out. A room is never owner-less, so the owner hands it over first, and
//! that hand-over is in the same editor (`room-owner-transfer-button[i]`).
//! Every other `false` names a role the viewer does not hold — the owner or an
//! admin grants it, never the viewer — or a rail that has no such act, and
//! neither is something this person can make true from here.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
use serde::{Deserialize, Serialize};

use crate::capabilities::ThreadCapabilities;
use crate::room::RoomRole;

/// The i18n key of the owner's blocked walk-out — the same sentence the
/// manager's `leave_room` refuses with, so the paint and the refusal cannot
/// disagree (rule 6).
pub const OWNER_LEAVE_REASON_KEY: &str = "error.send.room_owner_cannot_leave";

/// A thread or room control whose possibility follows the viewer's role or the
/// rail. Each names the element(s) it paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ThreadControl {
    /// `thread-add-participant-button`.
    Invite,
    /// The remove gesture of `thread-member-chip[i]` (the chip itself stays:
    /// it is the roster).
    RemoveMember,
    /// The room policy editor's staged controls and their Save:
    /// `room-join-rule-select`, `room-history-policy-select`,
    /// `room-nest-read-toggle`, `room-labeler-toggle[i]`,
    /// `room-settings-save-button`.
    SetPolicy,
    /// `room-admin-toggle[i]`.
    AppointAdmins,
    /// `room-owner-transfer-button[i]`.
    TransferOwnership,
    /// `room-leave-button` and `room-leave-confirm`.
    LeaveRoom,
    /// `room-remove-button` and `room-remove-confirm`.
    LocalRemove,
    /// The reply bar's `topic-toggle-button`.
    Subject,
}

impl ThreadCapabilities {
    /// Whether `control` can succeed now on this thread, for a viewer whose
    /// role in the room is `my_role` (`None` on a policy-less thread).
    ///
    /// - every control the capability allows → possible;
    /// - the walk-out of the room's owner → blocked, naming the hand-over;
    /// - everything else → absent: a role only the owner or an admin can
    ///   grant, or a rail that has no such act.
    pub fn possibility(&self, control: ThreadControl, my_role: Option<RoomRole>) -> Possibility {
        let allowed = match control {
            ThreadControl::Invite => self.supports_membership_change && self.can_invite,
            ThreadControl::RemoveMember => {
                self.supports_membership_change && self.can_remove_members
            }
            ThreadControl::SetPolicy => self.can_set_policy,
            ThreadControl::AppointAdmins => self.can_appoint_admins,
            ThreadControl::TransferOwnership => self.can_transfer_ownership,
            ThreadControl::LeaveRoom => self.can_leave_room,
            ThreadControl::LocalRemove => self.supports_local_remove,
            ThreadControl::Subject => self.supports_subject,
        };
        if allowed {
            return Possibility::Possible;
        }
        // The gate closes the walk-out on the owner alone, and only on a
        // governed room (a policy-less one has no owner to strand): the one
        // `false` here the viewer can lift, by handing the room over.
        if control == ThreadControl::LeaveRoom && my_role == Some(RoomRole::Owner) {
            return Possibility::blocked(LocalizedText::key(OWNER_LEAVE_REASON_KEY));
        }
        Possibility::Absent
    }
}

/// The i18n key of the sentence a withheld reply bar paints — the same one
/// `thread-room-notice` states for a bridged room no bridge serves
/// ([`crate::room::RoomNotice::BridgeDisconnected`]), so the disabled bar and
/// the notice never disagree.
pub const SEND_BLOCKED_REASON_KEY: &str = "conversations.unified.room_notice_bridge_disconnected";

impl ThreadCapabilities {
    /// Whether the reply bar (`dm-text-field`, `dm-send-button`) can send now:
    /// the typed answer beside the [`Self::can_send`] bool, which stays for the
    /// app families that already read it. Possible, or blocked by the sentence
    /// the room notice states (the room stays readable, never writable).
    pub fn send_possibility(&self) -> Possibility {
        if self.can_send {
            Possibility::Possible
        } else {
            Possibility::blocked(LocalizedText::key(SEND_BLOCKED_REASON_KEY))
        }
    }
}

/// [`ThreadCapabilities::possibility`] over FFI: a UniFFI record carries no
/// methods, so the app families read the predicate through this function
/// (the `mail_settings_mail_usable` shape).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn thread_control_possibility(
    capabilities: ThreadCapabilities,
    control: ThreadControl,
    my_role: Option<RoomRole>,
) -> Possibility {
    capabilities.possibility(control, my_role)
}

/// [`ThreadCapabilities::send_possibility`] over FFI — the reply bar's
/// (`dm-text-field`, `dm-send-button`) answer, read through a free function for
/// the same no-methods-on-a-record reason as [`thread_control_possibility`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn thread_send_possibility(capabilities: ThreadCapabilities) -> Possibility {
    capabilities.send_possibility()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Rail;
    use crate::capabilities::derive_capabilities;
    use crate::room::{
        HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot, RoomPolicySnapshot,
        RoomSnapshot,
    };
    use crate::thread::ThreadFlavor;

    #[test]
    fn the_send_answer_follows_can_send_and_says_the_room_notice_sentence() {
        let open = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);
        assert!(open.can_send);
        assert_eq!(open.send_possibility(), Possibility::Possible);
        let shut = ThreadCapabilities {
            can_send: false,
            ..open
        };
        assert_eq!(
            shut.send_possibility(),
            Possibility::blocked(LocalizedText::key(SEND_BLOCKED_REASON_KEY))
        );
        assert!(fauna_i18n::strings::lookup(SEND_BLOCKED_REASON_KEY).is_some());
    }

    const ALL: [ThreadControl; 8] = [
        ThreadControl::Invite,
        ThreadControl::RemoveMember,
        ThreadControl::SetPolicy,
        ThreadControl::AppointAdmins,
        ThreadControl::TransferOwnership,
        ThreadControl::LeaveRoom,
        ThreadControl::LocalRemove,
        ThreadControl::Subject,
    ];

    fn governed(role: RoomRole, join_rule: JoinRule) -> RoomSnapshot {
        RoomSnapshot {
            class: RoomClass::EndToEnd,
            members: vec![RoomMemberSnapshot {
                kind: PrincipalKind::User,
                role: Some(role),
            }],
            policy: Some(RoomPolicySnapshot {
                version: 1,
                name: None,
                join_rule,
                history_policy: HistoryPolicy::None,
            }),
            my_role: Some(role),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            severance_owed: false,
            moderation_unverified: false,
            bridge_disconnected: false,
            pending_invites: None,
            search: None,
        }
    }

    /// The capability vector a seat of `role` reads on a governed MLS group.
    fn seat(role: RoomRole, join_rule: JoinRule) -> ThreadCapabilities {
        governed(role, join_rule).gate(derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup))
    }

    fn answer(role: RoomRole, join_rule: JoinRule, control: ThreadControl) -> Possibility {
        seat(role, join_rule).possibility(control, Some(role))
    }

    fn owner_leave_blocked() -> Possibility {
        Possibility::blocked(LocalizedText::key(OWNER_LEAVE_REASON_KEY))
    }

    #[test]
    fn the_owner_may_do_everything_but_walk_out_which_waits_on_the_hand_over() {
        for control in [
            ThreadControl::Invite,
            ThreadControl::RemoveMember,
            ThreadControl::SetPolicy,
            ThreadControl::AppointAdmins,
            ThreadControl::TransferOwnership,
            ThreadControl::Subject,
        ] {
            assert_eq!(
                answer(RoomRole::Owner, JoinRule::Invite, control),
                Possibility::Possible,
                "{control:?}"
            );
        }
        assert_eq!(
            answer(RoomRole::Owner, JoinRule::Invite, ThreadControl::LeaveRoom),
            owner_leave_blocked()
        );
    }

    #[test]
    fn the_owners_blocked_walk_out_names_the_sentence_the_manager_refuses_with() {
        assert_eq!(
            fauna_i18n::strings::lookup(OWNER_LEAVE_REASON_KEY),
            Some(fauna_i18n::strings::error::send::ROOM_OWNER_CANNOT_LEAVE)
        );
    }

    #[test]
    fn an_admin_governs_the_room_but_never_its_admins_or_its_owner() {
        for control in [
            ThreadControl::Invite,
            ThreadControl::RemoveMember,
            ThreadControl::SetPolicy,
            ThreadControl::LeaveRoom,
        ] {
            assert_eq!(
                answer(RoomRole::Admin, JoinRule::Invite, control),
                Possibility::Possible,
                "{control:?}"
            );
        }
        for control in [
            ThreadControl::AppointAdmins,
            ThreadControl::TransferOwnership,
        ] {
            assert_eq!(
                answer(RoomRole::Admin, JoinRule::Invite, control),
                Possibility::Absent,
                "{control:?}"
            );
        }
    }

    #[test]
    fn a_plain_member_never_sees_a_control_only_a_role_could_give() {
        for control in [
            ThreadControl::Invite,
            ThreadControl::RemoveMember,
            ThreadControl::SetPolicy,
            ThreadControl::AppointAdmins,
            ThreadControl::TransferOwnership,
        ] {
            assert_eq!(
                answer(RoomRole::Member, JoinRule::Invite, control),
                Possibility::Absent,
                "{control:?}"
            );
        }
        assert_eq!(
            answer(RoomRole::Member, JoinRule::Invite, ThreadControl::LeaveRoom),
            Possibility::Possible
        );
    }

    #[test]
    fn a_member_invites_where_the_join_rule_lets_members_invite() {
        assert_eq!(
            answer(
                RoomRole::Member,
                JoinRule::MemberInvite,
                ThreadControl::Invite
            ),
            Possibility::Possible
        );
    }

    #[test]
    fn the_local_remove_is_a_bridged_rails_and_absent_elsewhere() {
        let bridged = derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne);
        assert_eq!(
            bridged.possibility(ThreadControl::LocalRemove, None),
            Possibility::Possible
        );
        let mls = derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup);
        assert_eq!(
            mls.possibility(ThreadControl::LocalRemove, None),
            Possibility::Absent
        );
    }

    #[test]
    fn the_topic_toggle_is_absent_where_the_rail_has_no_subject() {
        let bridged = derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne);
        assert_eq!(
            bridged.possibility(ThreadControl::Subject, None),
            Possibility::Absent
        );
        let mls = derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne);
        assert_eq!(
            mls.possibility(ThreadControl::Subject, None),
            Possibility::Possible
        );
    }

    #[test]
    fn a_one_to_one_offers_no_walk_out_and_no_room_controls() {
        // Leaving the only other person is the thread delete, not a membership
        // act: absent, never a disabled control.
        let dm = derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne);
        for control in [
            ThreadControl::LeaveRoom,
            ThreadControl::SetPolicy,
            ThreadControl::AppointAdmins,
            ThreadControl::TransferOwnership,
        ] {
            assert_eq!(
                dm.possibility(control, None),
                Possibility::Absent,
                "{control:?}"
            );
        }
    }

    #[test]
    fn no_answer_is_blocked_but_the_owners_walk_out() {
        for role in [RoomRole::Owner, RoomRole::Admin, RoomRole::Member] {
            for rule in [JoinRule::Invite, JoinRule::MemberInvite] {
                for control in ALL {
                    let got = answer(role, rule, control);
                    if role == RoomRole::Owner && control == ThreadControl::LeaveRoom {
                        assert_eq!(got, owner_leave_blocked());
                    } else {
                        assert!(
                            !matches!(got, Possibility::Blocked { .. }),
                            "{role:?} {rule:?} {control:?}: {got:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_ffi_twin_answers_what_the_method_does() {
        let caps = seat(RoomRole::Owner, JoinRule::Invite);
        for control in ALL {
            assert_eq!(
                thread_control_possibility(caps, control, Some(RoomRole::Owner)),
                caps.possibility(control, Some(RoomRole::Owner))
            );
        }
    }
}
