---
slug: backup-destinations-and-restore
title: Backup destinations and restore
section: your data and devices
goal: docs/goal/ui/backups.md § Manage backup destinations
guide: docs/guides/cloud-backup.md § Backup destinations: surviving the loss of the nest itself
---

## What a user gets

Your nest can copy its sealed backups somewhere else: another nest, a device of
yours, or a friend's device, which holds only what it cannot read. The Backups page
shows when each destination last received a copy, audits it and warns when one goes
dark or its store rots, attaches destinations to individual folders, and restores
mail from a snapshot with a history of every restore.

## Coverage contract

Stamped 2026-09-19 at f62a5e4e1c.

1. [app] Destinations are added, edited and removed, including a device of yours — `docs/goal/ui/backups.md` § Manage backup destinations
   - `tests/e2e-unified/tests/test_backups.py::test_backup_destination_crud`
   - `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination`
2. [app] A destination reports when it last received a copy, and a friend's device checks in as it pulls — `docs/goal/behavior/backup-destinations.md` § Per-destination status read
   - `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass`
   - `tests/e2e-unified/tests/test_backups.py::test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it`
3. [app] The audit advances when a destination passes and raises an alert when it goes dark or its store rots — `docs/goal/ui/backups.md` § Audit-alert surface
   - `tests/e2e-unified/tests/test_backups.py::test_backup_audit_pass_advances_the_last_checked_row`
   - `tests/e2e-unified/tests/test_backups.py::test_backup_audit_alerts_when_the_destination_goes_dark`
   - `tests/e2e-unified/tests/test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner`
   - `tests/e2e-unified/tests/test_backup_audit_observation.py::test_rendering_the_conversation_list_feeds_the_audit_observation`
   - `tests/e2e-unified/tests/test_backup_audit_folder_replica.py::test_a_delisted_covered_folder_row_is_caught_by_the_replica_anchor`
4. [app] Removing a friend-held destination without reclaiming leaves an orphaned store you can still reclaim — `docs/goal/behavior/backup-destinations.md` § Third destination kind — client device as custodian
   - `tests/e2e-unified/tests/test_backups.py::test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store`
5. [app] A friend holds sealed copies for you after a two-sided ceremony, on their device or a nest they pick — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
6. [app] A destination attaches to one folder and detaches again — `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage — destination places
   - `tests/e2e-unified/tests/test_folder_destination_places.py::test_folder_destination_place_attach_and_detach`
7. [app] Mail restores from a snapshot, every restore is listed, and a calendar that diverged after a restore is flagged with details — `docs/goal/ui/backups.md` § Restore from backup destination
   - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_history_renders`
   - `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail`
   - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_banner_and_modal`
8. [nest] Your nest snapshots and restores mail, records the history, and detects a diverged calendar — `docs/goal/behavior/backup-restore.md` § 6. Message-kind restore
   - `tests/e2e-unified/tests/api/test_dr_restore.py::test_mail_snapshot_create_restore_and_caldav_divergence`
   - `tests/e2e-unified/tests/api/test_dr_restore.py::test_restore_message_kind_confirm_mismatch_rejected`
9. [app] A standing warning tells you when every destination you have is one of your own devices, and clears once another kind holds a copy too — `docs/goal/behavior/backup-destinations.md` § Third destination kind — client device as custodian
   - `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination`
10. [app] Each destination says what kind it is, and a device of yours also shows how much it holds against the cap you set — `docs/goal/ui/backups.md` § Manage backup destinations
    - `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination`
11. [app] Each destination shows how much is still waiting to reach it — `docs/goal/ui/backups.md` § Element IDs
    - `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass`
12. [app] After losing your nest you pick which destination holds the copy and restore from it, from the app — `docs/goal/ui/backups.md` § Restore after losing the nest
    - `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one`
13. [app] A restore runs only after you type the snapshot's own id back — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail`
14. [app] A restore in progress says which step it is on, and says when it is done — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_reports_progress_and_says_when_it_is_done`
15. [app] You see who holds sealed copies for you and when they last confirmed they still have them, and you can take that back — `docs/goal/ui/devices.md` § Custody facet
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
16. [app] An offer to hold copies for someone states what your device would see before you accept, and you can stop holding at any time — `docs/goal/ui/devices.md` § Custody facet
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
17. [nest] A conversation is snapshotted and restored the way mail is, and the restore is listed like any other — `docs/goal/behavior/backup-restore.md` § 6. Message-kind restore
    - `tests/e2e-unified/tests/api/test_dr_restore.py::test_conv_snapshot_create_restore_and_history`
18. [app] After a restore, a mail app that reconnected holding newer state is flagged with what it lost, as a calendar app is — `docs/goal/ui/backups.md` § Restore divergence
    - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_flags_a_reconnected_mail_app_as_it_does_a_calendar`
19. [app] A restore that could not bring your account's configuration back says so once it is done, and a complete one says nothing extra — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_without_the_account_configuration_says_so_and_a_complete_one_does_not`
20. [app] After your nest's identity is rotated, its backups to other nests resume with nothing for you to do but open the Backups page — `docs/goal/architecture/segment-backup-protocol.md` § Cross-location backup protocol
    - `tests/e2e-unified/tests/test_backup_rotated_source_journey.py::test_a_rotated_source_box_keeps_backing_up_with_no_gesture`

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
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_crud` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups.py::test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_audit_pass_advances_the_last_checked_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_audit_alerts_when_the_destination_goes_dark` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backup_audit_observation.py::test_rendering_the_conversation_list_feeds_the_audit_observation` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backup_audit_folder_replica.py::test_a_delisted_covered_folder_row_is_caught_by_the_replica_anchor` | linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_folder_destination_places.py::test_folder_destination_place_attach_and_detach` | web (linux): passed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_history_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_banner_and_modal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 8 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_mail_snapshot_create_restore_and_caldav_divergence` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_restore_message_kind_confirm_mismatch_rejected` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): failed |
| 13 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_reports_progress_and_says_when_it_is_done` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_conv_snapshot_create_restore_and_history` | nest (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_flags_a_reconnected_mail_app_as_it_does_a_calendar` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_without_the_account_configuration_says_so_and_a_complete_one_does_not` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 20 | app | `tests/e2e-unified/tests/test_backup_rotated_source_journey.py::test_a_rotated_source_box_keeps_backing_up_with_no_gesture` | tui (linux): passed |
<!-- features-render:end -->
