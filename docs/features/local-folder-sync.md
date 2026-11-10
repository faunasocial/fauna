---
slug: local-folder-sync
title: Keep a folder on your computer in sync
section: your data and devices
goal: docs/goal/behavior/file-sync.md § User Experience
guide: docs/guides/cloud-sync.md § What syncing feels like
absences:
  web: "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
  ios: "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
  android: "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
---

## What a user gets

Bind a folder on your computer — your own, or one a friend shared with you as a
writer — and a small helper keeps it in sync in the background: a file you drop in
is on your nest and on your other computers moments later, edits and deletes travel
both ways, and a folder that vanishes wholesale is held for your say-so rather than
deleted everywhere. The app shows the helper's health, and changing which folders
sync takes effect at once.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

1. [app] A file dropped into a bound folder is uploaded by the background helper — `docs/goal/behavior/file-sync.md` § Files Appear Automatically
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_agent_uploads_bound_folder_file`
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_windows_agent_uploads_bound_folder_file`
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_macos_agent_uploads_bound_folder_file`
2. [app] Three computers on three machines converge on one folder — `docs/goal/behavior/file-sync.md` § User Experience
   - `tests/e2e-unified/tests/test_filesync_multiseat_live.py::test_three_seat_live_sync`
3. [app] The app shows the helper running — `docs/goal/architecture/apps/sync-agent.md` § Local agent health (app UI)
   - `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login`
   - `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login_windows_real`
   - `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login_macos_real`
4. [app] Adding or removing a bound folder takes effect at once, without signing out — `docs/goal/architecture/apps/linux.md` § File Sync
   - `tests/e2e-unified/tests/test_sync_live_apply.py::test_sync_location_map_edits_apply_live`
5. [app] A folder that vanishes wholesale is held, shown to you, and propagated only when you confirm — `docs/goal/behavior/delete-propagation.md` § How a delete travels
   - `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_a_vanished_folder_is_held_surfaced_and_propagated_only_on_confirm`
   - `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_an_on_demand_root_holds_only_its_hydrated_rows`
6. [app] A file you edited is never overwritten by a delete from elsewhere, and re-binding a folder with history keeps your local file — `docs/goal/behavior/file-sync.md` § Conflicts
   - `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_declined_delete_of_a_mid_debounce_edit_keeps_the_file`
   - `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_windows_declined_delete_of_a_mid_debounce_edit_keeps_the_file`
   - `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_macos_declined_delete_of_a_mid_debounce_edit_keeps_the_file`
   - `tests/e2e-unified/tests/test_filesync_bind_history.py::test_fresh_bind_onto_tombstoned_history_preserves_local_file`
   - `tests/e2e-unified/tests/test_filesync_bind_history.py::test_fresh_bind_onto_a_same_named_never_deleted_file_keeps_syncing`
7. [app] Two devices converge, including large files, shrinking files and concurrent edits, and a device offline for a while catches up — `docs/goal/behavior/file-sync.md` § Technical Flow
   - `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_restarted_seat_receives_what_it_missed`
8. [app] A folder whose bytes must not rest on the nest still syncs between devices, through the nest, without the nest keeping a copy — `docs/goal/behavior/file-sync.md` § Relay serving
   - `tests/e2e-unified/tests/test_filesync_metadata_only_relay.py::test_a_second_device_receives_a_metadata_only_folders_file_from_the_first`
9. [nest] The installed helper keeps running under the desktop's own supervision — `docs/goal/architecture/installers/linux-desktop.md` § 3. Flatpak
   - (maintainer-only)
10. [app] Paths you exclude from a folder stop syncing to this computer, and clearing the exclusion lets them through again, without restarting anything — `docs/goal/behavior/file-sync.md` § Status
   - `tests/e2e-unified/tests/test_folder_selective_sync_live.py::test_an_excluded_path_stops_syncing_until_the_exclusion_is_cleared`
11. [app] A device set to keep an archive keeps the files your other devices delete — `docs/goal/behavior/file-sync.md` § Technical Flow
   - `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_backup_seat_keeps_a_file_the_source_deleted`
12. [nest] Files matched by a folder's own ignore list are never uploaded, neither as they change nor by a later rescan — `docs/goal/behavior/sync-engine-deployments.md` § Control Plane Principle
   - `tests/e2e-unified/tests/api/test_folder_ignore_list.py::test_ignored_files_are_never_uploaded_on_a_change_or_a_rescan`
13. [app] One change a device cannot apply never holds up the rest: the others keep arriving, and the stuck one is listed for you to review — `docs/goal/behavior/file-sync.md` § Technical Flow
   - `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_permanently_unappliable_change_is_recorded_and_never_blocks_the_rest`
14. [nest] Only one of your devices at a time can hold a folder for exclusive editing; a second device asking while it is held is refused — `docs/goal/behavior/file-sync.md` § Folders
   - `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py::test_only_one_device_at_a_time_holds_a_folders_exclusive_lease`
15. [app] A device you tell not to take changes stops receiving them, and picks up exactly where it stopped when you let it take them again — `docs/goal/behavior/file-sync.md` § Technical Flow
   - `tests/e2e-unified/tests/test_folder_place_accepts_hold.py::test_a_device_told_not_to_take_changes_resumes_exactly_where_it_stopped`
16. [app] When part of a folder cannot be read, the folder says so, offers nothing to confirm, and no file is deleted anywhere — `docs/goal/behavior/delete-propagation.md` § How a delete travels
    - `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_an_unreadable_subtree_is_surfaced_and_nothing_is_deleted`
17. [app] A folder binds to a folder on your computer, and a typed path is not lost when you save — `docs/goal/ui/folders.md` § Binding (local-folder, desktop only)
    - `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder`
    - `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder_windows`
    - `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder_macos`
    - `tests/e2e-unified/tests/test_folders.py::test_folder_save_paths_does_not_drop_pending_location_path`
18. [app] A writer binds the shared folder to their own computer — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folders.py::test_writer_member_binds_location`
19. [app] Two or three of your computers converge on one folder — new files, edits, shrinking and large files, renames, new subfolders, deletes both ways and edits made at the same moment — `docs/goal/behavior/file-sync.md` § User Experience
    - `tests/e2e-unified/tests/test_filesync_seats.py::test_seats_converge`
    - `tests/e2e-unified/tests/test_filesync_seats.py::test_seat_scenarios`
