---
slug: contacts
title: Contacts
section: everyday
goal: docs/goal/ui/contacts.md § Goal
guide: docs/guides/app-tour.md § Contacts
---

## What a user gets

Your contacts page lists the people you have connected with and the knocks
waiting for you. Look someone up by their handle, send a knock, and see one arrive
while you watch. Blocking someone is a toggle on their profile.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] The roster and pending knocks load, each knock naming its sender, and typing narrows the roster — `docs/goal/ui/contacts.md` § Layout & flow
   - `tests/e2e-unified/tests/test_contacts.py::test_view_contacts`
   - `tests/e2e-unified/tests/test_contacts.py::test_roster_filter_narrows_by_handle`
   - `tests/e2e-unified/tests/test_contacts.py::test_knock_count_accessible`
   - `tests/e2e-unified/tests/test_knock_sender_display.py::test_knock_sender_shows_the_shared_short_id`
2. [app] Looking up a handle nobody has says so — `docs/goal/ui/contacts.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_contacts.py::test_contact_find_error_on_bad_lookup`
3. [app] Sending a knock puts it in the other person's queue — `docs/goal/ui/contacts.md` § User actions
   - `tests/e2e-unified/tests/test_contacts_knock_send_web.py::test_web_add_contact_sends_knock_to_recipient`
4. [app] A knock that arrives shows on the page without a reload — `docs/goal/ui/contacts.md` § Where logic lives
   - `tests/e2e-unified/tests/test_knock_live_refresh.py::test_knock_push_live_refreshes_mounted_contacts_page`
5. [app] A knock reaches someone on another nest — `docs/goal/ui/contacts.md` § Goal
   - `tests/e2e-unified/tests/test_contacts_cross_nest_knock.py::test_cross_nest_knock_reaches_the_peers_own_queue`
6. [nest] Your nest keeps your roster, accepts knocks, and honours who may reach you — `docs/goal/behavior/direct-messages.md` § Reach policy
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_contacts_accept_knock`
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_contacts_list_empty`
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_inbox_mode_api`
7. [app] Accepting a knock adds that person to your contacts, and blocking or dismissing one clears it from the list — `docs/goal/ui/contacts.md` § Layout & flow
   - `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_accepting_a_knock_adds_the_person_and_blocking_or_dismissing_clears_it`
8. [app] Confirming an accepted contact promotes it, and the confirm action is offered only on a contact it applies to — `docs/goal/ui/contacts.md` § Layout & flow
   - `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_confirming_an_accepted_contact_promotes_it_and_confirm_shows_only_where_it_applies`
9. [app] Each contact row says where the relationship stands — `docs/goal/ui/contacts.md` § Where logic lives
   - `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_each_contact_row_says_where_the_relationship_stands`
10. [nest] A message that actually reaches you from an accepted contact confirms the relationship without anyone clicking confirm — `docs/goal/ui/contacts.md` § Where logic lives
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_delivered_message_confirms_an_accepted_contact_with_no_confirm_call`
11. [app] Your own nickname for a person heads their row with their public name beneath it, your labels show on the row, and typing a label narrows the list to the people carrying it — `docs/goal/ui/contacts.md` § The private overlay
   - `tests/e2e-unified/tests/test_contact_overlay.py::test_a_nickname_notes_and_label_paint_on_the_roster_and_profile_and_survive_a_relaunch`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+4bc2efab standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_contacts.py::test_view_contacts` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_contacts.py::test_roster_filter_narrows_by_handle` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_contacts.py::test_knock_count_accessible` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_knock_sender_display.py::test_knock_sender_shows_the_shared_short_id` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_contacts.py::test_contact_find_error_on_bad_lookup` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_contacts_knock_send_web.py::test_web_add_contact_sends_knock_to_recipient` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): failed |
| 4 | app | `tests/e2e-unified/tests/test_knock_live_refresh.py::test_knock_push_live_refreshes_mounted_contacts_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_contacts_cross_nest_knock.py::test_cross_nest_knock_reaches_the_peers_own_queue` | web (linux): skipped, linux (linux): skipped, windows (windows): failed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_contacts_accept_knock` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_contacts_list_empty` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_inbox_mode_api` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_accepting_a_knock_adds_the_person_and_blocking_or_dismissing_clears_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_confirming_an_accepted_contact_promotes_it_and_confirm_shows_only_where_it_applies` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_contacts_edge_actions.py::test_each_contact_row_says_where_the_relationship_stands` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_delivered_message_confirms_an_accepted_contact_with_no_confirm_call` | nest (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_contact_overlay.py::test_a_nickname_notes_and_label_paint_on_the_roster_and_profile_and_survive_a_relaunch` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
