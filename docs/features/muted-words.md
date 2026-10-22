---
slug: muted-words
title: Muted words
section: family and personalization
goal: docs/goal/behavior/moderation.md § Muted keywords
guide: docs/guides/app-tour.md § Personalization
---

## What a user gets

Words you mute collapse any post, message or notification that contains them behind a reveal — in every feed and conversation and in your notifications, on all your devices; a security notice always shows in full. A word you would rather see less of than not at all can instead only sink matching posts in your ranked feeds.

## Coverage contract

Stamped 2026-10-01 at d8cb0887cb.

1. [app] Terms are added and removed in Settings, and an empty list says so — `docs/goal/behavior/moderation.md` § Muted keywords
   - `tests/e2e-unified/tests/test_muted_words.py::test_muted_words_settings_crud`
   - `tests/e2e-unified/tests/test_muted_words.py::test_muted_words_empty_state_marks_a_loaded_page_not_a_loading_one`
2. [app] A matching post collapses behind a reveal; other posts stay — `docs/goal/behavior/moderation.md` § Muted keywords
   - `tests/e2e-unified/tests/test_feed_muted_posts.py::test_muted_post_collapses_behind_reveal`
3. [app] A matching message collapses behind a reveal, including a real encrypted one — `docs/goal/behavior/moderation.md` § Muted keywords
   - `tests/e2e-unified/tests/test_muted_words.py::test_muted_message_collapses_behind_reveal`
   - `tests/e2e-unified/tests/test_conversations_real_muted_message.py::test_real_muted_message_collapses_behind_reveal`
4. [app] Removing a muted word brings back what it was hiding — `docs/goal/architecture/content-moderation-and-ranking.md` § Composition
   - `tests/e2e-unified/tests/test_feed_muted_posts.py::test_a_word_inside_a_longer_word_collapses_and_removing_it_brings_the_post_back`
5. [app] Your muted words are the same on all your devices, and a word added on one is never lost because another device changed the list — `docs/goal/architecture/content-moderation-and-ranking.md` § Implementation status today
   - `tests/e2e-unified/tests/test_muted_words_sync.py::test_words_added_on_two_devices_at_once_are_both_kept_on_both`
6. [app] A muted word matches whatever its capitalisation — `docs/goal/architecture/content-moderation-and-ranking.md` § Resolved design decisions (2026-07-05/06)
   - `tests/e2e-unified/tests/test_muted_words.py::test_muted_message_collapses_behind_reveal`
7. [app] A muted word matches anywhere inside a longer word — `docs/goal/architecture/content-moderation-and-ranking.md` § Resolved design decisions (2026-07-05/06)
   - `tests/e2e-unified/tests/test_feed_muted_posts.py::test_a_word_inside_a_longer_word_collapses_and_removing_it_brings_the_post_back`
8. [app] A muted word also hides notifications that contain it — `docs/goal/behavior/notifications.md` § Muted words
   - `tests/e2e-unified/tests/test_notifications_muted.py::test_a_muted_word_collapses_the_notification_rows_that_carry_it`
9. [app] Each muted word can carry its own strength, from pushing matching posts down to hiding them — `docs/goal/architecture/content-moderation-and-ranking.md` § Composition
   - `tests/e2e-unified/tests/test_muted_words.py::test_muted_word_level_show_less_does_not_collapse_a_message`
10. [app] A word added on another of your devices appears on an open Muted words page without leaving it — `docs/goal/architecture/account-runtime.md` § Multi-instance concurrency
   - `tests/e2e-unified/tests/test_store_change_notice.py::test_an_open_muted_words_page_shows_another_devices_word_without_a_revisit`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+64866f6d standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_muted_words.py::test_muted_words_settings_crud` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_muted_words.py::test_muted_words_empty_state_marks_a_loaded_page_not_a_loading_one` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_muted_posts.py::test_muted_post_collapses_behind_reveal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_muted_words.py::test_muted_message_collapses_behind_reveal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_real_muted_message.py::test_real_muted_message_collapses_behind_reveal` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_muted_posts.py::test_a_word_inside_a_longer_word_collapses_and_removing_it_brings_the_post_back` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_muted_words_sync.py::test_words_added_on_two_devices_at_once_are_both_kept_on_both` | tui (linux): failed |
| 6 | app | `tests/e2e-unified/tests/test_muted_words.py::test_muted_message_collapses_behind_reveal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed_muted_posts.py::test_a_word_inside_a_longer_word_collapses_and_removing_it_brings_the_post_back` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_notifications_muted.py::test_a_muted_word_collapses_the_notification_rows_that_carry_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_muted_words.py::test_muted_word_level_show_less_does_not_collapse_a_message` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_store_change_notice.py::test_an_open_muted_words_page_shows_another_devices_word_without_a_revisit` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
