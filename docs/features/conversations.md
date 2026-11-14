---
slug: conversations
title: Private conversations
section: everyday
goal: docs/goal/ui/conversations.md § Goal
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Message anyone by handle, on your nest or another. Conversations between people
on Fauna are end-to-end encrypted by default: your nest relays encrypted
messages it cannot read, unless a group chooses to be a community room, which
its home nest can read. Every bubble shows who wrote it and when, your history
is there after a restart and on your other devices, and a message that could not
be sent tells you so.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] You start a conversation by picking a person, and picking two starts a group — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_messaging.py::test_new_conversation_compose_opens`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_resolves_fauna_handle`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_multi_recipient_creates_group`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_typed_prefix_suggests_known_contact`
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_empty_input_has_no_suggestions`
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
14. [app] When you disconnect the app that carried such a conversation, the conversation stays readable and says the app is no longer connected, you can no longer write into it, and a reply still waiting to go out is marked not delivered, its text kept — `docs/goal/ui/conversations.md` § Element IDs
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_revoked_bridges_room_says_so_and_its_undelivered_reply_too`
15. [app] A message that arrived while the app was closed raises no notification when you next start it; the unread mark carries it instead — `docs/goal/behavior/message-banners.md` § Where logic lives
   - `tests/e2e-unified/tests/test_conversations_read_state_across_restart.py::test_a_message_that_arrived_while_the_app_was_closed_raises_no_banner`
16. [app] A message you sent from another of your devices raises no notification — `docs/goal/behavior/message-banners.md` § Where logic lives
   - `tests/e2e-unified/tests/test_conversations_across_own_devices.py::test_a_message_sent_from_another_of_your_devices_raises_no_banner`
17. [app] A conversation you read on one of your devices shows as read on your others — `docs/goal/behavior/conversation-read-state.md` § Goal
   - `tests/e2e-unified/tests/test_conversations_across_own_devices.py::test_a_conversation_read_on_one_device_shows_read_on_the_other`
18. [app] When new messages stop arriving because the app's receiving loop failed, the Conversations page says so on its error line until a working receive loop replaces it; only the notice that your account is open elsewhere outranks it — `docs/goal/ui/conversations.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_conversations_receive_stopped.py::test_a_stopped_receive_loop_says_so_until_a_newer_loop_replaces_it`
19. [app] When you pick someone on your own nest who has not opened Fauna yet, the picker says they are not reachable yet instead of offering plain email, and once they have signed in you can write to them — `docs/goal/ui/conversations.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_recipient_picker.py::test_home_nest_account_without_key_packages_reads_not_reachable`
20. [nest] A conversation an app carries from another network belongs to that network's name: when you approve the app again under a different name, the old conversations stay readable but disconnected, and nothing can be sent into them — `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes)
   - `tests/e2e-unified/tests/api/test_bridge_reconsent_moves_bridge_id.py::test_a_reconsent_under_another_bridge_id_leaves_the_old_rooms_readable_and_refused`
21. [app] Signing out or switching identity raises no notification for the conversations that load for the incoming identity — `docs/goal/behavior/message-banners.md` § Where logic lives
   - `tests/e2e-unified/tests/test_conversations_message_banner.py::test_switching_identity_raises_no_banner_for_the_incoming_identitys_threads`
