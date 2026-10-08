---
slug: reactions-and-message-delete
title: React to messages and delete your own
section: everyday
goal: docs/goal/ui/conversations.md § Reactions & message delete
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Every encrypted message has a menu with quick reactions, and your own messages
can be deleted, leaving a tombstone the other side sees. Mail messages, which cannot
carry reactions, offer neither.

## Coverage contract

Stamped 2026-09-19 at eb7ece9829.

1. [app] A message's menu offers quick reactions, and a reaction from someone else shows on your copy — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_reaction_option_elements_present_in_flyout`
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_react_cross_member_peer_sees_pill`
2. [app] Deleting your own message, with confirmation, leaves a tombstone; other people's messages offer no delete — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_message_delete.py::test_delete_own_message_shows_tombstone`
   - `tests/e2e-unified/tests/test_conversations_message_delete.py::test_peer_message_delete_button_absent`
3. [app] A mail message offers neither, because mail cannot carry them — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_capability_gate_no_button_on_smtp_thread`
4. [app] Tapping a reaction you gave takes it back — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_tapping_your_own_reaction_takes_it_back`
5. [app] Beyond the six quick reactions, a fuller picker lets you react with any emoji — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_the_fuller_picker_reacts_with_an_emoji_outside_the_quick_set`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+55c383bf standalone |
| linux | ✅ full | |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+a20190e6 live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_reaction_option_elements_present_in_flyout` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_react_cross_member_peer_sees_pill` | web (linux): passed, web (windows): error, linux (linux): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_conversations_message_delete.py::test_delete_own_message_shows_tombstone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_message_delete.py::test_peer_message_delete_button_absent` | web (linux): passed, linux (linux): passed, windows (windows): error, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_capability_gate_no_button_on_smtp_thread` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_tapping_your_own_reaction_takes_it_back` | linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_the_fuller_picker_reacts_with_an_emoji_outside_the_quick_set` | linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
