---
slug: backup-destinations-and-restore
title: Backup destinations and restore
section: your data and devices
goal: docs/goal/ui/backups.md § Manage backup destinations
guide: docs/guides/cloud-backup.md § Backup destinations: surviving the loss of the nest itself
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Your nest can copy its encrypted backups somewhere else: another nest, yours or
a friend's, or a device of yours. Another nest cannot read what it holds, apart
from a folder you have made public; a copy on a device of yours can be read by
anyone who can unlock that device. The Backups page shows when each destination
last received a copy, audits it and warns when one goes dark or its store rots,
restores your mail from a snapshot your nest keeps, with a history of every
restore, and, if you lose your nest, brings your mail, calendar, contacts and
posts, and any folder you attached, back from a destination's copy — from
another nest's copy once the lost nest is rebuilt with its own identity; a
folder's own settings attach a destination to it.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

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
   - `tests/e2e-unified/tests/test_backups.py::test_removing_a_custodian_with_the_opt_in_frees_the_local_store`
5. [app] A friend holds sealed copies for you after a two-sided ceremony, on their device or a nest they pick — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
6. [app] A destination attaches to one folder and detaches again — `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage — destination places
   - `tests/e2e-unified/tests/test_folder_destination_places.py::test_folder_destination_place_attach_and_detach`
7. [app] A mail snapshot your nest holds restores from the Backups page, every restore is listed, and a calendar that diverged after a restore is flagged with details — `docs/goal/ui/backups.md` § Restore from backup destination
   - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_history_renders`
   - `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail`
   - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_banner_and_modal`
8. [nest] A mail snapshot your nest holds restores, the restore is recorded in the history, and a calendar that diverged is detected — `docs/goal/behavior/backup-restore.md` § 6. Message-kind restore
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
    - `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_a_surviving_nest_destination_restores_the_mail`
13. [app] A restore runs only after you type the snapshot's own id back — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail`
14. [app] A restore in progress says which step it is on, and says when it is done — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_reports_progress_and_says_when_it_is_done`
15. [app] You see who holds sealed copies for you and when they last confirmed they still have them, and you can take that back — `docs/goal/ui/devices.md` § Custody facet
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custody_minted_on_another_device_renders_and_revokes_here`
16. [app] An offer to hold copies for someone states what your device would see before you accept, and you can stop holding at any time — `docs/goal/ui/devices.md` § Custody facet
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
17. [nest] A conversation snapshot your nest holds restores the way a mail one does, and the restore is listed like any other — `docs/goal/behavior/backup-restore.md` § 6. Message-kind restore
    - `tests/e2e-unified/tests/api/test_dr_restore.py::test_conv_snapshot_create_restore_and_history`
18. [app] After a restore, a mail app that reconnected holding newer state is flagged with what it lost, as a calendar app is — `docs/goal/ui/backups.md` § Restore divergence
    - `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_flags_a_reconnected_mail_app_as_it_does_a_calendar`
19. [app] A restore that could not bring your account's configuration back says so once it is done, and a complete one says nothing extra — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_without_the_account_configuration_says_so_and_a_complete_one_does_not`
20. [app] After your nest's identity is rotated, its backups to other nests resume with nothing for you to do but open the Backups page — `docs/goal/architecture/segment-backup-protocol.md` § Cross-location backup protocol
    - `tests/e2e-unified/tests/test_backup_rotated_source_journey.py::test_a_rotated_source_box_keeps_backing_up_with_no_gesture`
21. [app] When your nest goes back to an older copy of itself, the backup's banner offers to recover what it lost, and every lost message comes back once, where it was — `docs/goal/ui/backups.md` § Audit-alert surface
    - `tests/e2e-unified/tests/test_backups.py::test_a_nest_that_went_back_in_time_gets_its_lost_mail_back_from_the_backup`
22. [app] When your nest goes back to an older copy of itself and one of your own devices holds the backup, that device's banner offers to recover what it lost from its own copy, and every lost message comes back once, where it was — `docs/goal/ui/backups.md` § Audit-alert surface
    - `tests/e2e-unified/tests/test_backups.py::test_a_nest_that_went_back_in_time_gets_its_lost_mail_back_from_this_devices_copy`
23. [app] A destination that stops accepting your nest's backup says so on its row instead of showing a queue, and shows the queue again once it accepts — `docs/goal/behavior/backup-destinations.md` § Per-destination status read
    - `tests/e2e-unified/tests/test_backups.py::test_a_destination_that_refuses_this_box_reads_refused_not_queued`
24. [app] The restore section offers a box for each kind your nest restores (mail, calendar, contacts, posts and conversations), and a contacts snapshot restores from it — `docs/goal/ui/backups.md` § Restore from backup destination
    - `tests/e2e-unified/tests/test_backups_restore.py::test_a_contacts_snapshot_restores_from_the_restore_section`
25. [app] Before a copy is pulled back from a destination, the confirm lists each set as vouched, rolled back, unvouched or withheld by this device, and a set the confirm did not show is not pulled — `docs/goal/architecture/client-device-custodian.md` § Implementation status today
   - (none)