20. [app] The background sync helper never holds your identity key — `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model
    - `tests/e2e-unified/tests/test_sync_helper_holds_no_identity_key.py::test_the_sync_helper_never_holds_the_identity_key`
21. [app] Files your folder's ignore rules leave out — names in its own ignore list, temporary files and hidden files — stay on the computer that has them, while the files beside them still reach your other computers — `docs/goal/behavior/sync-engine-deployments.md` § Control Plane Principle
    - `tests/e2e-unified/tests/test_filesync_seats.py::test_seat_ignore`
22. [app] When the nest no longer holds the helper's sign-in, the app says the helper is not enrolled, until you sign in again — `docs/goal/architecture/apps/sync-agent.md` § Local agent health (app UI)
    - `tests/e2e-unified/tests/test_sync_agent_not_enrolled.py::test_a_revoked_grant_reads_not_enrolled_until_the_user_signs_in_again`
23. [app] The helper keeps syncing while the app is closed, for days, renewing its own sign-in without the app or your identity key — `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model
    - `tests/e2e-unified/tests/test_sync_agent_survives_tui_quit.py::test_the_helper_keeps_syncing_and_renews_its_own_sign_in_after_tui_quits`
24. [app] Signing out stops the helper syncing that identity's folders at once — `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model
   - `tests/e2e-unified/tests/test_sync_agent_sign_out_stops_folders.py::test_signing_out_stops_the_helper_serving_that_identitys_folders`
25. [app] Two devices editing the same text file while apart merge cleanly; where edits overlap, or the file is not text, the newest wins without conflict markers and the losing version is kept in version history — `docs/goal/behavior/file-sync.md` § Conflicts
   - `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_edits_made_apart_merge_cleanly_or_resolve_newest_wins_without_markers`
26. [app] A change one of your devices could not apply is listed for review on every app, naming the device, and clears once a later version lands; until then no later change silently overwrites your unpublished edits — `docs/goal/behavior/file-sync.md` § Conflicts
   - `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_permanently_unappliable_change_is_recorded_and_never_blocks_the_rest`
27. [app] Re-binding a computer's folder to a different set, or binding one path to two sets, loses no edit and skips no file — `docs/goal/behavior/file-sync.md` § Conflicts
   - `tests/e2e-unified/tests/test_filesync_rebind_lossless.py::test_rebinding_a_folder_to_another_set_loses_no_edit_and_skips_no_file`
   - `tests/e2e-unified/tests/test_filesync_rebind_lossless.py::test_one_path_bound_to_two_sets_loses_no_edit_and_skips_no_file`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ⚠ partial | 0.1.5-dev+381efcf2 standalone |
