---
slug: files-on-demand
title: Files on demand
section: your data and devices
goal: docs/goal/behavior/on-demand-files.md § On-Demand Files (Placeholders)
guide: docs/guides/cloud-sync.md § On-demand files ("free up space")
absences:
  web: "docs/goal/behavior/on-demand-files.md § On-Demand Files (Placeholders)"
---

## What a user gets

Where the operating system has cloud files, a synced folder can show every file
as a placeholder that downloads when you open it, so a big folder takes no space
until you need it. It is still two-way: a file you change uploads all the same.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

1. [app] Whether a folder is kept in full on this device or fetched on demand is your call per folder, and the app remembers it — `docs/goal/behavior/on-demand-files.md` § On-Demand Files (Placeholders)
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_defaults_on_and_a_flip_survives_re_entry`
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_yields_to_a_bound_folder`
   - `tests/e2e-unified/tests/test_folder_location_mode_toggle.py::test_folder_location_mode_toggle_flip_survives_re_entry`
2. [nest] Placeholders list, then download their bytes on open — `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path`
   - `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_eviction_ios`
   - `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_read_of_a_fetched_body_records_nothing_ios`
3. [app] A file you change inside an on-demand folder uploads like any other, and a file you put there joins the folder — `docs/goal/behavior/on-demand-files.md` § Sync direction
   - `tests/e2e-unified/tests/test_folder_on_demand_read_path_linux.py::test_on_demand_folder_is_read_and_written_through_its_mount`
   - `tests/e2e-unified/tests/test_folder_on_demand_read_path_windows.py::test_on_demand_folder_is_written_through_its_cloud_files_root`
4. [app] Freeing a file's space keeps the file listed where it was and never deletes it from your nest or your other devices — `docs/goal/behavior/on-demand-files.md` § A placeholder is
   - `tests/e2e-unified/tests/test_folder_location_free_space.py::test_freeing_keeps_the_file_listed_and_never_deletes_it`
5. [app] A change that has not reached your nest yet cannot have its space freed: your copy stays until it is safely recorded — `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/test_folder_location_free_space.py::test_an_unrecorded_change_is_kept`
6. [app] You can keep several folders on demand at once, and switching one on or off leaves the others working — `docs/goal/behavior/on-demand-files.md` § On-Demand Files (Placeholders)
   - `tests/e2e-unified/tests/test_folder_location_free_space.py::test_several_on_demand_folders_each_switch_leaves_the_others_working`
7. [nest] A file added from another device shows up in your on-demand folder by itself — `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path`
8. [app] Turning on-demand off for a folder never destroys a change that had not uploaded yet — `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/test_folder_location_free_space.py::test_turning_on_demand_off_keeps_an_unsent_change`
9. [app] Showing a folder on demand on a device the folder did not include adds that device to the folder's devices once, doing everything a device does by default — `docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once`
   - `tests/e2e-unified/tests/test_folder_location_on_demand_enrol.py::test_showing_a_folder_on_demand_enrols_a_place_less_device_once`

10. [app] A folder someone shared with you is shown on demand as well, and its switch only hides or shows it on this device: it never changes the folder's devices — `docs/goal/behavior/on-demand-files-shared-sets.md` § Shared sets on a capability host
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_a_member_row_is_hide_only`
11. [app] In an on-demand folder, a file this device has not opened takes no space, and opening it downloads its bytes — `docs/goal/behavior/on-demand-files-linux.md` § Linux FUSE binding
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open`
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_tui_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open`
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_windows.py::test_a_file_from_another_device_lists_as_a_cloud_only_placeholder_and_hydrates_on_open`
12. [app] A file added from another of your devices shows up in your on-demand folder without you doing anything — `docs/goal/behavior/on-demand-files-linux.md` § Linux FUSE binding
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open`
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_tui_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open`
   - `tests/e2e-unified/tests/test_folder_on_demand_hydrate_windows.py::test_a_file_from_another_device_lists_as_a_cloud_only_placeholder_and_hydrates_on_open`
13. [nest] In a folder whose nest keeps no file bytes, a file you wrote on this device stays on it and opens with no other device answering — `docs/goal/behavior/relay-serving.md` § Relay serving
   - `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_own_record_reopens_after_eviction_ios`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ⚠ partial | 0.1.5-dev+aaa3f320 standalone |
| windows | ⚠ partial | 0.1.2-dev+fccdae11.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+790eb749 standalone |
| ios | ⚠ partial | 0.1.5-dev+2922f9ed standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5-dev+02e5676e standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_defaults_on_and_a_flip_survives_re_entry` | macos (macos): passed, ios (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_yields_to_a_bound_folder` | macos (macos): passed, ios (macos): skipped |
| 1 | app | `tests/e2e-unified/tests/test_folder_location_mode_toggle.py::test_folder_location_mode_toggle_flip_survives_re_entry` | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path` | nest (macos): failed |
| 2 | nest | `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_eviction_ios` | nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_read_of_a_fetched_body_records_nothing_ios` | nest (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_on_demand_read_path_linux.py::test_on_demand_folder_is_read_and_written_through_its_mount` | linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_on_demand_read_path_windows.py::test_on_demand_folder_is_written_through_its_cloud_files_root` | windows (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_location_free_space.py::test_freeing_keeps_the_file_listed_and_never_deletes_it` — requires `folder-location-free-space-button` (not built: web, macos, ios, android) | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_location_free_space.py::test_an_unrecorded_change_is_kept` — requires `folder-location-free-space-button` (not built: web, macos, ios, android) | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_folder_location_free_space.py::test_several_on_demand_folders_each_switch_leaves_the_others_working` | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path` | nest (macos): failed |
| 8 | app | `tests/e2e-unified/tests/test_folder_location_free_space.py::test_turning_on_demand_off_keeps_an_unsent_change` | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): failed |
| 9 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once` | macos (macos): passed, ios (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_folder_location_on_demand_enrol.py::test_showing_a_folder_on_demand_enrols_a_place_less_device_once` | linux (linux): passed, tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_a_member_row_is_hide_only` | macos (macos): passed, ios (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open` | linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_tui_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open` | tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_windows.py::test_a_file_from_another_device_lists_as_a_cloud_only_placeholder_and_hydrates_on_open` | windows (windows): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open` | linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_linux.py::test_tui_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open` | tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_on_demand_hydrate_windows.py::test_a_file_from_another_device_lists_as_a_cloud_only_placeholder_and_hydrates_on_open` | windows (windows): passed, tui (windows): passed |
| 13 | nest | `tests/e2e-unified/tests/platform/ios/test_file_provider_eviction.py::test_file_provider_own_record_reopens_after_eviction_ios` | nest (macos): passed |
<!-- features-render:end -->
