---
slug: recover-a-lost-nest
title: Recover a lost nest
section: getting in
goal: docs/goal/architecture/nest/box-recovery.md § Goal
guide: docs/guides/cloud-backup.md § Restoring after disaster
---

## What a user gets

If the machine your nest ran on is gone for good, your app can bring it back
with the same identity, so every device and every contact that knew the old nest
carries on without a trust warning. The app keeps what it needs for that from the
moment you claim; recovery is a choice of where the new nest should run.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

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
   - (none)
5. [app] When the nest a device was signed in to is gone, that device still offers to recover it, from what the device itself holds — `docs/goal/architecture/nest/box-recovery.md` § The plane-era recovery floor
   - `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_when_the_saved_nest_is_dead`
6. [app] A nest identity that was rotated away is never offered for recovery — `docs/goal/architecture/nest/box-recovery.md` § Custody after rotation
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_identity_choice_shows_recover_lost_box_button` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_renders_box_list` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_cloud_method_advances_to_vps_config` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_box_recovery_ui.py::test_nest_recovery_selfhosted_method_advances_to_instructions` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_two_nest_fan_out_fires_and_fresh_device_recovers_both_boxes` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_on_a_reachable_custodied_nest` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_linking_a_box_custodies_it_and_a_fresh_device_recovers_both` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_box_recovery.py::test_rebuilt_box_readopts_custodied_seed_identity` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_box_recovery.py::test_claim_handoff_seed_derives_to_nest_info_identity` | nest (linux): passed |
| 4 | app | (none) | — |
| 5 | app | `tests/e2e-unified/tests/test_box_recovery_two_nest.py::test_launch_recover_button_appears_when_the_saved_nest_is_dead` | linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 6 | app | (none) | — |
<!-- features-render:end -->
