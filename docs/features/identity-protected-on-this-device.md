---
slug: identity-protected-on-this-device
title: Your identity is kept safe on this device
section: your data and devices
goal: docs/goal/architecture/apps/common.md § Credential storage
guide: docs/guides/identity-and-devices.md § Where the key lives — and where you should put a copy
---

## What a user gets

Your identity is a secret key, and each device you use keeps its own copy where that device protects secrets — the keychain on iPhone and Mac, the equivalent stores on Windows, Linux and Android. Where there is no such store — in the web app, and in the terminal app on a server or over SSH, or on a computer whose system keyring is locked or not set up — the app asks you to choose a passphrase the first time it opens and keeps the key encrypted with it, asking for the passphrase again at each launch (in a browser, once per session, as long as one Fauna tab stays open). Either way, reopening the app finds your identity where you left it: nothing to set up twice. Where the key is encrypted with a passphrase, you can change that passphrase from Settings, and the old one stops opening it.

The app never sends your key anywhere by itself — not into a backup, not into a sync between your devices. It leaves a device only when you move it yourself: by adding a device, exporting your identity, making a recovery kit, or — on iPhone and Mac — turning on the iCloud Keychain backup. What your device's own backup then does with the place the key is kept depends on the device: some keep the key on the device and out of the backup, others carry it still locked under your sign-in password or your passphrase — and a copy of your browser's data carries it encrypted with your passphrase, too.

## Coverage contract

Stamped 2026-10-21 at 46bbdabb61.

1. [app] Once you have set up your identity on a device, the next launch there finds it — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch`
   - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
2. [app] If the device's protected store cannot be reached, the app says so, rather than quietly keeping your key somewhere less protected — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_no_protected_store_is_said_and_the_key_rests_only_sealed`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_locked_keyring_is_said_on_the_create_surface_and_tui_still_starts`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_no_protected_store_is_a_loud_error_and_the_wizard_stays_put`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_keychain_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_keystore_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_credential_manager_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put`
   - `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_browser_has_no_protected_store_and_says_so_and_the_key_rests_only_sealed`
3. [app] Your key is never written unprotected: it is kept in the device's own protected store, or, where the device has none, in a file encrypted with the passphrase you chose — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_ios_identity_rests_device_bound_in_the_real_keychain_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_macos_identity_rests_sealed_in_a_keychain_file_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/real_session/test_identity_rests_in_credential_manager.py::test_the_identity_key_rests_non_roaming_in_credential_manager_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/real_session/test_identity_rests_in_the_android_secure_prefs.py::test_the_android_identity_rests_only_in_the_encrypted_store_and_never_reaches_the_nest`
4. [app] The app never puts your identity key into a backup or a sync that leaves this device unless you ask it to — `docs/goal/architecture/apps/common.md` § Credential storage
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_installed_android_package_keeps_the_identity_out_of_backup_and_off_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_ios_identity_rests_device_bound_in_the_real_keychain_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_macos_identity_rests_sealed_in_a_keychain_file_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_web_identity_rests_only_sealed_in_the_origin_and_never_reaches_the_nest`
   - `tests/e2e-unified/tests/real_session/test_identity_rests_in_credential_manager.py::test_the_identity_key_rests_non_roaming_in_credential_manager_and_never_reaches_the_nest`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5 docker |
| linux | ✅ full | 0.1.5-dev+cbd813be standalone |
| windows | ⚠ partial | |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | |
| android | ❌ failing | |
| tui | ✅ full | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock` | tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_no_protected_store_is_said_and_the_key_rests_only_sealed` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_locked_keyring_is_said_on_the_create_surface_and_tui_still_starts` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_no_protected_store_is_a_loud_error_and_the_wizard_stays_put` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_keychain_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put` | macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_keystore_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put` | — |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_credential_manager_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_store_unreachable.py::test_a_browser_has_no_protected_store_and_says_so_and_the_key_rests_only_sealed` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest` | linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_ios_identity_rests_device_bound_in_the_real_keychain_and_never_reaches_the_nest` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_macos_identity_rests_sealed_in_a_keychain_file_and_never_reaches_the_nest` | macos (macos): passed |
| 3 | app | `tests/e2e-unified/tests/real_session/test_identity_rests_in_credential_manager.py::test_the_identity_key_rests_non_roaming_in_credential_manager_and_never_reaches_the_nest` | windows (windows): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/real_session/test_identity_rests_in_the_android_secure_prefs.py::test_the_android_identity_rests_only_in_the_encrypted_store_and_never_reaches_the_nest` | android (linux): error |
| 4 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest` | linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_installed_android_package_keeps_the_identity_out_of_backup_and_off_the_nest` | — |
| 4 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_ios_identity_rests_device_bound_in_the_real_keychain_and_never_reaches_the_nest` | ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_macos_identity_rests_sealed_in_a_keychain_file_and_never_reaches_the_nest` | macos (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_key_stays_on_the_device.py::test_the_web_identity_rests_only_sealed_in_the_origin_and_never_reaches_the_nest` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/real_session/test_identity_rests_in_credential_manager.py::test_the_identity_key_rests_non_roaming_in_credential_manager_and_never_reaches_the_nest` | windows (windows): passed, tui (windows): passed |
<!-- features-render:end -->
