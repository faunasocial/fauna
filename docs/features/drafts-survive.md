---
slug: drafts-survive
title: Unfinished posts, messages and events survive a restart
section: everyday
goal: docs/goal/behavior/reserved-folders.md § Drafts Sync
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Half-written text is never lost: a post, a message or a calendar event you were
in the middle of is there again after you quit and reopen the app, and on your other
devices too. Cancelling a new conversation discards its draft on purpose. A file
you attached to an unfinished post comes back by name, and posting it asks you to
attach the file again rather than posting without it.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] A half-written post is there after restarting the app — `docs/goal/ui/feed.md` § Persistence
   - `tests/e2e-unified/tests/test_feed_draft_persistence.py::test_feed_compose_draft_survives_app_restart`
2. [app] A half-written message is there after restarting the app — `docs/goal/ui/conversations.md` § Persistence
   - `tests/e2e-unified/tests/test_conversations_draft_persistence.py::test_new_thread_compose_draft_survives_app_restart`
3. [app] A half-written event is there after restarting the app — `docs/goal/ui/events.md` § Persistence
   - `tests/e2e-unified/tests/test_event_draft_persistence.py::test_event_compose_draft_survives_app_restart`
4. [app] Cancelling a new conversation discards its draft — `docs/goal/ui/conversations.md` § User actions
   - `tests/e2e-unified/tests/test_conversations_new_thread_cancel.py::test_new_thread_cancel_discards_draft_and_dismisses`
5. [app] Leaving the app — closing its window, switching away on your phone, quitting — saves what you were typing first, without waiting for the autosave — `docs/goal/behavior/reserved-folders.md` § The leave-flush promise
   - `tests/e2e-unified/tests/test_windows_window_close_flush.py::test_window_close_forces_conversation_draft_flush_before_exit`
   - `tests/e2e-unified/tests/test_linux_window_close_flush.py::test_window_close_forces_conversation_draft_flush_before_exit`
   - `tests/e2e-unified/tests/test_tui_quit_flush.py::test_exit_tab_forces_conversation_draft_flush_before_exit`
   - `tests/e2e-unified/tests/test_web_leave_flush.py::test_pagehide_forces_conversation_draft_flush`
   - `tests/e2e-unified/tests/test_apple_leave_flush.py::test_quit_forces_conversation_draft_flush_before_exit`
   - `tests/e2e-unified/tests/test_apple_leave_flush.py::test_background_forces_conversation_draft_flush`
6. [nest] Drafts reach your other devices through your nest, sealed — `docs/goal/behavior/reserved-folders.md` § Drafts Sync
   - `tests/e2e-unified/tests/api/test_drafts_sync.py::test_drafts_round_trip_across_devices`
   - `tests/e2e-unified/tests/api/test_drafts_sync.py::test_drafts_reserved_set_not_user_listed`
7. [app] A file attached to a half-written post is still named after restarting the app, and posting asks to attach it again rather than posting without it — `docs/goal/ui/feed.md` § Persistence
   - `tests/e2e-unified/tests/test_feed_draft_attachment_restore.py::test_restored_draft_attachment_is_named_refused_and_removable`
8. [app] A file attached to a half-written message is still named after restarting the app, and sending refuses by name rather than sending without it — `docs/goal/ui/conversations.md` § Persistence
   - `tests/e2e-unified/tests/test_conversations_draft_attachment_restore.py::test_restored_message_draft_attachment_is_named_and_its_send_refuses`
9. [app] The audience picked for a half-written post — a tier, a room, or a sale with its price and teaser — is still picked after restarting the app — `docs/goal/ui/feed.md` § Persistence
   - `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_sale_with_its_price_and_teaser_survives_a_restart`
   - `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_tier_and_teaser_survive_a_restart`
   - `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_room_and_teaser_survive_a_restart`
10. [app] A half-written new message is kept when you switch to another conversation or step back out of the new-conversation composer — `docs/goal/ui/conversations.md` § Persistence
   - `tests/e2e-unified/tests/test_conversations_new_thread_draft_kept.py::test_a_half_written_new_message_is_kept_across_stepping_out_and_switching`
11. [app] A half-written event is still in the form when you leave the page or close the form and open it again, without restarting the app — `docs/goal/behavior/reserved-folders.md` § Drafts Sync
   - `tests/e2e-unified/tests/test_event_draft_openers.py::test_a_half_written_event_is_still_in_the_form_each_time_it_is_opened_again`
12. [app] Starting an event from a day on the calendar opens an empty form at that day rather than your saved draft, and once an event is created or discarded the next new-event form opens blank — `docs/goal/ui/events.md` § Persistence
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_draft_persistence.py::test_feed_compose_draft_survives_app_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_draft_persistence.py::test_new_thread_compose_draft_survives_app_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_event_draft_persistence.py::test_event_compose_draft_survives_app_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 4 | app | `tests/e2e-unified/tests/test_conversations_new_thread_cancel.py::test_new_thread_cancel_discards_draft_and_dismisses` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_windows_window_close_flush.py::test_window_close_forces_conversation_draft_flush_before_exit` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_linux_window_close_flush.py::test_window_close_forces_conversation_draft_flush_before_exit` | linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_tui_quit_flush.py::test_exit_tab_forces_conversation_draft_flush_before_exit` | tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_web_leave_flush.py::test_pagehide_forces_conversation_draft_flush` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_apple_leave_flush.py::test_quit_forces_conversation_draft_flush_before_exit` | macos (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_apple_leave_flush.py::test_background_forces_conversation_draft_flush` | ios (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_drafts_sync.py::test_drafts_round_trip_across_devices` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_drafts_sync.py::test_drafts_reserved_set_not_user_listed` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed_draft_attachment_restore.py::test_restored_draft_attachment_is_named_refused_and_removable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_conversations_draft_attachment_restore.py::test_restored_message_draft_attachment_is_named_and_its_send_refuses` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_sale_with_its_price_and_teaser_survives_a_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_tier_and_teaser_survive_a_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_feed_draft_audience_restore.py::test_a_post_drafts_room_and_teaser_survive_a_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_conversations_new_thread_draft_kept.py::test_a_half_written_new_message_is_kept_across_stepping_out_and_switching` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_event_draft_openers.py::test_a_half_written_event_is_still_in_the_form_each_time_it_is_opened_again` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | (none) | — |
<!-- features-render:end -->
