---
slug: conversations
title: Private conversations
section: everyday
goal: docs/goal/ui/conversations.md § Goal
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Message anyone by handle, on your nest or another. Conversations are end-to-end
encrypted: your nest relays sealed messages it cannot read. Every bubble shows who
wrote it and when, your history is there after a restart and on your other devices,
and a message that could not be sent tells you so.

## Coverage contract

Stamped 2026-08-31 at 4a2dfda4dc.

1. [app] You start a conversation by picking a person, and picking two starts a group — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_messaging.py::test_new_conversation_compose_opens`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_resolves_fauna_handle`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_multi_recipient_creates_group`
   - `tests/e2e-unified/tests/test_conversations_new_thread_keeps_threads_reachable.py::test_new_conversation_keeps_existing_threads_reachable`
2. [app] A message you send is delivered end-to-end encrypted and the other person's app shows it — `docs/goal/behavior/direct-messages.md` § User Experience
   - `tests/e2e-unified/tests/test_fauna_mls_real_roundtrip.py::test_fauna_mls_real_roundtrip`
   - `tests/e2e-unified/tests/test_fauna_mls_two_client_inbox_drain.py::test_fauna_mls_two_client_inbox_drain`
   - `tests/e2e-unified/tests/test_fauna_mls_web_receive.py::test_fauna_mls_web_receive`
   - `tests/e2e-unified/tests/test_fauna_mls_web_receives_from_linux_sender.py::test_fauna_mls_web_receives_from_linux_sender`
3. [app] A conversation with someone on another nest works the same way — `docs/goal/behavior/direct-messages.md` § Technical Flow — Cross-Nest
   - `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_fauna_mls_cross_nest_roundtrip`
   - `tests/e2e-unified/tests/test_fauna_mls_cross_nest_receive.py::test_fauna_mls_cross_nest_receive`
4. [app] A new message arrives while you watch, without a reload — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_conv_rail_push_wakes_native.py::test_conv_rail_push_wakes_native`
   - `tests/e2e-unified/tests/test_conv_rail_push_wakes_web.py::test_conv_rail_push_wakes_web`
5. [app] Your history is still there after restarting the app, including a message sent a moment before — `docs/goal/ui/conversations.md` § Persistence
   - `tests/e2e-unified/tests/test_conversations_history_survives_app_restart.py::test_conversations_history_survives_app_restart`
   - `tests/e2e-unified/tests/test_conversations_own_message_survives_an_immediate_restart.py::test_conversations_own_message_survives_an_immediate_restart`
   - `tests/e2e-unified/tests/test_conversations_history_survives_restart_cross_app.py::test_conversations_history_survives_restart_cross_app`
6. [app] Your conversations follow you to your other devices — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_fauna_mls_cross_device_sync.py::test_fauna_mls_cross_device_sync`
   - `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_cross_device_sync`
   - `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_concurrent_tabs`
7. [app] Every bubble shows the sender and the time — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_conversations_dm_sender.py::test_bubble_shows_sender_label`
   - `tests/e2e-unified/tests/test_conversations_bubble_timestamp.py::test_bubble_shows_shared_timestamp`
8. [app] A message that could not be sent says so on the page — `docs/goal/ui/conversations.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_send_surfaces_on_error_message`
   - `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_membership_op_surfaces_on_error_message`
9. [nest] Your nest relays sealed messages, key material and cross-nest invitations without reading them — `docs/goal/behavior/direct-messages.md` § Technical Flow — Same Nest
   - `tests/e2e-unified/tests/api/test_mls_channels.py::test_dm_roundtrip`
   - `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle`
   - `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_welcome_delivery`
   - `tests/e2e-unified/tests/api/test_mls_replica_sync.py::test_replica_round_trip_across_devices`
10. [app] When someone you already talk to over Fauna can't be reached, the picker says so instead of quietly falling back to plain email — and a stranger's address nobody can vouch for still goes as email — `docs/goal/ui/conversations.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_known_peer_unreachable_never_downgrades_to_email`
   - `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_first_contact_with_unreachable_authority_is_email`
11. [app] A new message raises a system notification while the app is running, except in the conversation you already have open — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_conversations_message_banner.py::test_new_message_raises_a_banner_except_in_the_open_thread`
12. [app] A Fauna message that arrived while the app was closed shows as unread the next time you start it — `docs/goal/behavior/conversation-read-state.md` § Goal
   - `tests/e2e-unified/tests/test_conversations_read_state_across_restart.py::test_a_native_message_sent_while_the_app_was_closed_is_unread_at_launch`
13. [app] A conversation that an app you connected carries from another network sits on your Conversations page beside the rest, under that network's own name, and your reply goes back out through that app — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| linux | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| windows | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+c438a386 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_messaging.py::test_new_conversation_compose_opens` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_resolves_fauna_handle` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_multi_recipient_creates_group` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_new_thread_keeps_threads_reachable.py::test_new_conversation_keeps_existing_threads_reachable` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_real_roundtrip.py::test_fauna_mls_real_roundtrip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_two_client_inbox_drain.py::test_fauna_mls_two_client_inbox_drain` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_web_receive.py::test_fauna_mls_web_receive` | web (linux): passed, web (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_web_receives_from_linux_sender.py::test_fauna_mls_web_receives_from_linux_sender` | web (linux): passed, web (windows): error, linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_fauna_mls_cross_nest_roundtrip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_receive.py::test_fauna_mls_cross_nest_receive` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_conv_rail_push_wakes_native.py::test_conv_rail_push_wakes_native` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_conv_rail_push_wakes_web.py::test_conv_rail_push_wakes_web` | web (linux): passed, linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_history_survives_app_restart.py::test_conversations_history_survives_app_restart` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_own_message_survives_an_immediate_restart.py::test_conversations_own_message_survives_an_immediate_restart` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_history_survives_restart_cross_app.py::test_conversations_history_survives_restart_cross_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_device_sync.py::test_fauna_mls_cross_device_sync` | web (linux): skipped, web (windows): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_cross_device_sync` | web (linux): passed, web (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_concurrent_tabs` | web (linux): passed, web (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_dm_sender.py::test_bubble_shows_sender_label` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_bubble_timestamp.py::test_bubble_shows_shared_timestamp` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_send_surfaces_on_error_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_membership_op_surfaces_on_error_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_dm_roundtrip` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_welcome_delivery` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_replica_sync.py::test_replica_round_trip_across_devices` | nest (linux): passed, nest (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_known_peer_unreachable_never_downgrades_to_email` | web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_first_contact_with_unreachable_authority_is_email` | web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversations_message_banner.py::test_new_message_raises_a_banner_except_in_the_open_thread` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_conversations_read_state_across_restart.py::test_a_native_message_sent_while_the_app_was_closed_is_unread_at_launch` | linux (linux): skipped, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
