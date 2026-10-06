---
slug: app-log
title: The app's own log
section: your data and devices
goal: docs/goal/architecture/apps/observability.md § 3. Surfaces
guide: docs/guides/app-tour.md § Settings
absences:
  web (outcome 6): "docs/goal/architecture/apps/observability.md § 2. Persistence & privacy"
---

## What a user gets

Settings has a Logs page with what the app has been doing, newest first,
filterable by severity, with copy and clear. Any error the app showed you is in
there too, so you can hand it to whoever helps you. On a computer or a phone the
same log is also kept in a file of its own on the device, so what happened
before a crash or a restart is not lost with the page.

## Coverage contract

Stamped 2026-09-26 at 7cd27fc591.

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
5. [app] The log never carries your message text, keys, tokens or claim codes — only what happened — `docs/goal/architecture/apps/observability.md` § 2. Persistence & privacy
   - `tests/e2e-unified/tests/test_settings_logs.py::test_message_text_never_reaches_the_log`
6. [app] The log is also kept in a file on your device, so what the app did before a crash or a restart is still there to hand over — `docs/goal/architecture/apps/observability.md` § 2. Persistence & privacy
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+d2b9791b standalone |
| linux | ⚠ partial | 0.1.2-dev+9d23e44e.dirty standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+5893bba8 standalone |
| ios | ⚠ partial | 0.1.2-dev+5893bba8 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+9d23e44e.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_level_filter_narrows` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_copy_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_heading_is_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_settings_error_is_captured` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_displayed_banner_reaches_ring` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): failed, ios (macos): failed, tui (linux): skipped |
| 3 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_a_copied_notice_reaches_the_ring_at_info` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_what_the_app_printed_or_swallowed_reaches_the_ring` | web (linux): skipped, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_settings_logs.py::test_message_text_never_reaches_the_log` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | (none) | — |
| 6 | app | absent by design on web | — |
<!-- features-render:end -->
