---
slug: devices-in-step-without-the-nest
title: devices-in-step-without-the-nest
section: your data and devices
goal: docs/goal/architecture/account-sync-plane.md § The peer leg — device↔device sync over iroh (requirement 7)
guide: docs/guides/identity-and-devices.md § Adding a device
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-11-07 at 9e37a3a09c.

1. [app] A preference you change on one of your devices reaches another of your devices on the same network while your nest, or your internet connection, is down — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down`
2. [app] A conversation you read on one of your devices shows as read on another of your devices on the same network while your nest is down — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_one_device_shows_read_on_the_other_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_tui_shows_read_on_the_phone_while_the_nest_is_down`
3. [app] Nothing is set up or switched on first, and there is no offline mode to enter: your devices find each other from what they learned while your nest was reachable — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_one_device_shows_read_on_the_other_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_tui_shows_read_on_the_phone_while_the_nest_is_down`
4. [app] Once your nest is reachable again, a change your devices passed between themselves while it was down reaches the nest from whichever of them connects first, with nothing lost and nothing asked of you — `docs/goal/architecture/account-sync-plane.md` § The bind leg
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down`
   - `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux |  no run recorded | |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios | ✅ full | 0.1.6-dev+ab9e138e standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.6-dev+64373b38 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down` | tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down` | ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_one_device_shows_read_on_the_other_while_the_nest_is_down` | tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_tui_shows_read_on_the_phone_while_the_nest_is_down` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down` | tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_one_device_shows_read_on_the_other_while_the_nest_is_down` | tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_conversation_read_on_tui_shows_read_on_the_phone_while_the_nest_is_down` | ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_one_device_reaches_the_other_while_the_nest_is_down` | tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_devices_in_step_without_the_nest.py::test_a_setting_changed_on_tui_reaches_the_phone_while_the_nest_is_down` | ios (macos): passed |
<!-- features-render:end -->
