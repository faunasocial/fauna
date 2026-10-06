---
slug: calendar-and-events
title: Calendars and events
section: everyday
goal: docs/goal/ui/events.md § Goal
guide: docs/guides/app-tour.md § Events
---

## What a user gets

Keep calendars on your nest and see them by month, week, day or as a list.
Create events from a slot, invite people by email, set a reminder, RSVP from the card
or the detail, and hide a calendar without deleting it. Your week starts on the day
your locale says, and an event added from another app appears while you are looking.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] Create and delete events; description, place and time survive to the detail view — `docs/goal/ui/events.md` § User actions
   - `tests/e2e-unified/tests/test_events.py::test_event_create_and_delete`
   - `tests/e2e-unified/tests/test_events.py::test_event_description_location_and_time_reach_the_detail_surface`
   - `tests/e2e-unified/tests/test_events.py::test_event_detail_back_returns_to_the_list`
2. [app] Month, week and day views render your events, and panning moves one visible range — `docs/goal/ui/events.md` § Week & day timeline views
   - `tests/e2e-unified/tests/test_events.py::test_switch_to_month_view`
   - `tests/e2e-unified/tests/test_events.py::test_switch_to_week_view`
   - `tests/e2e-unified/tests/test_events.py::test_switch_to_day_view`
   - `tests/e2e-unified/tests/test_events.py::test_month_navigation`
   - `tests/e2e-unified/tests/test_events.py::test_pan_moves_one_visible_range_per_view`
   - `tests/e2e-unified/tests/test_events.py::test_pan_is_inert_in_the_date_unfiltered_agenda`
3. [app] Clicking a day or an empty slot starts a new event at that date and time — `docs/goal/ui/events.md` § Layout & flow
   - `tests/e2e-unified/tests/test_events.py::test_month_day_cell_click_opens_day_view`
   - `tests/e2e-unified/tests/test_events.py::test_month_double_click_day_cell_opens_new_event_prefilled`
   - `tests/e2e-unified/tests/test_events.py::test_day_view_empty_slot_click_opens_quick_create_prefilled`
   - `tests/e2e-unified/tests/test_events.py::test_week_view_empty_slot_click_opens_quick_create_with_slot_time`
4. [app] RSVP from the detail or straight from the card — `docs/goal/ui/events.md` § User actions
   - `tests/e2e-unified/tests/test_events.py::test_event_rsvp`
   - `tests/e2e-unified/tests/test_events.py::test_event_card_rsvp`
5. [app] Invite someone by email and they join the attendee list — `docs/goal/ui/events.md` § Attendee list presentation
   - `tests/e2e-unified/tests/test_events.py::test_event_invite_attendee`
6. [app] Set, read and remove a reminder — `docs/goal/ui/events.md` § Reminders
   - `tests/e2e-unified/tests/test_events.py::test_event_reminder`
7. [app] Hiding a calendar takes its events out of view and leaves the others — `docs/goal/ui/events.md` § Where logic lives
   - `tests/e2e-unified/tests/test_calendar_visibility.py::test_calendar_visibility_filters_the_displayed_union`
8. [app] The week starts on your locale's first day — `docs/goal/ui/events.md` § Week & day timeline views
   - `tests/e2e-unified/tests/test_events_locale_week_start.py::test_tui_month_grid_opens_on_the_locale_week_start`
   - `tests/e2e-unified/tests/test_events_locale_week_start_linux.py::test_linux_month_grid_opens_on_the_locale_week_start`
   - `tests/e2e-unified/tests/test_events_locale_week_start_web.py::test_web_month_grid_opens_on_the_locale_week_start`
   - `tests/e2e-unified/tests/test_events_locale_week_start_apple.py::test_apple_month_grid_opens_on_the_locale_week_start`
   - `tests/e2e-unified/tests/test_events_locale_week_start_windows.py::test_windows_week_grid_opens_on_the_locale_week_start`
9. [app] A calendar or event added from another app appears while you stay on the page — `docs/goal/ui/events.md` § Where logic lives
   - `tests/e2e-unified/tests/test_caldav_external_appears.py::test_external_caldav_calendar_and_event_appear_while_on_page`
10. [app] An invitation sent to you from another calendar lands on your events page — `docs/goal/behavior/caldav-server.md` § Server-side auto-schedule
   - `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_mailbox_less_attendee_materializes_invite_on_events_page`
11. [app] Creating a calendar adds it to your calendar list, ready to take the next event you make — `docs/goal/ui/events.md` § Layout & flow
   - `tests/e2e-unified/tests/test_calendar_visibility.py::test_calendar_visibility_filters_the_displayed_union`
12. [app] With no calendar picked you see every calendar's events together, and picking one narrows the page to it — `docs/goal/ui/events.md` § Layout & flow
   - `tests/e2e-unified/tests/test_calendar_visibility.py::test_no_calendar_picked_shows_every_calendar_and_picking_one_narrows`
