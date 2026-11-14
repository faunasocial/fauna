---
slug: app-log
title: The app's own log
section: your data and devices
goal: docs/goal/architecture/apps/observability.md § 3. Surfaces
guide: docs/guides/app-tour.md § Settings
absences:
  web (outcome 7): "docs/goal/architecture/apps/observability.md § 2. Persistence & privacy"
  web (outcome 6): "docs/goal/architecture/apps/observability.md § 2. Persistence & privacy"
---

## What a user gets

Settings has a Logs page with what the app has been doing, newest first,
filterable by severity, with copy and clear. Any error the app showed you is in
there too, so you can hand it to whoever helps you. On a computer or a phone the
same log is also kept in a file of its own on the device, so what happened
before a crash or a restart is not lost with the page.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] The page shows the app's activity, filters by severity, and copies or clears — `docs/goal/architecture/apps/observability.md` § 3. Surfaces
   - `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_renders`
   - `tests/e2e-unified/tests/test_settings_logs.py::test_logs_level_filter_narrows`
   - `tests/e2e-unified/tests/test_settings_logs.py::test_logs_copy_and_clear`
   - `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_heading_is_visible`
2. [app] An error the app showed you is in the log — `docs/goal/architecture/apps/observability.md` § What must be logged
   - `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_settings_error_is_captured`
   - `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_banner_reaches_ring`
3. [app] Not only errors: a warning, a notice or a success line the app showed you is in the log too, at a matching level — `docs/goal/architecture/apps/observability.md` § What must be logged
   - `tests/e2e-unified/tests/test_settings_logs.py::test_a_copied_notice_reaches_the_ring_at_info`
4. [app] Something the app printed where you could not see it, or a failure it quietly swallowed, is in the log — `docs/goal/architecture/apps/observability.md` § What must be logged
   - `tests/e2e-unified/tests/test_settings_logs.py::test_what_the_app_printed_or_swallowed_reaches_the_ring`
5. [app] The app is written never to record your message text, keys, tokens or claim codes in the log — only what happened — `docs/goal/architecture/apps/observability.md` § 2. Persistence & privacy
   - `tests/e2e-unified/tests/test_settings_logs.py::test_message_text_never_reaches_the_log`
6. [app] The log is also kept in a file on your device, so what the app did before a crash or a restart is still there to hand over — `docs/goal/architecture/apps/observability.md` § 2. Persistence & privacy
   - `tests/e2e-unified/tests/test_settings_logs.py::test_a_relaunch_finds_the_previous_launchs_line_in_the_on_device_file`
7. [app] The log the app keeps on this device is held in a file of bounded size, so it never grows without limit — `docs/goal/architecture/apps/observability.md` § 1. Shared capture
   - `tests/e2e-unified/tests/test_app_log_size_cap_tui.py::test_the_on_device_log_directory_stays_under_its_size_cap`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+36883152.dirty standalone |
| linux | ✅ full | 0.1.5-dev+cbd813be standalone |
| windows | ⚠ partial | 0.1.5-dev+fbba25bc standalone |
| macos | ✅ full | 0.1.5-dev+03b5332b standalone |
| ios | ✅ full | 0.1.5-dev+03b5332b standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+af795ff8.dirty standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_level_filter_narrows` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_copy_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_heading_is_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_settings_error_is_captured` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_banner_reaches_ring` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_a_copied_notice_reaches_the_ring_at_info` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_what_the_app_printed_or_swallowed_reaches_the_ring` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_message_text_never_reaches_the_log` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_a_relaunch_finds_the_previous_launchs_line_in_the_on_device_file` — requires `on-device-log-reader` (test harness) (not built: web, windows) | web (linux): skipped, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | absent by design on web | — |
| 7 | app | `tests/e2e-unified/tests/test_app_log_size_cap_tui.py::test_the_on_device_log_directory_stays_under_its_size_cap` — requires `on-device-log-reader` (test harness) (not built: web, windows) | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | absent by design on web | — |
<!-- features-render:end -->
