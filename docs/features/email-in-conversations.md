---
slug: email-in-conversations
title: Email, in the same conversations
section: everyday
goal: docs/goal/behavior/mail-app-surface.md § First-party client send
guide: docs/guides/own-your-mail.md § Mail in the Fauna app
---

## What a user gets

Once mail is on, email lives in Conversations next to everything else. Write to
any address and it goes out from your own domain; replies and new mail arrive
decrypted in the same list, HTML mail renders cleanly, and pictures loaded from the
internet stay blocked until you choose. What you sent from another mail app shows
here too, and nothing you sent disappears when you restart.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A message to an outside address, typed and sent from the app, is delivered — `docs/goal/behavior/mail-app-surface.md` § First-party client send
   - `tests/e2e-unified/tests/test_mail_client_send.py::test_client_driven_send_relays_to_external_mx`
   - `tests/e2e-unified/tests/test_mail_client_send.py::test_client_send_without_committing_chip_still_relays`
   - `tests/e2e-unified/tests/test_tui_mail_outbound_from.py::test_tui_outbound_from_is_handle_at_handle_domain`
2. [app] Mail from outside arrives in Conversations, readable only by you, whatever its size — `docs/goal/behavior/mail-app-surface.md` § Inbound client receive
   - `tests/e2e-unified/tests/test_mail_client_receive.py::test_client_driven_receive_renders_decrypted`
   - `tests/e2e-unified/tests/test_mail_client_receive_over_frame_reference.py::test_client_driven_receive_renders_over_frame_reference_body`
3. [app] A reply to your mail lands in the same conversation — `docs/goal/behavior/mail-app-surface.md` § Inbound client receive
   - `tests/e2e-unified/tests/test_mail_client_reply_roundtrip.py::test_client_send_then_stub_autoreply_renders_decrypted`
4. [app] From a brand-new nest, turning mail on and exchanging mail works entirely from the app — `docs/goal/behavior/mail-app-surface.md` § First-party client send
   - `tests/e2e-unified/tests/test_mail_client_full_roundtrip.py::test_full_client_ui_mail_roundtrip`
   - `tests/e2e-unified/tests/test_mail_enable_live_nest.py::test_believable_live_mail_roundtrip`
   - `tests/e2e-unified/tests/test_mail_zero_cheat_live.py::test_live_mail_receive_ui_only`
5. [app] Mail you sent is still there after a restart, and mail you sent from another mail app shows as sent here — `docs/goal/behavior/mail-app-surface.md` § First-party client send
   - `tests/e2e-unified/tests/test_mail_sent_copy_restart.py::test_first_party_sent_copy_survives_client_restart`
   - `tests/e2e-unified/tests/test_mail_sent_copy_restart_web.py::test_first_party_sent_copy_survives_web_reload`
   - `tests/e2e-unified/tests/test_mail_sent_feed.py::test_external_mua_submission_surfaces_in_conversations_sent`
6. [app] HTML mail renders as formatted text, and what you write goes out readable by any mail app — `docs/goal/behavior/html-mail.md` § Rendering
   - `tests/e2e-unified/tests/test_mail_html_roundtrip.py::test_html_mail_markdown_roundtrip`
7. [app] A picture in a mail loaded from the internet stays blocked until you load it — `docs/goal/behavior/html-mail.md` § Security & privacy
   - `tests/e2e-unified/tests/test_mail_html_roundtrip.py::test_html_mail_remote_image_blocked_until_reveal`
   - `tests/e2e-unified/tests/test_conversations_remote_image.py::test_remote_image_blocked_until_reveal`
8. [app] A message too large to send is refused with a message before it leaves — `docs/goal/behavior/smtp-server.md` § Message size limits
   - `tests/e2e-unified/tests/test_mail_client_send.py::test_client_send_over_inline_ceiling_refused_with_localized_display_text`
9. [app] A message the app can no longer unlock never holds up the mail after it, and the app says how many it skipped — `docs/goal/behavior/mail-app-surface.md` § Inbound client receive
   - `tests/e2e-unified/tests/test_mail_client_receive_after_reenable.py::test_mailbox_keeps_receiving_past_a_record_the_reenabled_keys_cannot_open`
10. [app] A send refused because you reached your sending limit says so, instead of failing silently — `docs/goal/behavior/mail-app-surface.md` § Outbound metering
   - `tests/e2e-unified/tests/test_mail_client_send_refusals.py::test_send_at_the_sending_limit_is_refused_with_a_message_saying_so`
