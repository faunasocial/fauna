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

Your mail is indexed on your own devices from its decrypted contents, and your
files by their names, so the same search box finds a phrase inside a message
stored encrypted on your nest. The index travels between your devices encrypted,
and your nest cannot read it, apart from the part covering your mail, which it
can open with the password of a mail, calendar, contacts or file app you
connect, and only for that app's signed-in session.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] A mail that just arrived is found by a phrase from its body — `docs/goal/behavior/content-index.md` § Where queries run — per app
   - `tests/e2e-unified/tests/test_search_local_index.py::test_received_mail_is_found_by_search_via_the_local_index`
   - `tests/e2e-unified/tests/test_search_local_index_phone.py::test_a_phone_finds_a_received_mail_in_the_index_a_desktop_seat_built`
2. [app] Opening a mail search result marks the right message and scrolls it into view — `docs/goal/ui/conversations.md` § The selected message
   - `tests/e2e-unified/tests/test_conversations_selected_message.py::test_select_thread_and_message_marks_and_scrolls_the_named_message`
3. [app] A contact and a file of your own are found by their names — `docs/goal/behavior/content-index.md` § Goal
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
9. [app] Opening a draft found by search opens the composer holding that draft, whether it belongs to a conversation or stands alone — `docs/goal/ui/search.md` § State & data shape
   - `tests/e2e-unified/tests/test_search_private_index.py::test_opening_a_draft_found_by_search_opens_the_composer_holding_it`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_opens_a_draft_a_desktop_seat_indexed_into_the_composer_holding_it`
10. [app] A post of yours that both your private index and your nest find is listed once, not twice — `docs/goal/ui/search.md` § State & data shape
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_post_found_by_both_the_private_index_and_the_nest_is_listed_once`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_lists_once_a_post_both_a_desktop_seats_index_and_the_nest_find`
11. [app] After you rotate your mail keys, searching on this device still finds the mail from before — `docs/goal/architecture/owner-key-material.md` § Implementation status today
   - `tests/e2e-unified/tests/test_search_private_index.py::test_mail_indexed_before_a_mail_key_rotation_is_still_found_after_it`
12. [app] An event on your calendar is found by search on this device, and still after you rotate your mail keys — `docs/goal/behavior/content-index.md` § What's indexed
   - `tests/e2e-unified/tests/test_search_private_index.py::test_an_event_on_your_calendar_is_found_by_search_and_still_after_a_mail_key_rotation`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_and_opens_an_event_a_desktop_seat_indexed_and_still_after_a_mail_key_rotation`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| ios | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_search_local_index.py::test_received_mail_is_found_by_search_via_the_local_index` — requires `dm-message-timestamp-selected` (test harness) (not built: web, ios, ios-kids) | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_search_local_index_phone.py::test_a_phone_finds_a_received_mail_in_the_index_a_desktop_seat_built` | ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_selected_message.py::test_select_thread_and_message_marks_and_scrolls_the_named_message` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_navigates_to_its_card` | web (linux): skipped, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_file_search_result_navigates_to_its_media_detail` | web (linux): skipped, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_contact_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_file_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed` | macos (macos): failed, ios (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_content_indexed_on_one_device_is_found_from_another_that_never_indexed_it` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_content_a_desktop_seat_of_the_same_account_indexed` | macos (macos): passed, ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_result_shows_the_item_as_it_is_now` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_result_shows_the_draft_as_it_is_now_once_a_builder_relaunches` | macos (macos): passed, ios (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_content_index_rail.py::test_private_index_rests_opaque_syncs_to_a_second_device_and_survives_restart` | nest (linux): passed, nest (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_mail_your_device_filed_as_junk_never_turns_up_in_search` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_mail_a_phone_filed_as_junk_never_turns_up_in_its_search_over_a_desktop_seats_index` | macos (macos): passed, ios (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_opening_a_draft_found_by_search_opens_the_composer_holding_it` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_opens_a_draft_a_desktop_seat_indexed_into_the_composer_holding_it` | ios (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_post_found_by_both_the_private_index_and_the_nest_is_listed_once` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_lists_once_a_post_both_a_desktop_seats_index_and_the_nest_find` | ios (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_mail_indexed_before_a_mail_key_rotation_is_still_found_after_it` | tui (linux): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_an_event_on_your_calendar_is_found_by_search_and_still_after_a_mail_key_rotation` | tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_and_opens_an_event_a_desktop_seat_indexed_and_still_after_a_mail_key_rotation` | — |
<!-- features-render:end -->
