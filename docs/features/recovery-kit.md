---
slug: recovery-kit
title: Your recovery kit
section: your data and devices
goal: docs/goal/ui/settings.md § Recovery kit
guide: docs/guides/identity-and-devices.md § Your recovery kit
---

## What a user gets

Make a recovery kit from Settings: a phrase, shown once, that can restore your
account after you have lost every device, or replace a kit you already hold. A kit
alone names no account, so the restore asks which one; an identity key pasted where
the phrase belongs is refused.

## Coverage contract

Stamped 2026-09-19 at acb58fddf3.

1. [app] Creating a kit shows the phrase once and the status changes; leaving the page does not show it again — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_recovery_kit_settings.py::test_creating_a_recovery_kit_flips_the_status_and_shows_the_secret_once`
2. [app] Replacing the kit with the one you hold makes a new phrase and retires the old — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_recovery_kit_settings.py::test_replacing_the_kit_with_the_one_you_hold_mints_a_different_secret`
3. [app] The phrase alone restores your account after every device is gone — `docs/goal/behavior/identity-succession.md` § Seed escrow (loss recovery) — default on
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_the_phrase_alone_restores_the_account_after_losing_every_device`
4. [app] A phrase that names no account asks for one; an identity key is refused where the phrase belongs — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_phrase_that_names_no_account_asks_for_one_instead_of_failing`
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_an_identity_secret_is_never_taken_for_a_recovery_phrase`
5. [nest] Your nest registers the kit and refuses one from the wrong identity — `docs/goal/behavior/identity-succession.md` § The RecoveryKey
   - `tests/e2e-unified/tests/api/test_succession_fixture.py::test_the_fixture_registers_a_kit_the_nest_serves_back`
   - `tests/e2e-unified/tests/api/test_succession_fixture.py::test_a_kit_from_the_wrong_identity_is_refused`
6. [app] Your kit's state is told plainly: none made, one registered, one whose phrase cannot restore you, or a replacement in its waiting period — including a kit you made on another device — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_the_status_names_each_of_the_four_states_including_a_kit_made_elsewhere`
7. [app] If you no longer have your kit, you can replace it with your identity alone: the new phrase is shown at once and takes over after a waiting period — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_lost_kit_is_replaced_with_the_identity_alone_and_waits_out_its_window`
8. [app] A kit replacement you did not ask for can be cancelled with the kit you hold — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_replacement_you_did_not_ask_for_is_cancelled_with_the_kit_you_hold`
9. [app] When your phrase cannot currently restore your account, the page says so and repairs it from the kit you already hold, without retiring that kit — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_kit_whose_phrase_cannot_restore_is_repaired_without_retiring_it`
10. [app] After you replace your kit, it is the new phrase that restores your account — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_after_a_replace_the_new_phrase_restores_and_the_old_one_does_not`
11. [app] A restore that cannot work says which case you are in — nothing rests to restore from, or the account has already moved to a new identity — and what to do next — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_restore_with_no_sealed_copy_to_restore_from_says_so`
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_restore_of_an_identity_that_moved_on_routes_to_the_import`
12. [app] When your account's domain is gone, typing your handle together with your nest's own address still restores you — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_the_phrase_alone_restores_the_account_after_losing_every_device`
13. [app] A kit you copied or scanned restores your account with nothing typed but the phrase — `docs/goal/behavior/identity-succession.md` § The RecoveryKey
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_copied_kit_restores_the_account_with_nothing_typed_but_the_kit`
14. [app] After taking your account back, a kit you replace still carries what your files need: restoring from the new phrase on a new device opens the files you had before the takeover — `docs/goal/behavior/identity-succession.md` § Seed escrow (loss recovery) — default on
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_kit_replaced_after_a_succession_still_restores_the_predecessor_corpus`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+e48628f5 standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_recovery_kit_settings.py::test_creating_a_recovery_kit_flips_the_status_and_shows_the_secret_once` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_recovery_kit_settings.py::test_replacing_the_kit_with_the_one_you_hold_mints_a_different_secret` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_the_phrase_alone_restores_the_account_after_losing_every_device` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_phrase_that_names_no_account_asks_for_one_instead_of_failing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_an_identity_secret_is_never_taken_for_a_recovery_phrase` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_succession_fixture.py::test_the_fixture_registers_a_kit_the_nest_serves_back` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_succession_fixture.py::test_a_kit_from_the_wrong_identity_is_refused` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_the_status_names_each_of_the_four_states_including_a_kit_made_elsewhere` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_lost_kit_is_replaced_with_the_identity_alone_and_waits_out_its_window` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_replacement_you_did_not_ask_for_is_cancelled_with_the_kit_you_hold` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_recovery_kit_ceremonies.py::test_a_kit_whose_phrase_cannot_restore_is_repaired_without_retiring_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_after_a_replace_the_new_phrase_restores_and_the_old_one_does_not` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_restore_with_no_sealed_copy_to_restore_from_says_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_restore_of_an_identity_that_moved_on_routes_to_the_import` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_the_phrase_alone_restores_the_account_after_losing_every_device` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_copied_kit_restores_the_account_with_nothing_typed_but_the_kit` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_a_kit_replaced_after_a_succession_still_restores_the_predecessor_corpus` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
<!-- features-render:end -->
