---
slug: home-screen-widget
title: Unread count on your home screen
section: everyday
goal: docs/goal/architecture/apps/common.md § Home-screen widget
guide: docs/guides/app-tour.md § Notifications
absences:
  web: "docs/goal/architecture/apps/common.md § Home-screen widget"
  tui: "docs/goal/architecture/apps/common.md § Home-screen widget"
---

## What a user gets

Fauna shows how many unread messages are waiting for you without your opening
the app: in a widget on your phone's home screen or your Mac's desktop, and as a
count on the app's own icon on Windows and Linux. It keeps itself up to date in
the background.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] A Fauna widget on your home screen shows how many unread messages are waiting for you — `docs/goal/architecture/apps/common.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_widget_shows_the_apps_own_unread_count`
   - `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden`
2. [app] The widget's count keeps itself current in the background, without you opening the app — `docs/goal/architecture/apps/common.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_count_keeps_itself_current_without_opening_the_app`
   - `tests/e2e-unified/tests/test_home_screen_widget_background_refresh.py::test_the_scheduled_background_refresh_keeps_the_count_current`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_an_autostart_launch_is_resident_with_no_window_and_keeps_the_count_current`
   - `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden`
3. [app] The count on your home screen is the same unread total your conversations list shows in the app, never a different number — `docs/goal/architecture/apps/common.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_widget_shows_the_apps_own_unread_count`
   - `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden`
4. [app] A conversation you read on another of your devices stops counting on this one's home screen — `docs/goal/architecture/apps/common.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_a_conversation_read_on_another_device_stops_counting_on_the_badge`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_a_conversation_read_on_another_device_stops_counting_on_the_widget`
5. [app] Switching account or signing out never leaves the previous account's count on your home screen — `docs/goal/architecture/apps/ios.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_signing_out_never_leaves_the_previous_accounts_count`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_signing_out_never_leaves_the_previous_accounts_count_on_the_widget`
6. [app] Narrowing your conversations list with a search never changes the count on your home screen — `docs/goal/architecture/apps/windows.md` § Home-screen widget
   - `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_a_search_in_the_conversations_list_never_changes_the_badge`
   - `tests/e2e-unified/tests/test_home_screen_widget.py::test_a_search_in_the_conversations_list_never_changes_the_widget_count`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.5-dev+aaa3f320 standalone |
| windows | ⚠ partial | 0.1.5-dev+a73ed62e.dirty standalone |
| macos | ✅ full | 0.1.5-dev+6017a6f7 standalone |
| ios | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| android |  no run recorded | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_widget_shows_the_apps_own_unread_count` | macos (macos): passed, ios (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_count_keeps_itself_current_without_opening_the_app` | macos (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_home_screen_widget_background_refresh.py::test_the_scheduled_background_refresh_keeps_the_count_current` | ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_an_autostart_launch_is_resident_with_no_window_and_keeps_the_count_current` | macos (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_the_widget_shows_the_apps_own_unread_count` | macos (macos): passed, ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden` | windows (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_a_conversation_read_on_another_device_stops_counting_on_the_badge` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_a_conversation_read_on_another_device_stops_counting_on_the_widget` | macos (macos): passed, ios (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_signing_out_never_leaves_the_previous_accounts_count` | linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_signing_out_never_leaves_the_previous_accounts_count_on_the_widget` | macos (macos): passed, ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_a_search_in_the_conversations_list_never_changes_the_badge` | linux (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_home_screen_widget.py::test_a_search_in_the_conversations_list_never_changes_the_widget_count` | macos (macos): passed, ios (macos): passed |
<!-- features-render:end -->