13. [app] A calendar can be imported from a calendar file and exported back out as one — `docs/goal/ui/events.md` § Layout & flow
   - `tests/e2e-unified/tests/test_events.py::test_calendar_exported_to_a_file_imports_into_another_calendar`
   - `tests/e2e-unified/tests/test_events.py::test_calendar_import_with_no_file_chosen_says_so`
14. [app] An all-day event sits in its own band above the day, and events that clash sit side by side — including one that runs past midnight — `docs/goal/ui/events.md` § Layout & flow
   - `tests/e2e-unified/tests/test_events.py::test_day_timeline_bands_all_day_events_and_sets_clashes_side_by_side`
15. [app] Only an event's organizer can cancel it on your calendar — a cancellation sent by anyone else leaves the event where it is — `docs/goal/behavior/inbound-scheduling-authority.md` § Who may mutate an existing event over the inbound rail
   - `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_co_attendees_forged_cancel_leaves_the_invite_on_the_events_page`
16. [app] You are told when someone who may not change an event on your calendar tried to, and can dismiss the notice once you have read it — `docs/goal/ui/events.md` § Refused scheduling changes
   - `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_refused_cancel_is_listed_on_the_events_page`
17. [app] An organizer who took their account back with their recovery kit can still cancel the events they invited you to, while a cancellation from anyone else is still refused — `docs/goal/behavior/inbound-scheduling-authority.md` § Who may mutate an existing event over the inbound rail
   - `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_succeeded_organizers_cancel_removes_the_invite_from_the_events_page`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_events.py::test_event_create_and_delete` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 1 | app | `tests/e2e-unified/tests/test_events.py::test_event_description_location_and_time_reach_the_detail_surface` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 1 | app | `tests/e2e-unified/tests/test_events.py::test_event_detail_back_returns_to_the_list` | web (linux): skipped, linux (linux): skipped, windows (windows): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_switch_to_month_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_switch_to_week_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_switch_to_day_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_month_navigation` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_pan_moves_one_visible_range_per_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_events.py::test_pan_is_inert_in_the_date_unfiltered_agenda` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 3 | app | `tests/e2e-unified/tests/test_events.py::test_month_day_cell_click_opens_day_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 3 | app | `tests/e2e-unified/tests/test_events.py::test_month_double_click_day_cell_opens_new_event_prefilled` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 3 | app | `tests/e2e-unified/tests/test_events.py::test_day_view_empty_slot_click_opens_quick_create_prefilled` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 3 | app | `tests/e2e-unified/tests/test_events.py::test_week_view_empty_slot_click_opens_quick_create_with_slot_time` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 4 | app | `tests/e2e-unified/tests/test_events.py::test_event_rsvp` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 4 | app | `tests/e2e-unified/tests/test_events.py::test_event_card_rsvp` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 5 | app | `tests/e2e-unified/tests/test_events.py::test_event_invite_attendee` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 6 | app | `tests/e2e-unified/tests/test_events.py::test_event_reminder` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 7 | app | `tests/e2e-unified/tests/test_calendar_visibility.py::test_calendar_visibility_filters_the_displayed_union` | web (linux): passed, linux (linux): passed, windows (windows): failed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_events_locale_week_start.py::test_tui_month_grid_opens_on_the_locale_week_start` | tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_events_locale_week_start_linux.py::test_linux_month_grid_opens_on_the_locale_week_start` | linux (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_events_locale_week_start_web.py::test_web_month_grid_opens_on_the_locale_week_start` | web (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_events_locale_week_start_apple.py::test_apple_month_grid_opens_on_the_locale_week_start` | macos (macos): passed, ios (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_events_locale_week_start_windows.py::test_windows_week_grid_opens_on_the_locale_week_start` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_caldav_external_appears.py::test_external_caldav_calendar_and_event_appear_while_on_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_mailbox_less_attendee_materializes_invite_on_events_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_calendar_visibility.py::test_calendar_visibility_filters_the_displayed_union` | web (linux): passed, linux (linux): passed, windows (windows): failed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_calendar_visibility.py::test_no_calendar_picked_shows_every_calendar_and_picking_one_narrows` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): error |
| 13 | app | `tests/e2e-unified/tests/test_events.py::test_calendar_exported_to_a_file_imports_into_another_calendar` | macos (macos): passed, ios (macos): passed, tui (linux): error |
| 13 | app | `tests/e2e-unified/tests/test_events.py::test_calendar_import_with_no_file_chosen_says_so` | macos (macos): passed, ios (macos): passed, tui (linux): error |
| 14 | app | `tests/e2e-unified/tests/test_events.py::test_day_timeline_bands_all_day_events_and_sets_clashes_side_by_side` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): error |
| 15 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_co_attendees_forged_cancel_leaves_the_invite_on_the_events_page` | web (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_refused_cancel_is_listed_on_the_events_page` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_a_succeeded_organizers_cancel_removes_the_invite_from_the_events_page` | web (linux): passed, tui (linux): passed |
<!-- features-render:end -->
