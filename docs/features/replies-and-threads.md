---
slug: replies-and-threads
title: Replies, recipients and threads
section: everyday
goal: docs/goal/ui/conversations.md § Participants vs. reply recipients
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Replying quotes the message you answer. On a mail conversation you choose who
the reply goes to, reply-all included; on an encrypted conversation the group is the
audience. Mail with a subject threads by subject, and a change of subject shows as a
divider inside the thread.

## Coverage contract

Stamped 2026-09-19 at e9152e3330.

1. [app] A reply shows a quote of the message it answers — `docs/goal/architecture/render-model.md` § D2 — Embeds are first-class blocks, not sibling fields
   - `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_reply_shows_quote_of_parent`
   - `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_non_reply_has_no_quote`
   - `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_missing_parent_hides_quote`
2. [app] On a mail conversation you edit who the reply goes to, with reply-all; on an encrypted one the group is the audience — `docs/goal/ui/conversations.md` § Participants vs. reply recipients
   - `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_reply_seeds_to_line_with_sender_and_chip_removes`
   - `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_mail_thread_offers_reply_all`
   - `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_fauna_thread_hides_to_line_and_reply_all`
3. [app] Mail threads by subject, a Re: prefix joins the thread, and a changed subject shows a divider — `docs/goal/ui/conversations.md` § State & data shape
   - `tests/e2e-unified/tests/test_keying.py::test_smtp_subject_routes_to_subject_keyed`
   - `tests/e2e-unified/tests/test_keying.py::test_re_prefix_normalized`
   - `tests/e2e-unified/tests/test_keying.py::test_smtp_in_reply_to_overrides_subject`
   - `tests/e2e-unified/tests/test_subject_divider.py::test_divider_appears_on_subject_change`
4. [app] A reply in progress shows the message you are answering, and you can cancel it — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_conversations_reply_chrome.py::test_reply_preview_shows_the_message_answered_and_cancel_clears_it`
5. [app] On a mail conversation the people in the header are informational: choosing one shows the full address, and nobody can be added or removed — `docs/goal/ui/conversations.md` § Participants vs. reply recipients
   - `tests/e2e-unified/tests/test_conversations_reply_chrome.py::test_mail_header_chips_are_informational`
6. [app] You can turn on a subject line for a message and type one — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_mail_client_send.py::test_client_driven_send_relays_to_external_mx`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | 0.1.3-dev+8b195137 standalone |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_reply_shows_quote_of_parent` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_non_reply_has_no_quote` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_reply_quote.py::test_missing_parent_hides_quote` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_reply_seeds_to_line_with_sender_and_chip_removes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_mail_thread_offers_reply_all` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_reply_recipients.py::test_fauna_thread_hides_to_line_and_reply_all` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_keying.py::test_smtp_subject_routes_to_subject_keyed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_keying.py::test_re_prefix_normalized` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_keying.py::test_smtp_in_reply_to_overrides_subject` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_subject_divider.py::test_divider_appears_on_subject_change` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_reply_chrome.py::test_reply_preview_shows_the_message_answered_and_cancel_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_reply_chrome.py::test_mail_header_chips_are_informational` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_mail_client_send.py::test_client_driven_send_relays_to_external_mx` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
