---
slug: group-conversations
title: Group conversations
section: everyday
goal: docs/goal/behavior/conversation-rooms.md § Goal
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Add a person to a conversation and it becomes a group; groups can be renamed.
Adding someone to an existing group brings them in without starting over, and
taking someone out removes them from what the group says next.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] Adding a person to a one-to-one conversation starts a group — `docs/goal/behavior/direct-messages.md` § Group-Forked Threads (N-Member MLS)
   - `tests/e2e-unified/tests/test_thread_membership.py::test_add_to_oneonone_forks_new_group`
2. [app] Adding a person to a group brings them in and the group stays the same group — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_add_through_the_ui`
3. [app] A group can be renamed; a one-to-one conversation cannot — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_thread_rename.py::test_rename_mls_group`
   - `tests/e2e-unified/tests/test_thread_rename.py::test_rename_button_hidden_on_oneonone`
4. [nest] A group spanning two nests works — `docs/goal/behavior/direct-messages.md` § Technical Flow — Cross-Nest
   - `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_group_channel`
5. [app] Taking someone out of a group removes them, and they stop receiving what the group says next — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_remove_through_the_ui`
6. [app] A group made of Fauna members is an end-to-end room: its creator owns it, everyone else is a plain member who cannot remove anyone, and the owner's removal is a real re-key the removed person cannot follow — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
7. [app] Someone added to a room later sees only what the room says after they join, unless the room's history rule shares the past — `docs/goal/behavior/conversation-rooms.md` § History for joiners
   - `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_none_sees_nothing_before_the_join`
   - `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_full_sees_the_conversation_so_far`
8. [app] The owner of a room can hand it to another member: the new owner runs the room from then on, and the previous owner is an ordinary member who can be removed — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_ownership.py::test_the_owner_hands_the_room_over_and_the_root_moves`
9. [app] You can leave a group yourself; the owner hands the room over first — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_leave.py::test_a_member_leaves_the_room_and_the_floor_drops_them`
10. [app] The owner makes a member an admin, and an admin can then remove people — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
11. [app] The conversation's header says how private the room is: end-to-end, community (searched and labelled by the home nest), or carried by mail or another network — `docs/goal/behavior/conversation-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge`
12. [app] The owner or an admin chooses who may bring people in — themselves only, or any member — and the add-person control follows that choice — `docs/goal/behavior/conversation-rooms.md` § Join rules and invites
   - `tests/e2e-unified/tests/test_conversation_room_join_rule.py::test_the_join_rule_decides_whether_a_plain_member_may_invite`
13. [app] The owner or an admin of a room can delete anyone's message in it, leaving the same tombstone for everyone; a plain member can delete only their own — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_delete_any.py::test_an_admin_and_the_owner_delete_a_members_message_for_everyone`
14. [app] Someone who cannot be reached yet cannot be added to a group: the attempt is refused, the person does not stay in the list, and the app says why — `docs/goal/ui/conversations.md` § Participants vs. reply recipients
   - `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_add_of_an_unreachable_person_is_refused`
15. [app] A room can include your home nest, which makes it a community room the nest can search for its members: the people you invite accept or decline from their conversation list, and the owner or an admin can take the nest's read back, which empties its search — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
16. [app] A community room's owner or an admin can inspect the labelers published on the home nest and name up to four for the room; from then on each new message is labelled as it arrives, and every member sees the labels as badges — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_labelers.py::test_a_community_rooms_owner_names_a_labeler_and_a_members_message_is_labelled`
17. [app] Someone on another nest can be invited into a community room by their full handle and joins it through their own nest: the invitation reaches them there, accepting seats them, the founder's app lets them in by itself, and messages cross both ways — `docs/goal/behavior/conversation-rooms.md` § Join rules and invites
   - `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_joins_a_community_room_through_the_relay`
18. [app] A community room's members can search the room from the room itself — also from an account on another nest — and a post the search finds opens in the feed, even one their own nest does not hold — `docs/goal/behavior/room-search.md` § The surface
   - `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_searches_the_room_and_opens_a_room_post_through_the_relay`
