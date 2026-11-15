---
slug: reactions-and-message-delete
title: React to messages and delete your own
section: everyday
goal: docs/goal/ui/conversations.md § Reactions & message delete
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Every encrypted message has a menu with quick reactions, and your own messages
can be deleted, leaving **This message was deleted** in its place for everyone
in the conversation. Mail messages, which cannot carry reactions, offer neither.

## Coverage contract

Stamped 2026-09-19 at eb7ece9829.

1. [app] A message's menu offers quick reactions, and a reaction from someone else shows on your copy — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_reaction_option_elements_present_in_flyout`
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_react_cross_member_peer_sees_pill`
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_native_peer_reaction_shows_pill_on_senders_copy`
2. [app] Deleting your own message, with confirmation, leaves a tombstone; other people's messages offer no delete — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_message_delete.py::test_delete_own_message_shows_tombstone`
   - `tests/e2e-unified/tests/test_conversations_message_delete.py::test_peer_message_delete_button_absent`
3. [app] A mail message offers neither, because mail cannot carry them — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reactions.py::test_capability_gate_no_button_on_smtp_thread`
4. [app] Tapping a reaction you gave takes it back — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_tapping_your_own_reaction_takes_it_back`
5. [app] Beyond the six quick reactions, a fuller picker lets you react with any emoji — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_the_fuller_picker_reacts_with_an_emoji_outside_the_quick_set`
6. [app] A deleted message's tombstone and a reaction pill are still there after you quit and reopen the app — `docs/goal/ui/conversations.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_tombstone_and_reactions_survive_restart.py::test_tombstone_and_reaction_pill_survive_restart`
7. [app] Nobody else can make your message disappear: a delete claiming to be yours that someone else sent is ignored on every copy — `docs/goal/behavior/message-reactions.md` § Reactions & message delete
   - `tests/e2e-unified/tests/test_conversations_forged_delete.py::test_forged_delete_of_your_message_is_ignored`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5 docker |
| linux | ✅ full | 0.1.5-dev+8b64c820 standalone |
| windows | ⚠ partial | |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+79c80402 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+2f2257c1 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_reaction_option_elements_present_in_flyout` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_react_cross_member_peer_sees_pill` | web (linux): passed, linux (linux): passed, tui (linux): error |
| 1 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_native_peer_reaction_shows_pill_on_senders_copy` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_message_delete.py::test_delete_own_message_shows_tombstone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_message_delete.py::test_peer_message_delete_button_absent` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_reactions.py::test_capability_gate_no_button_on_smtp_thread` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_tapping_your_own_reaction_takes_it_back` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py::test_the_fuller_picker_reacts_with_an_emoji_outside_the_quick_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_conversations_tombstone_and_reactions_survive_restart.py::test_tombstone_and_reaction_pill_survive_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_forged_delete.py::test_forged_delete_of_your_message_is_ignored` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
