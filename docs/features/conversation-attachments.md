---
slug: conversation-attachments
title: Files in conversations
section: everyday
goal: docs/goal/ui/conversations.md § User actions
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Attach a file to a message: the chip shows its name and size before you send,
you can remove it, and the other side sees a picture inline or a file to open. A file
too large for a message is refused with a message rather than silently dropped.

## Coverage contract

Stamped 2026-09-19 at e9152e3330.

1. [app] A picture or file someone sends you shows in the bubble — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_conversations_attachments.py::test_inbound_image_and_file_attachments_render`
2. [app] A file you attach shows as a chip with its name and size, and can be removed before sending — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_conversations_compose_attachments.py::test_staged_attachment_shows_a_chip_and_can_be_removed`
   - `tests/e2e-unified/tests/test_conversations_compose_attachments.py::test_staged_attachment_chip_shows_its_size`
3. [app] A file you send is delivered and shows in your own copy of the message — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_web_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_web_real_faunamls_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_linux_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_tui_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_real_send_with_attachment_renders_in_sender_echo`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_real_send_with_attachment_renders_in_sender_echo`
4. [app] A file too large to send is refused with a message — `docs/goal/ui/conversations.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_real_faunamls_send_with_oversized_attachment_surfaces_on_error_message`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_real_send_with_oversized_attachment_surfaces_on_error_message`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_real_send_with_oversized_attachment_surfaces_on_error_message`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_real_send_with_oversized_attachment_surfaces_on_error_message`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_real_send_with_oversized_attachment_surfaces_on_error_message`
5. [app] A file you sent is still there after the nest's storage clean-up runs — `docs/goal/behavior/backup-restore.md` § 9. Garbage Collection
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_real_faunamls_conversation_attachment_survives_a_blob_gc_sweep`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_conversation_attachment_survives_a_blob_gc_sweep`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_conversation_attachment_survives_a_blob_gc_sweep`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_conversation_attachment_survives_a_blob_gc_sweep`
   - `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_conversation_attachment_survives_a_blob_gc_sweep`
6. [app] The attach button is unavailable on a conversation whose network cannot carry files — `docs/goal/ui/conversations.md` § State & data shape
   - `tests/e2e-unified/tests/test_capability_gating.py::test_bridged_thread_disables_attachment_and_topic`
   - `tests/e2e-unified/tests/test_capability_gating.py::test_fauna_oneonone_enables_all_compose_affordances`
7. [app] A file sent to an outside mail address arrives as an ordinary attachment, and a file on mail from outside shows in the bubble — `docs/goal/ui/conversations.md` § Attachments — implementation status today
   - `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_a_file_sent_to_an_outside_address_arrives_as_an_ordinary_attachment`
   - `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_a_file_on_mail_from_outside_shows_in_the_bubble`
8. [app] A file in an older message still shows after this device has dropped its cached copy, and on a device restored from your history — `docs/goal/ui/conversations.md` § Attachments — implementation status today
   - `tests/e2e-unified/tests/test_conversations_attachment_retention.py::test_an_attachment_this_device_dropped_is_fetched_again`
   - `tests/e2e-unified/tests/test_fauna_mls_cross_device_sync.py::test_an_attachment_restores_onto_a_second_device`
9. [app] A file this device cannot fetch or open still shows its name and size — `docs/goal/ui/conversations.md` § Attachments — implementation status today
   - `tests/e2e-unified/tests/test_conversations_attachment_retention.py::test_a_file_this_device_cannot_fetch_still_shows_its_name_and_size`
10. [app] Camera and location details are removed from a file before it is sent — `docs/goal/ui/conversations.md` § Attachments — implementation status today
   - `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_camera_and_location_details_are_removed_from_a_file_before_it_is_sent`
11. [app] A received picture that carries content credentials shows a provenance badge — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_conversations_attachment_c2pa.py::test_a_received_signed_picture_shows_a_provenance_badge`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_attachments.py::test_inbound_image_and_file_attachments_render` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_compose_attachments.py::test_staged_attachment_shows_a_chip_and_can_be_removed` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_compose_attachments.py::test_staged_attachment_chip_shows_its_size` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_web_real_send_with_attachment_renders_in_sender_echo` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_web_real_faunamls_send_with_attachment_renders_in_sender_echo` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_linux_real_send_with_attachment_renders_in_sender_echo` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_tui_real_send_with_attachment_renders_in_sender_echo` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_real_send_with_attachment_renders_in_sender_echo` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_real_send_with_attachment_renders_in_sender_echo` | macos (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_real_send_with_attachment_renders_in_sender_echo` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_real_send_with_attachment_renders_in_sender_echo` | — |
| 4 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_real_faunamls_send_with_oversized_attachment_surfaces_on_error_message` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_real_send_with_oversized_attachment_surfaces_on_error_message` | windows (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_real_send_with_oversized_attachment_surfaces_on_error_message` | macos (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_real_send_with_oversized_attachment_surfaces_on_error_message` | ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_real_send_with_oversized_attachment_surfaces_on_error_message` | — |
| 5 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_real_faunamls_conversation_attachment_survives_a_blob_gc_sweep` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_windows_conversation_attachment_survives_a_blob_gc_sweep` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_macos_conversation_attachment_survives_a_blob_gc_sweep` | macos (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_ios_conversation_attachment_survives_a_blob_gc_sweep` | ios (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_attachments_outbound.py::test_android_conversation_attachment_survives_a_blob_gc_sweep` | — |
| 6 | app | `tests/e2e-unified/tests/test_capability_gating.py::test_bridged_thread_disables_attachment_and_topic` | linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_capability_gating.py::test_fauna_oneonone_enables_all_compose_affordances` | linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_a_file_sent_to_an_outside_address_arrives_as_an_ordinary_attachment` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_a_file_on_mail_from_outside_shows_in_the_bubble` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_attachment_retention.py::test_an_attachment_this_device_dropped_is_fetched_again` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_fauna_mls_cross_device_sync.py::test_an_attachment_restores_onto_a_second_device` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_conversations_attachment_retention.py::test_a_file_this_device_cannot_fetch_still_shows_its_name_and_size` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_conversations_mail_attachments.py::test_camera_and_location_details_are_removed_from_a_file_before_it_is_sent` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_conversations_attachment_c2pa.py::test_a_received_signed_picture_shows_a_provenance_badge` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
<!-- features-render:end -->