26. [nest] When your nest goes back to an older copy of itself, the recovery also brings back the lost events, contacts and posts, each once, and never overwrites an event changed since — `docs/goal/architecture/client-device-custodian.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_backup_regression_recovers_records_api.py::test_a_regressed_nest_recovers_its_lost_records_once_and_keeps_a_later_edit`
27. [nest] Restored mail lands back in the mailbox it was filed in, and mail that arrives during an interrupted restore is kept — `docs/goal/architecture/client-device-custodian.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_backup_restore_mailbox_and_torn_arrival_api.py::test_restored_mail_lands_in_its_mailbox_and_an_arrival_during_a_torn_restore_is_kept`
28. [app] After losing your nest, a restore from a device's copy also brings back the folders it covered, under their names — `docs/goal/architecture/client-device-custodian.md` § Implementation status today
   - `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one`
29. [app] A device holding your backup that loses part of its copy fetches it again on its next pull, and its row reads passing once a later check confirms the copy — `docs/goal/architecture/client-device-custodian.md` § Implementation status today
   - `tests/e2e-unified/tests/test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner`
30. [app] A destination already holding another nest of yours is refused by name when you add it, and a restore onto a fresh nest refuses it the same way and writes nothing there — `docs/goal/architecture/segment-backup-protocol.md` § Implementation status today
   - `tests/e2e-unified/tests/test_backups.py::test_a_destination_another_box_holds_is_refused_by_name_on_restore_and_on_add`
31. [app] A destination added or changed on another of your devices shows on an open Backups page without leaving it — `docs/goal/architecture/account-runtime.md` § Multi-instance concurrency
   - `tests/e2e-unified/tests/test_store_change_notice.py::test_an_open_backups_page_shows_a_destination_another_device_adds_and_renames`
32. [nest] A destination whose proven identity differs from what it claims, or from the one you enrolled, is refused at enrollment and on every later connection — `docs/goal/behavior/backup-destinations.md` § Create / edit / remove protocol
   - `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_enrolling_a_destination_whose_claim_disagrees_with_its_proof_is_refused`
   - `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_a_destination_url_answered_by_another_nest_is_refused_at_every_later_connection`
33. [nest] Re-seeding a replacement box refuses a target nest that is not fresh — `docs/goal/architecture/nest/box-recovery.md` § Goal
   - `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_reseeding_a_replacement_box_refuses_a_target_nest_that_is_not_fresh`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| linux | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| windows | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5-dev+390d61b5 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_crud` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_backups.py::test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it` | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_audit_pass_advances_the_last_checked_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_audit_alerts_when_the_destination_goes_dark` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_backup_audit_observation.py::test_rendering_the_conversation_list_feeds_the_audit_observation` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_backup_audit_folder_replica.py::test_a_delisted_covered_folder_row_is_caught_by_the_replica_anchor` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_backups.py::test_removing_a_custodian_with_the_opt_in_frees_the_local_store` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_folder_destination_places.py::test_folder_destination_place_attach_and_detach` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_history_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_banner_and_modal` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_mail_snapshot_create_restore_and_caldav_divergence` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_restore_message_kind_confirm_mismatch_rejected` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_backups.py::test_client_device_custodian_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_backups.py::test_backup_destination_last_upload_time_reflects_a_nest_side_pass` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_a_surviving_nest_destination_restores_the_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_local_restore_action_restores_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_reports_progress_and_says_when_it_is_done` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custody_minted_on_another_device_renders_and_revokes_here` | web (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): failed |
| 16 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_dr_restore.py::test_conv_snapshot_create_restore_and_history` | nest (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_restore_divergence_flags_a_reconnected_mail_app_as_it_does_a_calendar` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_a_restore_without_the_account_configuration_says_so_and_a_complete_one_does_not` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_backup_rotated_source_journey.py::test_a_rotated_source_box_keeps_backing_up_with_no_gesture` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_backups.py::test_a_nest_that_went_back_in_time_gets_its_lost_mail_back_from_the_backup` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_backups.py::test_a_nest_that_went_back_in_time_gets_its_lost_mail_back_from_this_devices_copy` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 23 | app | `tests/e2e-unified/tests/test_backups.py::test_a_destination_that_refuses_this_box_reads_refused_not_queued` — requires `backup-destination-trust-rows` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 24 | app | `tests/e2e-unified/tests/test_backups_restore.py::test_a_contacts_snapshot_restores_from_the_restore_section` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 25 | app | (none) | — |
| 26 | nest | `tests/e2e-unified/tests/api/test_backup_regression_recovers_records_api.py::test_a_regressed_nest_recovers_its_lost_records_once_and_keeps_a_later_edit` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_backup_restore_mailbox_and_torn_arrival_api.py::test_restored_mail_lands_in_its_mailbox_and_an_arrival_during_a_torn_restore_is_kept` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_backups.py::test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 29 | app | `tests/e2e-unified/tests/test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 30 | app | `tests/e2e-unified/tests/test_backups.py::test_a_destination_another_box_holds_is_refused_by_name_on_restore_and_on_add` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 31 | app | `tests/e2e-unified/tests/test_store_change_notice.py::test_an_open_backups_page_shows_a_destination_another_device_adds_and_renames` | tui (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_enrolling_a_destination_whose_claim_disagrees_with_its_proof_is_refused` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_a_destination_url_answered_by_another_nest_is_refused_at_every_later_connection` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py::test_reseeding_a_replacement_box_refuses_a_target_nest_that_is_not_fresh` | nest (linux): passed |
<!-- features-render:end -->