22. [app] A conversation you join on one of your devices appears on your other devices while they run, and one you were just added to is still there after you quit at once and relaunch — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_conversations_join_reaches_running_device.py::test_a_conversation_joined_on_one_device_appears_on_the_other_while_it_runs`
   - `tests/e2e-unified/tests/test_conversations_join_reaches_running_device.py::test_a_conversation_you_were_just_added_to_survives_an_immediate_quit`
23. [app] If your nest cannot be reached when the app starts, your conversations still follow you to this device once it is back, with no relaunch — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_conversations_nest_unreachable_at_launch.py::test_a_device_whose_nest_was_unreachable_at_launch_gets_the_conversations_once_it_is_back`
24. [app] Reactions and deletions made on two of your devices at the same time all survive: nothing one device did is lost when the other saves — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_conversations_two_device_reaction_delete_merge.py::test_a_reaction_and_a_deletion_made_on_two_devices_at_once_both_survive`
25. [app] Unread is decided by when your nest received a message, never by the sender's clock: a backdated message still shows unread, and nothing reads unread by mistake after a restart — `docs/goal/behavior/conversation-read-state.md` § The read-marker record
   - `tests/e2e-unified/tests/test_conversations_unread_by_nest_position.py::test_unread_follows_the_nests_order_never_the_senders_clock`
26. [app] A device enrolled after you left a conversation starts it unread when you are brought back in, while a device that kept it resumes at your old read position — `docs/goal/behavior/conversation-read-state.md` § The read-marker record
   - `tests/e2e-unified/tests/test_conversations_rejoin_read_position.py::test_a_group_left_and_re_joined_opens_at_the_old_read_position`
   - `tests/e2e-unified/tests/test_conversations_rejoin_later_enrolled_device.py::test_a_device_enrolled_after_you_left_starts_the_rejoined_conversation_unread`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| linux | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| windows | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+390d61b5 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_messaging.py::test_new_conversation_compose_opens` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_resolves_fauna_handle` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_multi_recipient_creates_group` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_typed_prefix_suggests_known_contact` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_empty_input_has_no_suggestions` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_new_thread_keeps_threads_reachable.py::test_new_conversation_keeps_existing_threads_reachable` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_real_roundtrip.py::test_fauna_mls_real_roundtrip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_two_client_inbox_drain.py::test_fauna_mls_two_client_inbox_drain` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_web_receive.py::test_fauna_mls_web_receive` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_fauna_mls_web_receives_from_linux_sender.py::test_fauna_mls_web_receives_from_linux_sender` | web (linux): passed, linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_fauna_mls_cross_nest_roundtrip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_receive.py::test_fauna_mls_cross_nest_receive` — requires `cross-nest-mls-receive-seam` (test harness) (not built: windows, android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_conv_rail_push_wakes_native.py::test_conv_rail_push_wakes_native` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_conv_rail_push_wakes_web.py::test_conv_rail_push_wakes_web` | web (linux): passed, linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_history_survives_app_restart.py::test_conversations_history_survives_app_restart` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_own_message_survives_an_immediate_restart.py::test_conversations_own_message_survives_an_immediate_restart` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_history_survives_restart_cross_app.py::test_conversations_history_survives_restart_cross_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_device_sync.py::test_fauna_mls_cross_device_sync` — requires `mls-cross-device-sync-proof` (test harness) (not built: web) | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_cross_device_sync` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_fauna_mls_web_cross_device_sync.py::test_fauna_mls_web_concurrent_tabs` | web (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_dm_sender.py::test_bubble_shows_sender_label` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_bubble_timestamp.py::test_bubble_shows_shared_timestamp` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_send_surfaces_on_error_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_compose_error.py::test_failed_membership_op_surfaces_on_error_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_dm_roundtrip` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_cross_nest_api.py::test_cross_nest_welcome_delivery` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_mls_replica_sync.py::test_replica_round_trip_across_devices` | nest (linux): passed, nest (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_known_peer_unreachable_never_downgrades_to_email` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_nest_roundtrip.py::test_first_contact_with_unreachable_authority_is_email` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversations_message_banner.py::test_new_message_raises_a_banner_except_in_the_open_thread` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_conversations_read_state_across_restart.py::test_a_native_message_sent_while_the_app_was_closed_is_unread_at_launch` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_revoked_bridges_room_says_so_and_its_undelivered_reply_too` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_conversations_read_state_across_restart.py::test_a_message_that_arrived_while_the_app_was_closed_raises_no_banner` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_conversations_across_own_devices.py::test_a_message_sent_from_another_of_your_devices_raises_no_banner` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_conversations_across_own_devices.py::test_a_conversation_read_on_one_device_shows_read_on_the_other` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_conversations_receive_stopped.py::test_a_stopped_receive_loop_says_so_until_a_newer_loop_replaces_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 19 | app | `tests/e2e-unified/tests/test_recipient_picker.py::test_home_nest_account_without_key_packages_reads_not_reachable` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_bridge_reconsent_moves_bridge_id.py::test_a_reconsent_under_another_bridge_id_leaves_the_old_rooms_readable_and_refused` | nest (linux): passed, nest (macos): passed |
| 21 | app | `tests/e2e-unified/tests/test_conversations_message_banner.py::test_switching_identity_raises_no_banner_for_the_incoming_identitys_threads` | tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_conversations_join_reaches_running_device.py::test_a_conversation_joined_on_one_device_appears_on_the_other_while_it_runs` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_conversations_join_reaches_running_device.py::test_a_conversation_you_were_just_added_to_survives_an_immediate_quit` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 23 | app | `tests/e2e-unified/tests/test_conversations_nest_unreachable_at_launch.py::test_a_device_whose_nest_was_unreachable_at_launch_gets_the_conversations_once_it_is_back` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 24 | app | `tests/e2e-unified/tests/test_conversations_two_device_reaction_delete_merge.py::test_a_reaction_and_a_deletion_made_on_two_devices_at_once_both_survive` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 25 | app | `tests/e2e-unified/tests/test_conversations_unread_by_nest_position.py::test_unread_follows_the_nests_order_never_the_senders_clock` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 26 | app | `tests/e2e-unified/tests/test_conversations_rejoin_read_position.py::test_a_group_left_and_re_joined_opens_at_the_old_read_position` | windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 26 | app | `tests/e2e-unified/tests/test_conversations_rejoin_later_enrolled_device.py::test_a_device_enrolled_after_you_left_starts_the_rejoined_conversation_unread` | macos (macos): passed, tui (linux): passed |
<!-- features-render:end -->
