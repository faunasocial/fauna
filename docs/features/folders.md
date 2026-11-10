---
slug: folders
title: Folders
section: your data and devices
goal: docs/goal/ui/folders.md § Goal
guide: docs/guides/cloud-sync.md § The idea: folders
absences:
  web (outcome 13): "docs/goal/ui/folders.md § Layout & flow"
  ios (outcome 13): "docs/goal/architecture/apps/ios.md § Key Differences from macOS"
---

## What a user gets

A folder holds files your nest looks after; it has no fixed type: you say what
each of your devices does with it and who can see it. Make one in a short
wizard, choose which of your devices take part and how, decide how many copies
the nest keeps and whether it keeps the files' contents at all or only passes
them between your devices, and see what each device changed. Conflicts resolve
on their own and land on a review list, with a policy you choose per folder.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

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
   - `tests/e2e-unified/tests/test_folder_nest_place.py::test_a_refresh_keeps_staged_nest_place_edits`
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
    - `tests/e2e-unified/tests/api/test_folder_residency_serving_refusals.py::test_a_public_folder_is_never_metadata_only_in_either_order`
12. [app] A folder can be set so only one device at a time edits it, and its row says which of your devices is editing it right now, by name — `docs/goal/ui/folders.md` § Exclusive editing
    - `tests/e2e-unified/tests/test_folder_exclusive_editing_control.py::test_exclusive_editing_toggle_and_lease_status`
13. [app] Binding a location on a computer the folder did not include adds that computer to the folder's devices, doing everything a device does by default, and the folder row shows it straight away — `docs/goal/behavior/file-sync.md` § 4. Delivery (Nest → the other devices)
    - `tests/e2e-unified/tests/test_folder_bind_enrols_place.py::test_binding_a_location_gives_a_placeless_device_its_seat`
14. [app] An error a folder row's own background read put up while the nest was down clears by itself once the nest answers again, and never takes a failed action's message with it — `docs/goal/ui/folders.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_folder_row_read_error_retires.py::test_a_row_read_error_painted_in_a_nest_gap_retires_after_the_nest_returns`
15. [app] The default conflict policy for new folders is the same on every device of yours — `docs/goal/architecture/account-runtime.md` § Implementation status today
   - `tests/e2e-unified/tests/test_folders.py::test_the_default_conflict_policy_is_the_same_on_another_of_your_devices`
16. [app] A folder made or changed on another of your devices shows on an open Folders page without leaving it — `docs/goal/architecture/account-runtime.md` § Multi-instance concurrency
   - `tests/e2e-unified/tests/test_folders.py::test_a_folder_made_on_another_of_your_devices_shows_on_an_open_folders_page`
17. [nest] Your file and folder names rest sealed on your nest, so its administrator cannot read them, and a record arriving without its seal is refused — `docs/goal/architecture/encryption-at-rest.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_folder_names_rest_sealed.py::test_file_and_folder_names_rest_sealed_and_an_unsealed_record_is_refused`
18. [app] Any combination of a device's receive and serve flags can be set and is kept exactly as set, and a file dropped into an archive device's directory syncs like any other — `docs/goal/behavior/folders.md` § Implementation status today
   - `tests/e2e-unified/tests/test_folder_place_flags_kept.py::test_every_place_flag_combination_is_kept_exactly_as_set`
   - `tests/e2e-unified/tests/test_folder_place_flags_kept.py::test_a_file_dropped_into_an_archive_seats_directory_syncs`
19. [app] A folder you leave on one device does not come back on your other devices — `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync
   - `tests/e2e-unified/tests/test_folder_left_stays_gone.py::test_a_folder_left_on_one_device_does_not_come_back_on_the_other`
20. [app] In a shared folder the version that lost a conflict is kept by its author and shows on the review list, and picking the other version points only at a version your nest still holds — `docs/goal/ui/folders.md` § Conflicts
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_concurrent_edit_is_reviewed_on_the_losing_seat_and_its_version_restored`
   - `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_a_repoint_to_a_version_the_nest_does_not_hold_changes_nothing`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| linux | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| windows | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folders_empty_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_create_folder_via_wizard_sync_mode` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_delete_folder` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folders_load_surfaces_no_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_folders.py::test_edit_folder_paths` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_place_editor.py::test_device_place_editor_edits_a_seat_in_place` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_a_refresh_keeps_staged_nest_place_edits` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_residency_control_arms_before_it_evicts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_residency_control.py::test_the_flip_back_to_full_needs_no_confirm` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_conflict_policy_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_stamps_new_sets` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_sync_default_conflict_policy_survives_relaunch` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_resolved_conflict_renders_review_row_and_repoints` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): failed |
| 7 | app | `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_unresolved_conflict_row_is_informational` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): failed |
| 8 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_device_activity_reflects_recorded_changes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_folder_nest_place.py::test_nest_place_policy_is_reachable_editable_and_clearable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_folder_wizard_places.py::test_a_new_folder_starts_with_the_devices_and_places_picked_in_the_wizard` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_folder_residency_serving_refusals.py::test_a_no_rest_folder_refuses_to_be_served_whichever_moves_second` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_folder_residency_serving_refusals.py::test_a_public_folder_is_never_metadata_only_in_either_order` | nest (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_exclusive_editing_control.py::test_exclusive_editing_toggle_and_lease_status` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_folder_bind_enrols_place.py::test_binding_a_location_gives_a_placeless_device_its_seat` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | app | absent by design on web, ios, ios-kids | — |
| 14 | app | `tests/e2e-unified/tests/test_folder_row_read_error_retires.py::test_a_row_read_error_painted_in_a_nest_gap_retires_after_the_nest_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_folders.py::test_the_default_conflict_policy_is_the_same_on_another_of_your_devices` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_folders.py::test_a_folder_made_on_another_of_your_devices_shows_on_an_open_folders_page` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_folder_names_rest_sealed.py::test_file_and_folder_names_rest_sealed_and_an_unsealed_record_is_refused` | nest (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_folder_place_flags_kept.py::test_every_place_flag_combination_is_kept_exactly_as_set` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_folder_place_flags_kept.py::test_a_file_dropped_into_an_archive_seats_directory_syncs` | linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_folder_left_stays_gone.py::test_a_folder_left_on_one_device_does_not_come_back_on_the_other` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_concurrent_edit_is_reviewed_on_the_losing_seat_and_its_version_restored` — requires `group-set-feed` (not built: web, ios, android, android-kids, ios-kids) | tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_devices_conflicts.py::TestConflictReviewList::test_a_repoint_to_a_version_the_nest_does_not_hold_changes_nothing` | tui (linux): passed |
<!-- features-render:end -->
