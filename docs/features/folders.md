---
slug: folders
title: Folders
section: your data and devices
goal: docs/goal/ui/folders.md § Goal
guide: docs/guides/cloud-sync.md § The idea: folders
---

## What a user gets

A folder is a set of files your nest keeps: a syncing folder or a backup. Make
one in a short wizard, choose which of your devices take part and how, decide how
many copies the nest keeps and whether the bytes rest on it at all, and see what
each device changed. Conflicts resolve on their own and land on a review list, with
a policy you choose per folder.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

1. [app] The wizard makes a folder — a folder has no type, and what the nest keeps of it is the row's own policy — and a folder can be deleted — `docs/goal/ui/folders.md` § Layout & flow
   - `tests/e2e-unified/tests/test_folders.py::test_folders_empty_state`
   - `tests/e2e-unified/tests/test_folders.py::test_create_folder_via_wizard_sync_mode`
   - `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable`
   - `tests/e2e-unified/tests/test_folders.py::test_delete_folder`
   - `tests/e2e-unified/tests/test_folders.py::test_folders_load_surfaces_no_error`
2. [app] Which paths a folder includes and excludes can be edited and persists — `docs/goal/ui/folders.md` § User actions
   - `tests/e2e-unified/tests/test_folders.py::test_edit_folder_paths`
3. [app] Each device's place in a folder is editable in place — `docs/goal/ui/folders.md` § Layout & flow
   - `tests/e2e-unified/tests/test_folder_place_editor.py::test_device_place_editor_edits_a_seat_in_place`
4. [app] How many snapshots the nest keeps of a folder is set from the folder row — `docs/goal/behavior/backup-restore.md` § 8b. The nest place's snapshot policy
   - `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable`
5. [app] Whether a folder's bytes rest on the nest at all is a choice, armed before it evicts anything — `docs/goal/behavior/file-sync.md` § Content residency
   - `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_residency_control_arms_before_it_evicts`
   - `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_flip_back_to_full_needs_no_confirm`
6. [app] The conflict policy is set per folder and a default stamps new folders — `docs/goal/ui/folders.md` § Conflicts
   - `tests/e2e-unified/tests/test_folders.py::test_folder_conflict_policy_round_trip`
   - `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_stamps_new_sets`
   - `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_survives_relaunch`
7. [app] A resolved conflict shows on the review list and points at the copy that won — `docs/goal/behavior/file-sync.md` § Conflicts
   - `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_resolved_conflict_renders_review_row_and_repoints`
   - `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_unresolved_conflict_row_is_informational`
8. [app] The folder row shows what each device changed — `docs/goal/behavior/sync-engine-deployments.md` § Control Plane Principle
   - `tests/e2e-unified/tests/test_folders.py::test_folder_device_activity_reflects_recorded_changes`
9. [app] How many earlier versions of a file your nest keeps, and for how long, is set per folder, and left blank it keeps them all — `docs/goal/behavior/file-versions.md` § Retention
   - `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable`
10. [app] A new folder starts with the devices you picked while making it, each doing what you said it should do with the folder — `docs/goal/ui/folders.md` § Layout & flow
    - `tests/e2e-unified/tests/test_folder_wizard_places.py::test_a_new_folder_starts_with_the_devices_and_places_picked_in_the_wizard`
11. [nest] A folder set to keep no bytes on your nest refuses to also be served out, whichever of the two you turn on second, and the refusal says what to change — `docs/goal/behavior/file-sync.md` § Content residency
    - `tests/e2e-unified/tests/api/test_folder_residency_serving_refusals.py::test_a_no_rest_folder_refuses_to_be_served_whichever_moves_second`
12. [app] A folder can be set so only one device at a time edits it, and its row says which of your devices is editing it right now, by name — `docs/goal/ui/folders.md` § Exclusive editing
    - `tests/e2e-unified/tests/test_folder_exclusive_editing_control.py::test_exclusive_editing_toggle_and_lease_status`
13. [app] Binding a location on a computer the folder did not include adds that computer to the folder's devices, doing everything a device does by default, and the folder row shows it straight away — `docs/goal/behavior/file-sync.md` § 4. Delivery (Nest → the other devices)
    - `tests/e2e-unified/tests/test_folder_bind_enrols_place.py::test_binding_a_location_gives_a_placeless_device_its_seat`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+2cb6e915.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folders_empty_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_create_folder_via_wizard_sync_mode` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_delete_folder` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folders_load_surfaces_no_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_folders.py::test_edit_folder_paths` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_place_editor.py::test_device_place_editor_edits_a_seat_in_place` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 4 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 5 | app | `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_residency_control_arms_before_it_evicts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_flip_back_to_full_needs_no_confirm` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_conflict_policy_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_stamps_new_sets` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_survives_relaunch` | linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_resolved_conflict_renders_review_row_and_repoints` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 7 | app | `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_unresolved_conflict_row_is_informational` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): error |
| 8 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_device_activity_reflects_recorded_changes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 10 | app | `tests/e2e-unified/tests/test_folder_wizard_places.py::test_a_new_folder_starts_with_the_devices_and_places_picked_in_the_wizard` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_folder_residency_serving_refusals.py::test_a_no_rest_folder_refuses_to_be_served_whichever_moves_second` | nest (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_exclusive_editing_control.py::test_exclusive_editing_toggle_and_lease_status` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_folder_bind_enrols_place.py::test_binding_a_location_gives_a_placeless_device_its_seat` | tui (linux): passed |
<!-- features-render:end -->
