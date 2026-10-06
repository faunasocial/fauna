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

Stamped 2026-09-19 at fdd225839d.

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
11. [app] The conversation's header says how private the room is — `docs/goal/behavior/conversation-rooms.md` § The three classes
   - `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal`
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

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3bd74b7 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+b9593858 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_thread_membership.py::test_add_to_oneonone_forks_new_group` | web (linux): passed, web (windows): passed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_add_through_the_ui` | web (linux): passed, web (windows): error, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_thread_rename.py::test_rename_mls_group` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_thread_rename.py::test_rename_button_hidden_on_oneonone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_group_channel` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_mls_remove_through_the_ui` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_none_sees_nothing_before_the_join` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversation_room_history.py::test_a_newcomer_under_full_sees_the_conversation_so_far` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversation_room_ownership.py::test_the_owner_hands_the_room_over_and_the_root_moves` | web (linux): passed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_conversation_room_leave.py::test_a_member_leaves_the_room_and_the_floor_drops_them` | windows (windows): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversation_room_roles.py::test_a_room_is_born_governed_and_its_roles_govern_removal` | web (linux): passed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_conversation_room_join_rule.py::test_the_join_rule_decides_whether_a_plain_member_may_invite` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_conversation_room_delete_any.py::test_an_admin_and_the_owner_delete_a_members_message_for_everyone` | tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_thread_membership_real.py::test_in_place_add_of_an_unreachable_person_is_refused` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_conversation_room_community.py::test_a_community_room_is_founded_joined_searched_and_its_read_withdrawn` | tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_conversation_room_labelers.py::test_a_community_rooms_owner_names_a_labeler_and_a_members_message_is_labelled` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_conversation_room_community_cross_nest.py::test_a_member_of_another_nest_joins_a_community_room_through_the_relay` | tui (linux): passed |
<!-- features-render:end -->
