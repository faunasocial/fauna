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
the app: in a widget you place on your phone's or Mac's home screen, and as a
count on the app's own icon on Windows and Linux. It keeps itself up to date
in the background.

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
   - (none)
5. [app] Switching account or signing out never leaves the previous account's count on your home screen — `docs/goal/architecture/apps/ios.md` § Home-screen widget
   - (none)
6. [app] Narrowing your conversations list with a search never changes the count on your home screen — `docs/goal/architecture/apps/windows.md` § Home-screen widget
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+0881a284.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+779edfa9 standalone |
| ios | ⚠ partial | 0.1.2-dev+2a55990d standalone |
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
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
<!-- features-render:end -->
