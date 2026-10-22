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
---

## What a user gets

On iPhone and Mac you can choose to keep a copy of your identity in your
iCloud Keychain as well as on the device. It is off until you turn it on, so
by default your identity stays on the one device. With it on, a new iPhone or
Mac restored from your Apple account, with iCloud Keychain on, finds your
identity when the app first starts there. Turning it off removes the copy
from iCloud Keychain and nothing more: every device that already has your
identity keeps it. Adding a device and your recovery kit remain the main ways
to have your identity in more than one place; the cloud copy is an extra you
can opt into.

## Coverage contract

Stamped 2026-10-21 at 77c610862e.

1. [app] You can choose to have your identity key kept in your device account's cloud keychain as well, and it is off until you turn it on — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_icloud_backup_toggle.py::test_icloud_backup_toggle_defaults_off_and_round_trips`
2. [app] Turning that copy off on one device never removes your identity from another device that has it on — `docs/goal/architecture/apps/common.md` § Credential storage
   - (none)
3. [app] A new iPhone or Mac restored from your Apple account, with iCloud Keychain on, finds your identity when the app first starts there — `docs/goal/architecture/apps/common.md` § Credential storage
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | — absent | |
| windows | — absent | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android | — absent | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_icloud_backup_toggle.py::test_icloud_backup_toggle_defaults_off_and_round_trips` | — |
| 2 | app | (none) | — |
| 3 | app | (none) | — |
<!-- features-render:end -->