19. [app] Whoever may withdraw a room's pending invitations sees them, and withdrawing one takes it off the invitee's list — `docs/goal/behavior/room-invitations.md` § Join rules and invites
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
20. [app] A message the room search finds opens in the conversation, selected and brought into view — `docs/goal/behavior/room-search.md` § The surface
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
21. [app] A room search that finds nothing says so, and a search that fails shows the error rather than an empty result — `docs/goal/behavior/room-search.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_failed_room_search_shows_its_error_and_a_hit_not_yet_here_selects_once_it_lands`
22. [app] Room search is offered only in community rooms — `docs/goal/behavior/room-search.md` § The surface
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn`
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
23. [app] A message the room search finds that has not reached this device yet says so, and is selected once it arrives — `docs/goal/behavior/room-search.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_failed_room_search_shows_its_error_and_a_hit_not_yet_here_selects_once_it_lands`
24. [app] In a room with someone from another nest, the member list names them, they see the same names, and a removal made by that nest's admin takes effect — `docs/goal/architecture/federation.md` § Implementation status today
   - `tests/e2e-unified/tests/test_conversation_room_cross_nest_roster.py::test_a_foreign_member_is_named_on_both_seats_and_their_removal_as_admin_lands_at_home`
25. [app] Adding someone again whose earlier join never arrived brings them in, and re-adding a member who is already in changes nothing — `docs/goal/architecture/mls-group-key-material.md` § Implementation status today
   - `tests/e2e-unified/tests/test_thread_membership_real.py::test_re_adding_a_member_whose_welcome_never_arrived_heals_and_a_duplicate_add_changes_nothing`
26. [app] If you leave a group and are brought back in, it opens at the read position you had before — `docs/goal/architecture/delegable-scope-reclamation.md` § Implementation status today
   - `tests/e2e-unified/tests/test_conversations_rejoin_read_position.py::test_a_group_left_and_re_joined_opens_at_the_old_read_position`
27. [app] A room invitation is judged again when you accept it: one from an admin since demoted, a member who has left or an identity since handed on lapses, the accept is refused and the invitation leaves your list — `docs/goal/behavior/room-invitations.md` § Join rules and invites
   - `tests/e2e-unified/tests/test_conversation_room_invitation_lapse.py::test_an_invitation_from_an_admin_since_demoted_lapses_at_accept`
28. [nest] Deleting an account removes the room invitations addressed to it, and a pending invitation follows an account its owner takes back — `docs/goal/behavior/room-invitations.md` § Join rules and invites
   - `tests/e2e-unified/tests/api/test_room_invitation_fate.py::test_deleting_an_account_removes_the_room_invitations_addressed_to_it`
   - `tests/e2e-unified/tests/api/test_room_invitation_fate.py::test_a_pending_room_invitation_follows_an_account_its_owner_takes_back`
29. [app] When another member adds or removes someone, every member's participant list follows the room's agreed roster, and a newcomer shows by name on every seat — one you never met, or seen from a seat on another nest — never as a blank — `docs/goal/behavior/conversation-rooms.md` § The floor roster
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
   - `tests/e2e-unified/tests/test_conversation_room_cross_nest_roster.py::test_a_foreign_member_is_named_on_both_seats_and_their_removal_as_admin_lands_at_home`
30. [nest] Once removed, a member stays out: no report of theirs re-seats them, nothing rewrites a community room's roster, and a member on another nest who is removed loses the roster, the member list and the attachment door until honestly re-added — `docs/goal/behavior/conversation-rooms.md` § The floor roster
   - `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_a_removed_members_own_report_never_reseats_them`
   - `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_nothing_rewrites_a_community_rooms_roster`
   - `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_a_removed_foreign_member_loses_the_three_doors_until_reinvited`
31. [app] A tombstone left by an owner or admin deleting someone else's message is still there after a relaunch — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_an_owners_delete_of_a_members_message_survives_a_relaunch`
32. [app] A policy change refused on Save shows on the page's error line and the editor stays open, and an ownership offer the room moved past tells the offering owner to offer again — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_policy_refusal.py::test_a_refused_policy_save_keeps_the_editor_open_and_a_stale_offer_says_offer_again`
33. [app] In a room spanning two nests, attachments rest on the room's home nest: both members open them, they survive clean-up on both nests, a stranger cannot upload, and a native app opens them even when the home nest's certificate is self-signed — `docs/goal/behavior/conversation-rooms.md` § The home nest
   - `tests/e2e-unified/tests/test_conversation_room_community_cross_nest_attachments.py::test_a_two_nest_rooms_attachments_rest_on_its_home_and_survive_clean_up`