| windows | ⚠ partial | 0.1.5-dev+381efcf2 standalone |
| macos | ⚠ partial | 0.1.5-dev+381efcf2 standalone |
| ios | — absent | |
| android | — absent | |
| tui | ⚠ partial | 0.1.5-dev+381efcf2 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_agent_uploads_bound_folder_file` | linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_windows_agent_uploads_bound_folder_file` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_macos_agent_uploads_bound_folder_file` | macos (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_filesync_multiseat_live.py::test_three_seat_live_sync` | tui (linux): failed |
| 3 | app | `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login_windows_real` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sync_agent_status.py::test_sync_agent_status_reads_running_after_login_macos_real` | macos (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_sync_live_apply.py::test_sync_location_map_edits_apply_live` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_a_vanished_folder_is_held_surfaced_and_propagated_only_on_confirm` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_an_on_demand_root_holds_only_its_hydrated_rows` | windows (windows): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_declined_delete_of_a_mid_debounce_edit_keeps_the_file` | linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_windows_declined_delete_of_a_mid_debounce_edit_keeps_the_file` | windows (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_filesync_delete_declined_debounce.py::test_macos_declined_delete_of_a_mid_debounce_edit_keeps_the_file` | macos (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_filesync_bind_history.py::test_fresh_bind_onto_tombstoned_history_preserves_local_file` | linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_filesync_bind_history.py::test_fresh_bind_onto_a_same_named_never_deleted_file_keeps_syncing` | linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_restarted_seat_receives_what_it_missed` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_filesync_metadata_only_relay.py::test_a_second_device_receives_a_metadata_only_folders_file_from_the_first` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | nest | (maintainer-only) | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_folder_selective_sync_live.py::test_an_excluded_path_stops_syncing_until_the_exclusion_is_cleared` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_backup_seat_keeps_a_file_the_source_deleted` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_folder_ignore_list.py::test_ignored_files_are_never_uploaded_on_a_change_or_a_rescan` | nest (linux): passed, nest (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_permanently_unappliable_change_is_recorded_and_never_blocks_the_rest` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py::test_only_one_device_at_a_time_holds_a_folders_exclusive_lease` | nest (linux): passed, nest (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_folder_place_accepts_hold.py::test_a_device_told_not_to_take_changes_resumes_exactly_where_it_stopped` — requires `folder-place-accepts-hold` (not built: web, windows, ios, android, android-kids, ios-kids) | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_filesync_mass_delete_floor.py::test_an_unreadable_subtree_is_surfaced_and_nothing_is_deleted` — requires `folder-location-unreadable` (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder_windows` | windows (windows): passed |
| 17 | app | `tests/e2e-unified/tests/test_folders.py::test_bind_location_nested_under_folder_macos` | macos (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_save_paths_does_not_drop_pending_location_path` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_folders.py::test_writer_member_binds_location` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 19 | app | `tests/e2e-unified/tests/test_filesync_seats.py::test_seats_converge` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 19 | app | `tests/e2e-unified/tests/test_filesync_seats.py::test_seat_scenarios` | linux (linux): passed, windows (windows): failed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 20 | app | `tests/e2e-unified/tests/test_sync_helper_holds_no_identity_key.py::test_the_sync_helper_never_holds_the_identity_key` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 21 | app | `tests/e2e-unified/tests/test_filesync_seats.py::test_seat_ignore` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_sync_agent_not_enrolled.py::test_a_revoked_grant_reads_not_enrolled_until_the_user_signs_in_again` | tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_sync_agent_survives_tui_quit.py::test_the_helper_keeps_syncing_and_renews_its_own_sign_in_after_tui_quits` | tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_sync_agent_sign_out_stops_folders.py::test_signing_out_stops_the_helper_serving_that_identitys_folders` | tui (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_edits_made_apart_merge_cleanly_or_resolve_newest_wins_without_markers` | macos (macos): passed, tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_filesync_seat_catchup.py::test_a_permanently_unappliable_change_is_recorded_and_never_blocks_the_rest` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 27 | app | `tests/e2e-unified/tests/test_filesync_rebind_lossless.py::test_rebinding_a_folder_to_another_set_loses_no_edit_and_skips_no_file` | tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_filesync_rebind_lossless.py::test_one_path_bound_to_two_sets_loses_no_edit_and_skips_no_file` | tui (linux): passed |
<!-- features-render:end -->
