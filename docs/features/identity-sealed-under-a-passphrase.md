---
slug: identity-sealed-under-a-passphrase
title: Your identity encrypted with a passphrase
section: your data and devices
goal: docs/goal/architecture/apps/tui.md § Credential storage
guide: docs/guides/identity-and-devices.md § Where the key lives — and where you should put a copy
absences:
  linux: "docs/goal/ui/settings.md § Credential store"
  windows: "docs/goal/ui/settings.md § Credential store"
  macos: "docs/goal/ui/settings.md § Credential store"
  ios: "docs/goal/ui/settings.md § Credential store"
  android: "docs/goal/ui/settings.md § Credential store"
  tui (outcome 18): "docs/goal/architecture/apps/tui.md § Declared platform absences"
---

## What a user gets

Where the app has no protected store of the device's own to keep your identity in — the terminal app on a server or over SSH, or on a computer whose system keyring is locked or not set up, and the web app in any browser — Fauna keeps your identity encrypted with a passphrase you choose. You choose it before you set anything up, typing it twice, and the app asks for it again before it opens anything; in a browser, a new tab asks nothing while another Fauna tab is already open. What you type never shows, a wrong passphrase is refused, and you can change the passphrase from Settings without being signed out; the app reminds you to back up your identity first, because nobody can reset a forgotten passphrase — your identity then comes back from your recovery kit, an exported copy of your key, or another device where you're signed in. In the web app you can also start over from the unlock screen: that erases this browser's copy of your keys and nothing else. In a browser and on a device that support it, with the web app opened at your nest's name over HTTPS rather than at a bare IP address, you can also add a passkey that unlocks your encrypted identity with your device's screen lock or a security key; the passphrase keeps working. However the app stops, whatever it finds it cannot read, and however many copies of it are open, your encrypted identity is never cut short, emptied or lost. On a computer or phone that keeps your identity in its own protected store instead, there is no passphrase to choose.

## Coverage contract

Stamped 2026-10-17 at 2ad53d795c.

1. [app] A wrong passphrase is refused with a message saying the passphrase is wrong or the stored identity is damaged, and the app stays locked until the right one is given — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
2. [app] You choose your passphrase before setting up your identity, typing it twice, and two entries that differ are refused — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
3. [app] Nothing opens before your identity is unlocked: a launch asks for your passphrase first, unless another open window of the app has already unlocked it — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
   - `tests/e2e-unified/tests/test_web_passkey_identity_store.py::test_web_passkey_wrap_journey`
4. [app] A passphrase you type is never shown: each character appears only as a dot — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
5. [app] You can change the passphrase that seals your identity, and the old one stops working — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
6. [app] Changing the passphrase asks for the current one first, and a wrong current passphrase is refused with nothing changed — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
7. [app] A new passphrase that does not match its confirmation, or an empty entry, is refused, and the old passphrase keeps working — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
8. [app] Before you change the passphrase the app reminds you to back up your identity first, since a forgotten new passphrase cannot be recovered — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
9. [app] Changing the passphrase does not sign you out or interrupt what you are doing — `docs/goal/architecture/apps/tui.md` § Credential storage
   - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
   - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
10. [app] If the app stops in the middle of changing the passphrase, your identity still opens with either the old passphrase or the new one, never neither — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_an_interrupted_passphrase_change_opens_with_old_or_new`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_an_interrupted_passphrase_change_opens_with_old_or_new`
11. [app] Settings tells you how your identity is held on this device — `docs/goal/ui/settings.md` § Credential store
    - `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
12. [app] A forgotten passphrase cannot be reset: your identity comes back only from your recovery kit or a copy of your key kept elsewhere — `docs/goal/ui/settings.md` § Credential store
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_forgotten_passphrase_cannot_be_reset`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_forgotten_passphrase_cannot_be_reset`
13. [app] If the app stops while saving your sealed identity, what it saved is never left cut short or unreadable — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_crash_while_saving_never_cuts_the_sealed_file_short`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_crash_while_saving_never_cuts_the_sealed_file_short`
14. [app] A sealed identity the app cannot read — damaged, or written by a newer version of the app — is left as it is, never replaced with an empty one, so it never silently costs you your identity — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_an_unreadable_sealed_identity_is_left_as_it_is`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_an_unreadable_sealed_identity_is_left_as_it_is`
15. [app] Two copies of the app open at once on the same sealed identity never lose it between them — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_two_copies_on_one_sealed_identity_never_lose_it`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_two_copies_on_one_sealed_identity_never_lose_it`
16. [app] Once your identity is sealed under a passphrase, the app keeps finding it there: it never switches by itself to another store and leaves your sealed identity behind — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_sealed_identity_is_never_left_behind_for_another_store`
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_sealed_identity_is_never_left_behind_for_another_store`
17. [app] After you reset the app, the next setup asks you to choose a passphrase again — `docs/goal/architecture/apps/tui.md` § Credential storage
    - `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey`
    - `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_after_a_reset_the_next_setup_chooses_a_passphrase_again`
18. [app] On a browser and device that support it, you can add a passkey that opens your encrypted identity in place of the passphrase; the passphrase keeps working, and removing the passkey leaves it as the way in — `docs/goal/architecture/apps/web.md` § Identity store at rest
    - `tests/e2e-unified/tests/test_web_passkey_identity_store.py::test_web_passkey_wrap_journey`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+1c9a6a26 standalone |
| linux | — absent | |
| windows | — absent | |
| macos | — absent | |
| ios | — absent | |
| android | — absent | |
| tui | ✅ full | 0.1.5-dev+67f8b249 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock` | tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock` | tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_headless_store_create_claim_relaunch_unlock` | tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_passkey_identity_store.py::test_web_passkey_wrap_journey` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_an_interrupted_passphrase_change_opens_with_old_or_new` | tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_an_interrupted_passphrase_change_opens_with_old_or_new` | web (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_tui_credential_store_rekey.py::test_change_passphrase_relaunch_old_refused_new_unlocks` | tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_forgotten_passphrase_cannot_be_reset` | tui (linux): passed, tui (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_forgotten_passphrase_cannot_be_reset` | web (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_crash_while_saving_never_cuts_the_sealed_file_short` | tui (linux): passed, tui (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_crash_while_saving_never_cuts_the_sealed_file_short` | web (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_an_unreadable_sealed_identity_is_left_as_it_is` | tui (linux): passed, tui (macos): passed |
| 14 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_an_unreadable_sealed_identity_is_left_as_it_is` | web (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_two_copies_on_one_sealed_identity_never_lose_it` | tui (linux): passed, tui (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_two_copies_on_one_sealed_identity_never_lose_it` | web (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_a_sealed_identity_is_never_left_behind_for_another_store` | tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_a_sealed_identity_is_never_left_behind_for_another_store` | web (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_web_sealed_identity_store.py::test_web_sealed_identity_store_journey` | web (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_tui_headless_credential_store.py::test_after_a_reset_the_next_setup_chooses_a_passphrase_again` | tui (linux): passed, tui (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_web_passkey_identity_store.py::test_web_passkey_wrap_journey` | web (linux): passed |
| 18 | app | absent by design on tui | — |
<!-- features-render:end -->
