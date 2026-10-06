---
slug: private-search-index
title: Search your own mail and files, privately, on this device
section: everyday
goal: docs/goal/behavior/content-index.md § Goal
guide: docs/guides/who-can-see-what.md § Search
absences:
  web: "docs/goal/behavior/content-index.md § Where queries run — per app"
---

## What a user gets

Your mail and files are indexed on your own device from their unsealed
contents, so the same search box finds a phrase inside a message your nest cannot
read. The index syncs sealed between your devices; the nest never holds a readable
copy.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] A mail that just arrived is found by a phrase from its body — `docs/goal/behavior/content-index.md` § Where queries run — per app
   - `tests/e2e-unified/tests/test_search_local_index.py::test_received_mail_is_found_by_search_via_the_local_index`
2. [app] Opening a mail search result marks the right message and scrolls it into view — `docs/goal/ui/conversations.md` § The selected message
   - `tests/e2e-unified/tests/test_conversations_selected_message.py::test_select_thread_and_message_marks_and_scrolls_the_named_message`
3. [app] A contact and a file of your own are found by a phrase from their contents — `docs/goal/behavior/content-index.md` § Goal
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_navigates_to_its_card`
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_file_search_result_navigates_to_its_media_detail`
   - `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_contact_result_a_desktop_seat_indexed`
   - `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_file_result_a_desktop_seat_indexed`
4. [app] A conversation message, a post and a draft of your own are found by a phrase from their contents — `docs/goal/behavior/content-index.md` § Goal
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed`
5. [app] Content indexed on one of your devices is found from another device that never indexed it — `docs/goal/behavior/content-index.md` § Goal
   - `tests/e2e-unified/tests/test_search_private_index.py::test_content_indexed_on_one_device_is_found_from_another_that_never_indexed_it`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_content_a_desktop_seat_of_the_same_account_indexed`
6. [app] A result shows the item as it is now: edited text is what matches, and something deleted since it was indexed stops appearing — `docs/goal/behavior/content-index.md` § Where queries run — per app
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_result_shows_the_item_as_it_is_now`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_result_shows_the_draft_as_it_is_now_once_a_builder_relaunches`
7. [nest] Your nest stores and syncs your private index without being able to read it, and a nest restart does not lose it — `docs/goal/behavior/content-index.md` § Encryption posture — what's plaintext where
   - `tests/e2e-unified/tests/api/test_content_index_rail.py::test_private_index_rests_opaque_syncs_to_a_second_device_and_survives_restart`
8. [app] Mail your device filed as junk never turns up in your search results — `docs/goal/behavior/content-index.md` § What's indexed
   - `tests/e2e-unified/tests/test_search_private_index.py::test_mail_your_device_filed_as_junk_never_turns_up_in_search`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_mail_a_phone_filed_as_junk_never_turns_up_in_its_search_over_a_desktop_seats_index`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+9b7a1a50 standalone |
| android | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_search_local_index.py::test_received_mail_is_found_by_search_via_the_local_index` | linux (linux): passed, windows (windows): failed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_selected_message.py::test_select_thread_and_message_marks_and_scrolls_the_named_message` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_navigates_to_its_card` | linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_file_search_result_navigates_to_its_media_detail` | linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_contact_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_file_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): failed |
| 4 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed` | macos (macos): failed, ios (macos): failed |
| 5 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_content_indexed_on_one_device_is_found_from_another_that_never_indexed_it` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): failed |
| 5 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_content_a_desktop_seat_of_the_same_account_indexed` | macos (macos): passed, ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_result_shows_the_item_as_it_is_now` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_result_shows_the_draft_as_it_is_now_once_a_builder_relaunches` | macos (macos): passed, ios (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_content_index_rail.py::test_private_index_rests_opaque_syncs_to_a_second_device_and_survives_restart` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_mail_your_device_filed_as_junk_never_turns_up_in_search` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_mail_a_phone_filed_as_junk_never_turns_up_in_its_search_over_a_desktop_seats_index` | macos (macos): passed, ios (macos): passed |
<!-- features-render:end -->
