---
slug: snapshots
title: Backups: snapshots
section: your data and devices
goal: docs/goal/ui/backups.md § Goal
guide: docs/guides/cloud-backup.md § Snapshots: the time machine on your nest
---

## What a user gets

Every folder keeps snapshots on your nest. Browse a snapshot's files, download
a single file exactly as it was, check a snapshot's integrity, prune old ones with a
preview first, recover one you pruned, and see when a folder was last backed up.
Deleting a snapshot for good needs a typed confirmation.

## Coverage contract

Stamped 2026-09-19 at f62a5e4e1c.

1. [app] A snapshot lists its files, and the newest one is what "last backed up" reports — `docs/goal/ui/backups.md` § Snapshot-list shape
   - `tests/e2e-unified/tests/test_backups.py::test_snapshot_detail_files_visible`
   - `tests/e2e-unified/tests/test_backups.py::test_last_backed_up_is_the_selected_set_s_newest_snapshot`
   - `tests/e2e-unified/tests/test_backups.py::test_backup_folder_selector_visible`
2. [app] A single file downloads from a snapshot byte for byte — `docs/goal/behavior/backup-restore.md` § 3. Restore: Single File
   - `tests/e2e-unified/tests/test_backups.py::test_snapshot_file_download_button_downloads_sealed_bytes`
   - `tests/e2e-unified/tests/test_backups_download.py::test_download_single_file_bytes_roundtrip`
3. [app] Prune previews before it deletes, a pruned snapshot can be recovered, and deleting for good needs a typed confirmation — `docs/goal/ui/backups.md` § Snapshot-list shape
   - `tests/e2e-unified/tests/test_backups.py::test_prune_previews_before_it_deletes`
   - `tests/e2e-unified/tests/test_backups.py::test_a_soft_deleted_snapshot_can_be_recovered_from_the_app`
   - `tests/e2e-unified/tests/test_backups.py::test_snapshot_immediate_delete`
   - `tests/e2e-unified/tests/test_backups.py::test_snapshot_delete_button_indexed`
4. [app] An integrity check reports its result without raising an error — `docs/goal/ui/backups.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_backups.py::test_completed_check_is_a_result_not_an_error`
5. [nest] Your nest lists, prunes within a hard floor, checks and serves snapshots, and never one person's to another — `docs/goal/behavior/backup-restore.md` § 7. Deletion Safety
   - `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_list_folder_scoped`
   - `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_prune_never_crosses_the_hard_floor`
   - `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_check_integrity_folder_scoped_ok`
   - `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_cross_user_snapshot_get_denied`
   - `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_snapshot_get_lists_the_seeded_file`
   - `tests/e2e-unified/tests/api/test_snapshot_create_idempotency.py::test_repeated_create_restore_cycles`
6. [nest] Files backed up from a computer restore on it byte for byte — `docs/goal/behavior/backup-restore.md` § Restoring Files
   - (none)
7. [app] You take a snapshot of a folder whenever you want one — `docs/goal/ui/backups.md` § Goal
   - `tests/e2e-unified/tests/test_backups.py::test_snapshot_detail_files_visible`
8. [app] Every snapshot row reads as a snapshot — when it was taken, how many files, how big — newest first — `docs/goal/ui/backups.md` § Snapshot-list shape
   - `tests/e2e-unified/tests/test_backups.py::test_every_snapshot_row_reads_as_a_snapshot_newest_first`
9. [app] A snapshot on its way out says so on its own row: when it will go, and how long you can still get it back — `docs/goal/ui/backups.md` § Snapshot-list shape
   - `tests/e2e-unified/tests/test_backups.py::test_a_snapshot_on_its_way_out_says_so_on_its_own_row`
10. [app] A prune that finds nothing to remove says so, and a folder with no retention set says that instead — `docs/goal/ui/backups.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_backups.py::test_a_prune_with_nothing_to_remove_says_which_kind_of_nothing`
11. [nest] A snapshot you delete is not gone at once: it waits two days, then stays recoverable for a month before its data can be released — `docs/goal/behavior/backup-restore.md` § 7. Deletion Safety
    - `tests/e2e-unified/tests/api/test_snapshot_deletion_safety.py::test_delete_waits_two_days_then_stays_recoverable_for_a_month`
12. [nest] A folder you changed and then left alone gets a snapshot by itself, with nothing asked of you — `docs/goal/behavior/backup-restore.md` § Automatic Snapshots
    - `tests/e2e-unified/tests/api/test_snapshot_auto_capture.py::test_a_changed_then_quiet_folder_snapshots_itself`
13. [nest] Two snapshots can be compared to see exactly what changed between them — `docs/goal/behavior/backup-restore.md` § Browsing Snapshots
    - `tests/e2e-unified/tests/api/test_snapshot_diff.py::test_diff_reports_added_removed_and_modified`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+27a1b955 standalone |
| linux | ⚠ partial | 0.1.2-dev+78e73031 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_snapshot_detail_files_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_last_backed_up_is_the_selected_set_s_newest_snapshot` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_folder_selector_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups.py::test_snapshot_file_download_button_downloads_sealed_bytes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups_download.py::test_download_single_file_bytes_roundtrip` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_prune_previews_before_it_deletes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_a_soft_deleted_snapshot_can_be_recovered_from_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_snapshot_immediate_delete` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_snapshot_delete_button_indexed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_completed_check_is_a_result_not_an_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_list_folder_scoped` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_prune_never_crosses_the_hard_floor` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_check_integrity_folder_scoped_ok` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_cross_user_snapshot_get_denied` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_backups.py::test_snapshot_get_lists_the_seeded_file` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_snapshot_create_idempotency.py::test_repeated_create_restore_cycles` | nest (linux): passed |
| 6 | nest | (none) | — |
| 7 | app | `tests/e2e-unified/tests/test_backups.py::test_snapshot_detail_files_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_backups.py::test_every_snapshot_row_reads_as_a_snapshot_newest_first` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_backups.py::test_a_snapshot_on_its_way_out_says_so_on_its_own_row` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_backups.py::test_a_prune_with_nothing_to_remove_says_which_kind_of_nothing` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_snapshot_deletion_safety.py::test_delete_waits_two_days_then_stays_recoverable_for_a_month` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_snapshot_auto_capture.py::test_a_changed_then_quiet_folder_snapshots_itself` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_snapshot_diff.py::test_diff_reports_added_removed_and_modified` | nest (linux): passed |
<!-- features-render:end -->
