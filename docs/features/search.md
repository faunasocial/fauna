---
slug: search
title: Search
section: everyday
goal: docs/goal/ui/search.md § Goal
guide: docs/guides/app-tour.md § Search
absences:
  web (outcomes 4, 8): "docs/goal/behavior/content-index.md § Where queries run — per app"
---

## What a user gets

One search box finds posts, people and files. Results open where they live: a
post in its detail, a person on their card, a file in Media. Long result lists load
more on demand. People and files are found through the private index your own
devices keep, so in the web app, which keeps none, search finds posts.

## Coverage contract

Stamped 2026-09-26 at 61f9d7ceb0.

1. [app] Searching returns results, and a search for nothing says there are none — `docs/goal/ui/search.md` § User actions
   - `tests/e2e-unified/tests/test_search.py::test_search_and_clear`
   - `tests/e2e-unified/tests/test_search.py::test_search_no_results`
2. [app] A long result list offers to load more — `docs/goal/ui/search.md` § User actions
   - `tests/e2e-unified/tests/test_search.py::test_search_load_more_hidden_when_under_limit`
   - `tests/e2e-unified/tests/test_search.py::test_search_load_more_button_appears_at_limit`
3. [app] A post result opens in its detail — `docs/goal/ui/search.md` § User actions
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_post_search_result_navigates_to_its_post_detail`
4. [app] A deep-linked result whose target was deleted since being indexed surfaces an error rather than a blank pane — `docs/goal/ui/search.md` § Implementation status today
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_for_a_deleted_card_surfaces_error`
   - `tests/e2e-unified/tests/test_search.py::test_a_phone_result_for_a_card_deleted_since_a_desktop_seat_indexed_it_surfaces_error`
5. [app] Narrowing a search to one kind of thing shows only results of that kind — `docs/goal/ui/search.md` § Goal
   - `tests/e2e-unified/tests/test_search_outcomes.py::test_narrowing_a_search_to_one_kind_shows_only_that_kind`
6. [app] A search that fails says so and still shows what it did find, instead of going blank — `docs/goal/ui/search.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_says_so_and_keeps_what_it_found`
   - `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_on_a_phone_keeps_the_draft_a_desktop_seat_indexed`
   - `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_on_web_says_so_instead_of_going_blank`
7. [app] Each result shows a snippet of the text that matched and says what kind of thing it is — `docs/goal/ui/search.md` § Goal
   - `tests/e2e-unified/tests/test_search_outcomes.py::test_each_search_result_shows_its_matching_snippet_and_its_kind`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents`
   - `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed`
8. [app] A contact or file found in your private index opens on its card or in Media — `docs/goal/ui/search.md` § User actions
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_navigates_to_its_card`
   - `tests/e2e-unified/tests/test_search.py::test_activating_a_file_search_result_navigates_to_its_media_detail`
   - `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_contact_result_a_desktop_seat_indexed`
   - `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_file_result_a_desktop_seat_indexed`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+e48628f5 standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ✅ full | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_search.py::test_search_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_search.py::test_search_no_results` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_search.py::test_search_load_more_hidden_when_under_limit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_search.py::test_search_load_more_button_appears_at_limit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_post_search_result_navigates_to_its_post_detail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_for_a_deleted_card_surfaces_error` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_result_for_a_card_deleted_since_a_desktop_seat_indexed_it_surfaces_error` | macos (macos): passed, ios (macos): passed |
| 4 | app | absent by design on web | — |
| 5 | app | `tests/e2e-unified/tests/test_search_outcomes.py::test_narrowing_a_search_to_one_kind_shows_only_that_kind` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_says_so_and_keeps_what_it_found` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): failed |
| 6 | app | `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_on_a_phone_keeps_the_draft_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_search_outcomes.py::test_a_failed_search_on_web_says_so_instead_of_going_blank` | web (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_search_outcomes.py::test_each_search_result_shows_its_matching_snippet_and_its_kind` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): failed |
| 7 | app | `tests/e2e-unified/tests/test_search_private_index.py::test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed` | macos (macos): failed, ios (macos): failed |
| 8 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_contact_search_result_navigates_to_its_card` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_search.py::test_activating_a_file_search_result_navigates_to_its_media_detail` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_contact_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_search.py::test_a_phone_opens_a_file_result_a_desktop_seat_indexed` | macos (macos): passed, ios (macos): passed |
| 8 | app | absent by design on web | — |
<!-- features-render:end -->
