---
slug: media
title: Browse, upload and manage your files
section: everyday
goal: docs/goal/ui/media.md § Goal
guide: docs/guides/app-tour.md § Media
---

## What a user gets

Media shows every file in every folder you can read, or one folder at a time.
Upload into a folder, open a file's version history and restore an older one, delete
with a single confirmation, and watch a file that just synced appear without
reloading. Each item says whether it is synced.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] The browser shows all your files across folders, narrows to one folder, and says when a folder is empty — `docs/goal/ui/media.md` § Default view: cross-set all-media
   - `tests/e2e-unified/tests/test_media.py::test_media_explorer_chrome`
   - `tests/e2e-unified/tests/test_media.py::test_all_media_cross_set`
   - `tests/e2e-unified/tests/test_media.py::test_media_empty_state_marks_a_loaded_page_not_a_loading_one`
2. [app] Uploading puts the file in the chosen folder, including a folder you just made — `docs/goal/ui/media.md` § User actions
   - `tests/e2e-unified/tests/test_media.py::test_media_upload_into_selected_set`
   - `tests/e2e-unified/tests/test_media.py::test_upload_into_a_freshly_created_empty_folder`
   - `tests/e2e-unified/tests/test_media.py::test_media_upload_no_set_error`
3. [app] Deleting a file, with one confirmation, removes it from the browser and from your disk; cancelling changes nothing — `docs/goal/ui/media.md` § User actions
   - `tests/e2e-unified/tests/test_media.py::test_media_delete_removes_the_item`
   - `tests/e2e-unified/tests/test_media.py::test_media_delete_removes_the_file_from_disk`
   - `tests/e2e-unified/tests/test_media.py::test_media_delete_cancel_is_a_no_op`
4. [app] A file's version history lists earlier versions and restores one — `docs/goal/behavior/file-versions.md` § Restore
   - `tests/e2e-unified/tests/test_media.py::test_file_version_history_and_restore`
   - `tests/e2e-unified/tests/test_version_prune_recovery.py::test_version_prune_soft_prunes_and_the_ui_recovers`
5. [app] A file that syncs in while you are on the page appears without a reload — `docs/goal/ui/media.md` § Where logic lives
   - `tests/e2e-unified/tests/test_media.py::test_media_page_shows_a_file_recorded_while_it_is_open`
6. [app] Each item shows its sync state — `docs/goal/behavior/file-sync.md` § Per-file sync-status display
   - `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_media_item_shows_synced_badge`
   - `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_apple_media_item_shows_its_engine_sync_state`
   - `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_macos_media_item_of_an_agent_bound_set_shows_synced`
7. [nest] Your nest lists files across every folder you can read and removes a deleted one — `docs/goal/ui/media.md` § State & data shape
   - `tests/e2e-unified/tests/api/test_media_seed.py::test_cross_set_media_seed_lists_and_filters`
8. [app] The browse switches between a list and a grid, and sorts by name, size or date — `docs/goal/ui/media.md` § Layout & flow
   - `tests/e2e-unified/tests/test_media.py::test_media_browse_switches_list_and_grid_and_sorts_by_name_size_and_date`
9. [app] Each item shows a picture of itself, or a placeholder when there is none — `docs/goal/ui/media.md` § Layout & flow
   - `tests/e2e-unified/tests/test_media.py::test_media_item_shows_its_picture_or_a_placeholder`
10. [app] A version that was pruned can still be listed and recovered — `docs/goal/behavior/file-versions.md` § Retention
   - `tests/e2e-unified/tests/test_version_prune_recovery.py::test_version_prune_soft_prunes_and_the_ui_recovers`
11. [app] Pressing upload with no file chosen says so, instead of doing nothing — `docs/goal/ui/media.md` § User actions
   - `tests/e2e-unified/tests/test_media.py::test_media_upload_with_no_file_chosen_says_so`
12. [app] Each item says whether the folder it lives in can be reached right now, separately from whether the file is in sync — `docs/goal/ui/media.md` § Layout & flow
   - `tests/e2e-unified/tests/test_media_source_status.py::test_media_item_says_whether_its_folder_can_be_reached_apart_from_its_sync_state`
   - `tests/e2e-unified/tests/test_media_source_status.py::test_media_item_in_a_full_folder_is_reachable_with_no_seat_connected`
13. [app] Uploading into a folder whose contents stay on your devices is refused with the reason shown, and nothing is uploaded — `docs/goal/ui/media.md` § User actions
   - `tests/e2e-unified/tests/test_media.py::test_upload_into_a_metadata_only_folder_is_refused_with_its_reason`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+e3bf6a64 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_media.py::test_media_explorer_chrome` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_media.py::test_all_media_cross_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_media.py::test_media_empty_state_marks_a_loaded_page_not_a_loading_one` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_media.py::test_media_upload_into_selected_set` | web (linux): failed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_media.py::test_upload_into_a_freshly_created_empty_folder` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_media.py::test_media_upload_no_set_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_media.py::test_media_delete_removes_the_item` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_media.py::test_media_delete_removes_the_file_from_disk` | linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_media.py::test_media_delete_cancel_is_a_no_op` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_media.py::test_file_version_history_and_restore` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_version_prune_recovery.py::test_version_prune_soft_prunes_and_the_ui_recovers` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_media.py::test_media_page_shows_a_file_recorded_while_it_is_open` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_media_item_shows_synced_badge` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_apple_media_item_shows_its_engine_sync_state` | linux (linux): failed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 6 | app | `tests/e2e-unified/tests/test_media_sync_state_badge.py::test_macos_media_item_of_an_agent_bound_set_shows_synced` | — |
| 7 | nest | `tests/e2e-unified/tests/api/test_media_seed.py::test_cross_set_media_seed_lists_and_filters` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_media.py::test_media_browse_switches_list_and_grid_and_sorts_by_name_size_and_date` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_media.py::test_media_item_shows_its_picture_or_a_placeholder` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_version_prune_recovery.py::test_version_prune_soft_prunes_and_the_ui_recovers` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_media.py::test_media_upload_with_no_file_chosen_says_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_media_source_status.py::test_media_item_says_whether_its_folder_can_be_reached_apart_from_its_sync_state` | web (linux): failed, linux (linux): failed, tui (linux): failed |
| 12 | app | `tests/e2e-unified/tests/test_media_source_status.py::test_media_item_in_a_full_folder_is_reachable_with_no_seat_connected` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_media.py::test_upload_into_a_metadata_only_folder_is_refused_with_its_reason` | tui (linux): passed |
<!-- features-render:end -->