11. [app] Mail you received before rotating your mail keys stays readable afterwards — `docs/goal/behavior/mail-app-surface.md` § Inbound client receive
   - `tests/e2e-unified/tests/test_mail_client_receive_after_rotation.py::test_mail_received_before_a_key_rotation_stays_readable_after_it`
12. [app] Before your account has an address of its own, a send is refused on the spot with a message saying why — `docs/goal/behavior/mail-app-surface.md` § First-party client send
   - `tests/e2e-unified/tests/test_mail_client_send_refusals.py::test_send_before_the_account_has_an_address_is_refused_on_the_spot`
13. [app] Mail filed as spam never shows in Conversations — `docs/goal/behavior/mail-app-surface.md` § Inbound client receive
   - `tests/e2e-unified/tests/test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk`
   - `tests/e2e-unified/tests/test_mail_junk_absent_from_conversations.py::test_mail_filed_as_junk_on_arrival_never_shows_in_conversations`
14. [app] Mail you have not opened stays marked unread across a restart, including mail that arrived while the app was closed — `docs/goal/behavior/conversation-read-state.md` § Mail: `\Seen` is the marker
   - `tests/e2e-unified/tests/test_mail_read_state.py::test_a_mail_nobody_opened_is_still_unread_after_a_restart`
15. [app] Mail you read on one device shows as read on your other devices without a restart, and stays read — `docs/goal/behavior/conversation-read-state.md` § Mail: `\Seen` is the marker
   - `tests/e2e-unified/tests/test_mail_read_state.py::test_a_mail_read_on_one_device_reads_on_the_other_and_stays_read`
16. [app] A message held back by the warm-up says, where you read your sent mail, when it will be delivered — `docs/goal/behavior/mail-deliverability.md` § Enforcement at submission time
   - (none)
17. [app] On a message you received you can see its malware and spam verdict, and never anyone else's — `docs/goal/behavior/mail-content-scanning.md` § User sees their own message's row
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_client_send.py::test_client_driven_send_relays_to_external_mx` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_client_send.py::test_client_send_without_committing_chip_still_relays` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tui_mail_outbound_from.py::test_tui_outbound_from_is_handle_at_handle_domain` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_client_receive.py::test_client_driven_receive_renders_decrypted` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_client_receive_over_frame_reference.py::test_client_driven_receive_renders_over_frame_reference_body` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_client_reply_roundtrip.py::test_client_send_then_stub_autoreply_renders_decrypted` | web (linux): failed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): skipped |
| 4 | app | `tests/e2e-unified/tests/test_mail_client_full_roundtrip.py::test_full_client_ui_mail_roundtrip` | web (linux): failed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): skipped |
| 4 | app | `tests/e2e-unified/tests/test_mail_enable_live_nest.py::test_believable_live_mail_roundtrip` | — |
| 4 | app | `tests/e2e-unified/tests/test_mail_zero_cheat_live.py::test_live_mail_receive_ui_only` | — |
| 5 | app | `tests/e2e-unified/tests/test_mail_sent_copy_restart.py::test_first_party_sent_copy_survives_client_restart` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_sent_copy_restart_web.py::test_first_party_sent_copy_survives_web_reload` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_sent_feed.py::test_external_mua_submission_surfaces_in_conversations_sent` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_mail_html_roundtrip.py::test_html_mail_markdown_roundtrip` | web (linux): skipped, web (windows): skipped, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): skipped, tui (windows): skipped |
| 7 | app | `tests/e2e-unified/tests/test_mail_html_roundtrip.py::test_html_mail_remote_image_blocked_until_reveal` | web (linux): skipped, web (windows): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): skipped, tui (windows): skipped |
| 7 | app | `tests/e2e-unified/tests/test_conversations_remote_image.py::test_remote_image_blocked_until_reveal` | web (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_mail_client_send.py::test_client_send_over_inline_ceiling_refused_with_localized_display_text` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_client_receive_after_reenable.py::test_mailbox_keeps_receiving_past_a_record_the_reenabled_keys_cannot_open` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_client_send_refusals.py::test_send_at_the_sending_limit_is_refused_with_a_message_saying_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_client_receive_after_rotation.py::test_mail_received_before_a_key_rotation_stays_readable_after_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_client_send_refusals.py::test_send_before_the_account_has_an_address_is_refused_on_the_spot` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_junk_absent_from_conversations.py::test_mail_filed_as_junk_on_arrival_never_shows_in_conversations` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_mail_read_state.py::test_a_mail_nobody_opened_is_still_unread_after_a_restart` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_mail_read_state.py::test_a_mail_read_on_one_device_reads_on_the_other_and_stays_read` | linux (linux): passed, tui (linux): passed |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
<!-- features-render:end -->