34. [app] After an identity succession the successor holds the owner's or admin's seat in the room — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/test_conversation_room_community_succession.py::test_a_community_rooms_owner_takes_the_account_back_and_keeps_the_room`
35. [nest] Deleting your account is refused while you hold a community room's owner seat that a local member could take over — `docs/goal/behavior/conversation-rooms.md` § Roles and authorization
   - `tests/e2e-unified/tests/api/test_room_owner_seat_account_delete.py::test_an_owner_with_a_local_member_who_could_take_over_cannot_delete_the_account`
   - `tests/e2e-unified/tests/api/test_room_owner_seat_account_delete.py::test_a_scheduled_deletion_is_refused_again_when_a_local_member_sat_down_meanwhile`
36. [app] A removal that cannot be completed safely fails visibly and can be retried; it never reports success while the removed person keeps reading — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_conversation_room_removal_refusal.py::test_a_removal_the_nest_turns_away_fails_visibly_and_a_retry_completes_it`
37. [app] Removing someone from a community room re-keys it so they cannot read what comes next; a re-key that fails says so, and the next owner's or admin's app to look completes it without being asked — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_foreign_admin_removes_a_home_member_through_the_relay`
   - `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_removal_whose_rekey_fails_says_so_and_the_next_pass_completes_it`
38. [app] Someone added to a community room later reads only from the tip, unless the room shares its history, in which case the whole retained history arrives — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_history.py::test_a_community_rooms_later_joiner_under_none_reads_only_from_the_tip`
   - `tests/e2e-unified/tests/test_conversation_room_community_history.py::test_a_community_rooms_later_joiner_under_full_receives_the_retained_history`
39. [app] In a community room a member deletes their own message and reacts, an owner or admin deletes anyone's message leaving the same tombstone for everyone, and a deletion parked when you quit is applied after relaunch — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_rooms_own_delete_reaction_and_admin_delete_reach_every_seat`
   - `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_room_comes_back_with_its_messages_and_tombstone_after_a_relaunch`
   - `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_rooms_delete_parked_at_quit_is_applied_after_the_relaunch`
40. [app] Editing a community room's policy changes only what you changed, and handing a community room over leaves the previous owner an ordinary member who can be removed — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_policy.py::test_a_community_rooms_policy_edit_changes_only_what_changed_and_its_hand_over_demotes_the_old_owner`
41. [app] A member on another nest can leave a community room and be invited back, keeping what they already read, and someone removed can be invited again — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_foreign_member.py::test_a_foreign_member_leaves_and_returns_and_a_removed_one_is_invited_back`
42. [app] An invitation into a room you already sit in is settled quietly, declining tells the room nothing, re-inviting leaves exactly one invitation standing, and an invitation to someone whose inbox is full is refused with that reason — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_a_declined_invitation_tells_the_room_nothing_and_a_reinvitation_stands_once`
   - `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_an_invitation_to_a_full_inbox_is_refused_with_that_reason`
   - `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_an_invitation_into_a_room_you_already_sit_in_is_settled_quietly`
43. [app] A community room still waiting for its key says so, and one unkeyed room never stalls your other conversations — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_awaiting_key.py::test_a_community_room_waiting_for_its_key_says_so_and_stalls_nothing_else`
44. [app] A room whose moderation rests on a name that cannot be verified says so — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_moderation_unverified.py::test_a_room_says_so_when_its_moderation_rests_on_a_retired_name_from_another_nest`
45. [nest] Only live members read a community room's labels: a stranger or a removed member gets sealed envelopes and no verdicts, and once the members withdraw the nest's read a later re-key keeps it withdrawn — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_only_live_members_read_a_community_rooms_labels`
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_withdrawn_read_stays_withdrawn_across_a_later_rekey`
46. [nest] A community room's search serves only its members, and never a message sealed before a joiner's admission when the room shares no history — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_community_rooms_search_serves_only_its_members`
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_joiner_without_the_rooms_history_never_finds_an_earlier_message`
47. [nest] Nobody outside a community room can post into it, and a message sealed in another member's name, or by a non-member, never becomes a bubble or a search hit — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_nobody_outside_a_community_room_can_post_into_it`
   - `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_message_in_another_members_name_or_a_non_members_never_becomes_a_hit`
48. [app] A member on another nest can invite others into a community room, while handing the room to an owner on another nest is refused rather than half done — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_foreign_member.py::test_a_foreign_member_invites_into_the_room_and_handing_them_the_room_is_refused`
49. [app] Two members of a community room exchange an attachment: a picture sent there opens for the other member — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_picture.py::test_a_picture_sent_in_a_community_room_opens_for_the_other_member`
50. [app] After taking your account back, or on a seat that lost its key, your app re-keys its own seat and reads the room again — `docs/goal/behavior/community-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_community_succession.py::test_a_community_rooms_owner_takes_the_account_back_and_keeps_the_room`
   - `tests/e2e-unified/tests/test_conversation_room_community_seat_lost_key.py::test_a_community_room_seat_that_lost_its_key_is_rekeyed_by_its_own_app`
