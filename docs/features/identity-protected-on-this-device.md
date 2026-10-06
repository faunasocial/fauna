---
slug: identity-protected-on-this-device
title: Your identity is kept safe on this device
section: your data and devices
goal: docs/goal/architecture/apps/common.md § Credential storage
guide: docs/guides/identity-and-devices.md § Where the key lives — and where you should put a copy
absences:
  web (outcome 2): "docs/goal/ui/settings.md § Credential store"
  linux (outcome 2): "docs/goal/ui/settings.md § Credential store"
  windows (outcome 2): "docs/goal/ui/settings.md § Credential store"
  macos (outcome 2): "docs/goal/ui/settings.md § Credential store"
  ios (outcome 2): "docs/goal/ui/settings.md § Credential store"
  android (outcome 2): "docs/goal/ui/settings.md § Credential store"
---

## What a user gets

Your identity is a secret key, and each device you use keeps its own copy
where that device protects secrets — the keychain on iPhone and Mac, the
equivalent stores on Windows, Linux and Android. The web app is the
exception: a browser has no such store, so there the key sits in the
browser's ordinary site storage, without that protection. On a server or over SSH, where there is no such
store, the terminal app asks you for a passphrase on first run and keeps the
key in a file sealed with it, asking for the passphrase again at each launch.
Either way, reopening the app finds your identity where you left it:
nothing to set up twice. Where the key is sealed under a passphrase, you can
change that passphrase from Settings, and the old one stops opening it.

The app never sends your key anywhere by itself — not into a backup, not
into a sync between your devices. It leaves a device only when you move it
yourself: by adding a device, exporting your identity, or making a recovery
kit. What your device's own backup then does with the place the key is kept
depends on the device: some keep the key on the device and out of the
backup, others carry it still locked under your sign-in password or your
passphrase, and in the web app it goes wherever a copy of your browser's
data goes.

## Coverage contract

Stamped 2026-10-01 at aa1e02642f.

1. [app] Once you have set up your identity on a device, the next launch there finds it — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch`
   - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock`
2. [app] You can change the passphrase that seals your identity, and the old one stops working — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
3. [app] If the device's protected store cannot be reached, the app says so, rather than quietly keeping your key somewhere less protected — `docs/goal/architecture/apps/common.md` § Credential storage
   - (none)
4. [app] Your key is never written unprotected: it is kept in the device's own protected store, or, where the device has none, in a file sealed under the passphrase you chose — `docs/goal/architecture/apps/common.md` § Credential storage
   - (none)
5. [app] The app never puts your identity key into a backup or a sync that leaves this device unless you ask it to — `docs/goal/architecture/apps/common.md` § Credential storage
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch` | linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed |
| 2 | app | absent by design on web, linux, windows, macos, ios, android | — |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
<!-- features-render:end -->
