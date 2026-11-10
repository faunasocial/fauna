---
slug: recover-a-lost-nest
title: Recover a lost nest
section: getting in
goal: docs/goal/architecture/nest/box-recovery.md § Goal
guide: docs/guides/cloud-backup.md § Restoring after disaster
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

If the machine your nest ran on is gone for good, your app can bring it back
with the same identity, so every device and every contact that knew the old nest
carries on without a trust warning. The app keeps what it needs for that from the
moment you claim; recovery is a choice of where the new nest should run.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] The sign-up screen offers to recover a lost nest, lists the nests you own, and lets you choose cloud or self-hosted for the new one — `docs/goal/architecture/nest/box-recovery.md` § Recovery UI (step 4)
   - `tests/e2e-unified/tests/test_box_recovery_ui.py::test_identity_choice_shows_recover_lost_box_button`
   - `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_renders_box_list`
   - `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_cloud_method_advances_to_vps_config`
   - `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_selfhosted_method_advances_to_instructions`
2. [app] A device that never saw one of your nests can still recover it by reaching another of yours — `docs/goal/architecture/nest/box-recovery.md` § Mechanism
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_two_nest_fan_out_fires_and_fresh_device_recovers_both_boxes`
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_on_a_reachable_custodied_nest`
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_linking_a_box_custodies_it_and_a_fresh_device_recovers_both`
3. [nest] A rebuilt nest presents the same identity the old one had — `docs/goal/architecture/nest/box-recovery.md` § Mechanism
   - `tests/e2e-unified/tests/api/test_box_recovery.py::test_rebuilt_box_readopts_custodied_seed_identity`
   - `tests/e2e-unified/tests/api/test_box_recovery.py::test_claim_handoff_seed_derives_to_nest_info_identity`
4. [app] Once the new nest is up, your posts, mail, calendar, files and contacts come back from your backup destinations — `docs/goal/architecture/nest/box-recovery.md` § Goal
   - `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_every_kind_comes_back_onto_the_rebuilt_one`
   - `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_every_kind_comes_back_from_a_surviving_nest`
5. [app] When the nest a device was signed in to is gone, that device still offers to recover it, from what the device itself holds — `docs/goal/architecture/nest/box-recovery.md` § The plane-era recovery floor
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_when_the_saved_nest_is_dead`
6. [app] A nest identity that was rotated away is never offered for recovery — `docs/goal/architecture/nest/box-recovery.md` § Custody after rotation
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_a_rotated_away_identity_is_never_offered_for_recovery`
7. [app] When the app cannot confirm that your nest's recovery seed is held off the box, it warns you, in the same words on every app — `docs/goal/architecture/nest/box-recovery.md` § The plane-era recovery floor
   - `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_an_unconfirmed_custody_warns_the_admin_in_the_shared_words`
8. [app] A nest you administer but did not claim yourself is also among the nests your device can recover: every admin's device comes to hold its recovery seed — `docs/goal/architecture/nest/box-recovery.md` § Mechanism
   - `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_a_co_admins_device_custodies_a_box_it_did_not_claim`
9. [app] Choosing cloud rebuilds the lost nest on a new server carrying its saved identity and points its domain at the new server — `docs/goal/architecture/nest/box-recovery.md` § Recovery UI (step 4)
   - `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_cloud_recovery_rebuilds_with_the_saved_seed_and_repoints_the_domain`
10. [app] Choosing self-hosted shows a command to copy that carries the lost nest's recovery seed for the installer, and copying waits until the command is resolved — `docs/goal/architecture/nest/box-recovery.md` § Recovery UI (step 4)
   - `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_the_selfhosted_command_carries_the_seed_and_copy_waits_for_it`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+5a9a5fa8 standalone |
| linux | ⚠ partial | 0.1.5-dev+5a9a5fa8 standalone |
| windows | ⚠ partial | 0.1.5-dev+5a9a5fa8 standalone |
| macos | ⚠ partial | 0.1.5-dev+5a9a5fa8 standalone |
| ios | ⚠ partial | 0.1.5-dev+56c8dd96.dirty standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+5a9a5fa8 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_identity_choice_shows_recover_lost_box_button` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_renders_box_list` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_cloud_method_advances_to_vps_config` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_selfhosted_method_advances_to_instructions` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_two_nest_fan_out_fires_and_fresh_device_recovers_both_boxes` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_on_a_reachable_custodied_nest` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_linking_a_box_custodies_it_and_a_fresh_device_recovers_both` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_box_recovery.py::test_rebuilt_box_readopts_custodied_seed_identity` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_box_recovery.py::test_claim_handoff_seed_derives_to_nest_info_identity` | nest (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_every_kind_comes_back_onto_the_rebuilt_one` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_every_kind_comes_back_from_a_surviving_nest` | web (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_when_the_saved_nest_is_dead` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_a_rotated_away_identity_is_never_offered_for_recovery` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_an_unconfirmed_custody_warns_the_admin_in_the_shared_words` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_a_co_admins_device_custodies_a_box_it_did_not_claim` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_cloud_recovery_rebuilds_with_the_saved_seed_and_repoints_the_domain` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_box_recovery_seed_custody.py::test_the_selfhosted_command_carries_the_seed_and_copy_waits_for_it` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
<!-- features-render:end -->
