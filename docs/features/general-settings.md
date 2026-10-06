---
slug: general-settings
title: Start at login and close to tray
section: your data and devices
goal: docs/goal/architecture/apps/common.md § Desktop Residency
guide: docs/guides/app-tour.md § Settings
absences:
  web: "docs/goal/architecture/apps/common.md § Desktop Residency"
  ios: "docs/goal/architecture/apps/common.md § Desktop Residency"
  android: "docs/goal/architecture/apps/common.md § Desktop Residency"
  tui: "docs/goal/architecture/apps/common.md § Desktop Residency"
---

## What a user gets

On the desktop the app can start when you sign in, and closing its window is
never a silent stop: the app keeps running in the tray where there is one, sync
carries on where the desktop keeps its agent alive past the window, and where
nothing can stay behind, closing quits and the setting says so. Close to tray
is on by default wherever there is a tray, and start at login is on by default
on Linux and Windows; on macOS you turn start at login on yourself.

## Coverage contract

Stamped 2026-09-22 at e9f79fdf25.

1. [app] Closing the window is never a silent stop: either something stays behind and keeps working — the app hidden in the tray, or a sync agent that outlives it — or closing quits and the setting says so; where close-to-tray is a choice, it survives a relaunch — `docs/goal/architecture/apps/common.md` § Desktop Residency
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_host_present_enables_toggle_and_hides_on_close`
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_no_host_greys_toggle_and_quits_on_close`
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install`
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_choice_survives_a_relaunch`
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_host_disappears_at_runtime_regreys_and_quits`
   - `tests/e2e-unified/tests/test_sync_agent_survives_macos_window_close.py::test_macos_window_close_leaves_the_real_sync_agent_serving`
   - `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install`
   - `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_closing_hides_to_tray_and_keeps_the_app_alive`
   - `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_choice_survives_a_relaunch`
2. [app] Started at sign-in, the app stays hidden only when it lands in your feed — `docs/goal/architecture/apps/windows.md` § App Lifecycle
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_j_autostart_stays_hidden_only_when_it_lands_in_the_main_app`
3. [app] Close to tray is on from the first launch, before you ever touch the setting — `docs/goal/architecture/apps/common.md` § Desktop Residency
   - `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install`
   - `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install`
4. [app] The app can start when you sign in to your desktop, and the choice is yours to keep or turn off — `docs/goal/architecture/apps/windows.md` § App Lifecycle
   - `tests/e2e-unified/tests/test_autostart_choice.py::test_start_at_login_is_a_choice_that_survives_a_relaunch`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | |
| macos | ⚠ partial | |
| ios | — absent | |
| android | — absent | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_host_present_enables_toggle_and_hides_on_close` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_no_host_greys_toggle_and_quits_on_close` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_choice_survives_a_relaunch` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_host_disappears_at_runtime_regreys_and_quits` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_sync_agent_survives_macos_window_close.py::test_macos_window_close_leaves_the_real_sync_agent_serving` | macos (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_closing_hides_to_tray_and_keeps_the_app_alive` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_choice_survives_a_relaunch` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_j_autostart_stays_hidden_only_when_it_lands_in_the_main_app` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_tray_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_windows_close_to_tray.py::test_close_to_tray_defaults_on_for_a_fresh_install` | windows (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_autostart_choice.py::test_start_at_login_is_a_choice_that_survives_a_relaunch` | linux (linux): passed, windows (windows): passed |
<!-- features-render:end -->
