---
slug: devices
title: Your devices
section: your data and devices
goal: docs/goal/ui/devices.md § Goal
guide: docs/guides/identity-and-devices.md § Adding a device
---

## What a user gets

The Devices page lists every device signed in as you, exactly as your nest
knows them, marks the one you are holding, shows which folders each takes part in,
and lets you remove one. Your own identity is one tap away to hand to a new device.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] The page lists exactly the devices your nest holds for you, and a new device appears once it registers — `docs/goal/ui/devices.md` § Layout & flow
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_devices_page_matches_nest_roster`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_shows_after_register`
2. [app] The device you are holding is marked, and each device shows its folder roles — `docs/goal/behavior/devices.md` § This-device marker (client-side, no wire change)
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_marks_this_device`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_fileset_role_badge_shows_role_chip`
3. [app] Each device can be removed, and your own identity is one copy away — `docs/goal/ui/devices.md` § User actions
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_remove_button_visible`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_peer_actor_id_copy_button_is_single_instance`
4. [nest] A device that registers for sync is listed for its owner, with its folder roles — `docs/goal/behavior/devices.md` § Listing Devices
   - `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_registered_device_appears_in_ws_rpc_list`
   - `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_device_folder_role_appears_in_ws_rpc_list`
5. [app] The device you are signed in on reads Online, and a device nothing is connected as reads Offline — `docs/goal/behavior/devices.md` § Listing Devices
   - `tests/e2e-unified/tests/test_device_online.py::test_the_signed_in_apps_own_device_reads_online`
6. [app] When your account is at its tier's device limit, the page says so and names both remedies — remove a device here, or ask the admin for a bigger tier — and the notice clears once a slot frees — `docs/goal/ui/devices.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_is_told_on_the_devices_page_and_the_remedy_clears_it`
7. [nest] Your nest refuses a new device past the tier's limit and enrolls nothing, while a device you already have re-registers freely — `docs/goal/behavior/devices.md` § Step 4 — Register for sync
   - `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers`
8. [app] A removal that cannot be done safely deletes nothing and says why — the device in your hand, one the app cannot verify yet, or a row that disagrees with the device's own record — `docs/goal/ui/devices.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_device_removal_refusal.py::test_removing_the_device_in_hand_is_refused_and_deletes_nothing`
9. [app] A device your nest still counts but no row here can name is shown by its fingerprint and can be removed — `docs/goal/architecture/account-data-taxonomy.md` § The generation machinery
   - `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_whose_entry_was_deleted_elsewhere_is_listed_by_fingerprint_and_removed`
   - `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_that_misstates_its_row_is_refused_on_its_row_and_removed_by_its_card`
10. [app] A device of yours that holds no keys is marked relay-only — a fact shown, never a switch — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
11. [app] You ask a friend from your conversations to hold copies for you, and are told what they would see before you confirm — `docs/goal/ui/nests.md` § Trust facet — custody rows
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
12. [app] What you hold for someone is metered against a budget you picked when you accepted, and you can change it here — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
13. [app] An offer to hold copies that you do not want is declined and goes away — `docs/goal/ui/devices.md` § Where logic lives
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_goes_away`
14. [app] Removing a copy you hold for someone frees its space, where pausing only stops it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - (none)
15. [app] A copy you declined or removed does not come back when its owner sends it again, and your pause and allowance survive the owner's refresh — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - (none)
16. [nest] Removing a device ends every sign-in that device made, for good — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+dae82e54 standalone |
| linux | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| windows | ⚠ partial | 0.1.2-dev+4bfe5053.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+6e1708ef standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+4bfe5053.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_devices_page_matches_nest_roster` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_shows_after_register` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_marks_this_device` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_fileset_role_badge_shows_role_chip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_remove_button_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_peer_actor_id_copy_button_is_single_instance` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_registered_device_appears_in_ws_rpc_list` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_device_folder_role_appears_in_ws_rpc_list` | nest (linux): passed, nest (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_device_online.py::test_the_signed_in_apps_own_device_reads_online` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_is_told_on_the_devices_page_and_the_remedy_clears_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_device_removal_refusal.py::test_removing_the_device_in_hand_is_refused_and_deletes_nothing` | linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_whose_entry_was_deleted_elsewhere_is_listed_by_fingerprint_and_removed` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_that_misstates_its_row_is_refused_on_its_row_and_removed_by_its_card` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_goes_away` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | nest | (none) | — |
<!-- features-render:end -->
