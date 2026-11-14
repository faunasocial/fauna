---
slug: identity-in-your-cloud-keychain
title: A copy of your identity in your cloud keychain
section: your data and devices
goal: docs/goal/architecture/apps/common.md § Credential storage
guide: docs/guides/identity-and-devices.md § Where the key lives — and where you should put a copy
absences:
  web: "docs/goal/architecture/apps/common.md § Credential storage"
  linux: "docs/goal/architecture/apps/common.md § Credential storage"
  windows: "docs/goal/architecture/apps/common.md § Credential storage"
  android: "docs/goal/architecture/apps/common.md § Credential storage"
  tui: "docs/goal/architecture/apps/common.md § Credential storage"
  ios (outcome 4): "docs/goal/architecture/apps/ios.md § Credential Storage"
---

## What a user gets

On iPhone you can choose to keep a copy of your identity in your iCloud Keychain
as well as on the device. It is off until you turn it on, so by default your
identity stays out of iCloud. With it on, a new iPhone restored from your Apple
account, with iCloud Keychain on, finds your identity when the app first starts
there. The Mac app shows the same switch but cannot make the copy yet: turning
it on leaves it off and says so. Once it can, your Mac and your iPhone on one
Apple account will share the one copy. Turning it off removes the copy from iCloud Keychain and
nothing more: every device that already has your identity keeps it. Adding a
device and your recovery kit remain the main ways to have your identity in more
than one place; the cloud copy is an extra you can opt into.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] You can choose to have your identity key kept in your device account's cloud keychain as well, and it is off until you turn it on — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_icloud_backup_toggle.py::test_icloud_backup_toggle_defaults_off_and_round_trips`
2. [app] Turning that copy off on one device never removes your identity from another device that has it on — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_turning_the_copy_off_keeps_this_devices_working_copy`
3. [app] A new iPhone or Mac restored from your Apple account, with iCloud Keychain on, finds your identity when the app first starts there — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_a_restored_device_finds_the_identity_from_the_circle_copy`
   - `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_the_copy_carries_the_identity_and_a_restored_device_is_a_new_device`
4. [app] On a Mac whose build cannot place the copy in the cloud keychain, turning the switch on is refused and says why, instead of seeming to work — `docs/goal/architecture/apps/ios.md` § Credential Storage
   - `tests/e2e-unified/tests/test_icloud_backup_refused_on_macos.py::test_the_toggle_cannot_rest_on_where_no_copy_can_land`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | — absent | |
| windows | — absent | |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+2b3552a6 standalone |
| android | — absent | |
| tui | — absent | |
| android-kids | — absent | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_icloud_backup_toggle.py::test_icloud_backup_toggle_defaults_off_and_round_trips` | macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_turning_the_copy_off_keeps_this_devices_working_copy` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_a_restored_device_finds_the_identity_from_the_circle_copy` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_icloud_backup_copy.py::test_the_copy_carries_the_identity_and_a_restored_device_is_a_new_device` | ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_icloud_backup_refused_on_macos.py::test_the_toggle_cannot_rest_on_where_no_copy_can_land` | macos (macos): passed |
| 4 | app | absent by design on ios, ios-kids | — |
<!-- features-render:end -->
