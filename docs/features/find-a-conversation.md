---
slug: find-a-conversation
title: Find a conversation
section: everyday
goal: docs/goal/ui/conversations.md § User actions
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Type in the box above the list and it narrows to conversations whose name or
last message matches, including the text of encrypted messages, which only your
device can read. The sort button cycles latest, oldest and unread first.

## Coverage contract

Stamped 2026-09-19 at e9152e3330.

1. [app] Typing narrows the list to matching conversations, and clearing restores it — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_conversations_search.py::test_search_filters_thread_list`
   - `tests/e2e-unified/tests/test_conversations_real_search.py::test_search_filters_on_real_decrypted_snippet`
   - `tests/e2e-unified/tests/test_messaging.py::test_conversation_search_visible`
   - `tests/e2e-unified/tests/test_conversations_search.py::test_search_finds_a_term_past_the_visible_snippet`
2. [app] The sort button cycles through the three orders — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_messaging.py::test_conversation_sort_button_cycles_three_orders_without_error`
3. [app] Each conversation in the list shows whether it has unread messages, which network it is on and when it last moved — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_messaging.py::test_conversation_row_shows_unread_until_its_thread_is_opened`
   - `tests/e2e-unified/tests/test_messaging.py::test_conversation_row_shows_when_it_last_moved`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+72d6a508 standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_search.py::test_search_filters_thread_list` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_real_search.py::test_search_filters_on_real_decrypted_snippet` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_messaging.py::test_conversation_search_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_search.py::test_search_finds_a_term_past_the_visible_snippet` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_messaging.py::test_conversation_sort_button_cycles_three_orders_without_error` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_messaging.py::test_conversation_row_shows_unread_until_its_thread_is_opened` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_messaging.py::test_conversation_row_shows_when_it_last_moved` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