51. [app] An add that cannot be judged — the app cannot tell whether the person's earlier join arrived — says so rather than guessing — `docs/goal/architecture/mls-group-key-material.md` § Implementation status today
   - `tests/e2e-unified/tests/test_thread_membership_real.py::test_re_adding_a_member_whose_join_cannot_be_judged_says_so_and_touches_nothing`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | 🚧 unbuilt | 0.1.5-dev+390d61b5 standalone |
| linux | 🚧 unbuilt | 0.1.5-dev+390d61b5 standalone |
| windows | 🚧 unbuilt | 0.1.2-dev+b9593858 standalone |
| macos | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| ios | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| android | 🚧 unbuilt | 0.1.5-dev+390d61b5 standalone |
| tui | ✅ full | 0.1.5-dev+390d61b5 standalone |
| android-kids | 🚧 unbuilt | 0.1.5-dev+390d61b5 standalone |
| ios-kids | 🚧 unbuilt | 0.1.5-dev+390d61b5 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_thread_membership.py::test_add_to_oneonone_forks_new_group` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_add_through_the_ui` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_thread_rename.py::test_rename_mls_group` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_thread_rename.py::test_rename_button_hidden_on_oneonone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_group_channel` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_remove_through_the_ui` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_none_sees_nothing_before_the_join` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_full_sees_the_conversation_so_far` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversation_room_ownership.py::test_the_owner_hands_the_room_over_and_the_root_moves` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_conversation_room_leave.py::test_a_member_leaves_the_room_and_the_floor_drops_them` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_conversation_room_join_rule.py::test_the_join_rule_decides_whether_a_plain_member_may_invite` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_conversation_room_delete_any.py::test_an_admin_and_the_owner_delete_a_members_message_for_everyone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 14 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_add_of_an_unreachable_person_is_refused` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 16 | app | `tests/e2e-unified/tests/test_conversation_room_labelers.py::test_a_community_rooms_owner_names_a_labeler_and_a_members_message_is_labelled` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_joins_a_community_room_through_the_relay` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_searches_the_room_and_opens_a_room_post_through_the_relay` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 19 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 20 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 21 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 21 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_failed_room_search_shows_its_error_and_a_hit_not_yet_here_selects_once_it_lands` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 23 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_failed_room_search_shows_its_error_and_a_hit_not_yet_here_selects_once_it_lands` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 24 | app | `tests/e2e-unified/tests/test_conversation_room_cross_nest_roster.py::test_a_foreign_member_is_named_on_both_seats_and_their_removal_as_admin_lands_at_home` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 25 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_re_adding_a_member_whose_welcome_never_arrived_heals_and_a_duplicate_add_changes_nothing` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 26 | app | `tests/e2e-unified/tests/test_conversations_rejoin_read_position.py::test_a_group_left_and_re_joined_opens_at_the_old_read_position` | macos (macos): failed, tui (linux): passed, tui (macos): passed |
| 27 | app | `tests/e2e-unified/tests/test_conversation_room_invitation_lapse.py::test_an_invitation_from_an_admin_since_demoted_lapses_at_accept` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_room_invitation_fate.py::test_deleting_an_account_removes_the_room_invitations_addressed_to_it` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_room_invitation_fate.py::test_a_pending_room_invitation_follows_an_account_its_owner_takes_back` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 29 | app | `tests/e2e-unified/tests/test_conversation_room_cross_nest_roster.py::test_a_foreign_member_is_named_on_both_seats_and_their_removal_as_admin_lands_at_home` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_a_removed_members_own_report_never_reseats_them` | nest (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_nothing_rewrites_a_community_rooms_roster` | nest (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_room_removal_stays_out.py::test_a_removed_foreign_member_loses_the_three_doors_until_reinvited` | nest (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_an_owners_delete_of_a_members_message_survives_a_relaunch` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 32 | app | `tests/e2e-unified/tests/test_conversation_room_policy_refusal.py::test_a_refused_policy_save_keeps_the_editor_open_and_a_stale_offer_says_offer_again` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_conversation_room_community_cross_nest_attachments.py::test_a_two_nest_rooms_attachments_rest_on_its_home_and_survive_clean_up` — requires `community-room-foreign-attachment-pin` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): failed, tui (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_conversation_room_community_succession.py::test_a_community_rooms_owner_takes_the_account_back_and_keeps_the_room` — requires `community-room-survives-succession` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 35 | nest | `tests/e2e-unified/tests/api/test_room_owner_seat_account_delete.py::test_an_owner_with_a_local_member_who_could_take_over_cannot_delete_the_account` | nest (linux): passed |
| 35 | nest | `tests/e2e-unified/tests/api/test_room_owner_seat_account_delete.py::test_a_scheduled_deletion_is_refused_again_when_a_local_member_sat_down_meanwhile` | nest (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_conversation_room_removal_refusal.py::test_a_removal_the_nest_turns_away_fails_visibly_and_a_retry_completes_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_foreign_admin_removes_a_home_member_through_the_relay` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 37 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_removal_whose_rekey_fails_says_so_and_the_next_pass_completes_it` — requires `community-room-rekey-retry` (not built: web, linux, windows, macos, ios, android, android-kids, ios-kids) | macos (macos): skipped, tui (linux): passed |
| 38 | app | `tests/e2e-unified/tests/test_conversation_room_community_history.py::test_a_community_rooms_later_joiner_under_none_reads_only_from_the_tip` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 38 | app | `tests/e2e-unified/tests/test_conversation_room_community_history.py::test_a_community_rooms_later_joiner_under_full_receives_the_retained_history` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 39 | app | `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_rooms_own_delete_reaction_and_admin_delete_reach_every_seat` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 39 | app | `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_room_comes_back_with_its_messages_and_tombstone_after_a_relaunch` — requires `community-room-relaunch-restore` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 39 | app | `tests/e2e-unified/tests/test_conversation_room_delete_relaunch.py::test_a_community_rooms_delete_parked_at_quit_is_applied_after_the_relaunch` — requires `community-room-relaunch-restore` (not built: windows, android-kids, ios-kids); `room-roster-refusal-per-actor` (test harness) (not built: windows) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_conversation_room_community_policy.py::test_a_community_rooms_policy_edit_changes_only_what_changed_and_its_hand_over_demotes_the_old_owner` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 41 | app | `tests/e2e-unified/tests/test_conversation_room_community_foreign_member.py::test_a_foreign_member_leaves_and_returns_and_a_removed_one_is_invited_back` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_a_declined_invitation_tells_the_room_nothing_and_a_reinvitation_stands_once` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_an_invitation_to_a_full_inbox_is_refused_with_that_reason` — requires `room-invite-inbox-full-reason` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_conversation_room_community_invitation_edges.py::test_an_invitation_into_a_room_you_already_sit_in_is_settled_quietly` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 43 | app | `tests/e2e-unified/tests/test_conversation_room_community_awaiting_key.py::test_a_community_room_waiting_for_its_key_says_so_and_stalls_nothing_else` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 44 | app | `tests/e2e-unified/tests/test_conversation_room_community_moderation_unverified.py::test_a_room_says_so_when_its_moderation_rests_on_a_retired_name_from_another_nest` — requires `community-room-survives-succession` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_only_live_members_read_a_community_rooms_labels` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_withdrawn_read_stays_withdrawn_across_a_later_rekey` | nest (linux): passed |
| 46 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_community_rooms_search_serves_only_its_members` | nest (linux): passed |
| 46 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_joiner_without_the_rooms_history_never_finds_an_earlier_message` | nest (linux): passed |
| 47 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_nobody_outside_a_community_room_can_post_into_it` | nest (linux): passed |
| 47 | nest | `tests/e2e-unified/tests/api/test_community_room_floor_promises.py::test_a_message_in_another_members_name_or_a_non_members_never_becomes_a_hit` | nest (linux): passed |
| 48 | app | `tests/e2e-unified/tests/test_conversation_room_community_foreign_member.py::test_a_foreign_member_invites_into_the_room_and_handing_them_the_room_is_refused` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 49 | app | `tests/e2e-unified/tests/test_conversation_room_community_picture.py::test_a_picture_sent_in_a_community_room_opens_for_the_other_member` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 50 | app | `tests/e2e-unified/tests/test_conversation_room_community_succession.py::test_a_community_rooms_owner_takes_the_account_back_and_keeps_the_room` — requires `community-room-survives-succession` (not built: windows, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 50 | app | `tests/e2e-unified/tests/test_conversation_room_community_seat_lost_key.py::test_a_community_room_seat_that_lost_its_key_is_rekeyed_by_its_own_app` — requires `room-seat-lost-key` (test harness) (not built: windows) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 51 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_re_adding_a_member_whose_join_cannot_be_judged_says_so_and_touches_nothing` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
