---
slug: share-a-folder
title: Share a folder with people
section: your data and devices
goal: docs/goal/ui/folders.md § Sharing a folder (cross-user)
guide: docs/guides/cloud-sharing-family.md § Sharing a folder with someone
absences:
  web (outcome 20): "docs/goal/ui/folders.md § Sharing a folder (cross-user)"
  ios (outcome 20): "docs/goal/ui/folders.md § Sharing a folder (cross-user)"
  android (outcome 20): "docs/goal/ui/folders.md § Sharing a folder (cross-user)"
  web (outcome 12): "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
  web (outcomes 17, 18, 19, 27, 28, 29): "docs/goal/behavior/p2p.md § Implementation status today"
  android-kids (outcome 9): "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids (outcome 9): "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Share a folder with someone, on your nest or another, as a reader or a writer;
they accept or decline, and a writer can add files. What they receive is
encrypted with a key only members hold, and a member's own app opens it. Making
the folder public changes that, as [Make a folder public, or a
website](public-folders-and-websites.md) explains; if you serve it over WebDAV
or put it behind a paywall, the box that serves it can read it too. A folder can
also be served to standard file apps over WebDAV; its files stay stored
encrypted on your nest, which opens them with the password of a mail, calendar,
contacts or file app you connect, for that app's signed-in session.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] The owner shares with a person, sees the roster, changes their role, and can un-share — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_folder_owner_side_sharing_affordances`
   - `tests/e2e-unified/tests/test_folders.py::test_folder_full_share_round_trip`
   - `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing`
2. [app] The person invited sees the pending share and accepts or declines it — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline`
3. [app] A member opens the owner's files, and keeps opening them after the owner rotates the key — `docs/goal/behavior/file-sync.md` § Multi-writer shared sets
   - `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_decrypts_shared_set_content_through_their_own_media_page`
   - `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_re_ingests_custody_across_owner_rotation_and_decrypts_gen2`
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_writer_member_decrypts_owner_upload`
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_macos_writer_member_decrypts_owner_upload`
   - `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_windows_writer_member_decrypts_owner_upload`
4. [app] A folder shared from another nest shows in your folders list once accepted — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folder_cross_nest_foreign_row.py::test_cross_nest_shared_folder_renders_as_a_foreign_row`
5. [app] A folder can be served to standard file apps, and the switch waits until mail is set up — `docs/goal/behavior/webdav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_serves_and_unserves_a_sync_set`
   - `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_is_disabled_until_mail_is_set_up`
6. [app] You can leave a folder someone shared with you, and it stops appearing in your list — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline`
7. [app] A folder shared by someone you already know simply appears in your list; only a stranger's share waits for your answer — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_a_known_persons_share_appears_and_only_a_strangers_waits`
8. [app] The owner can limit how much a person may store in a shared folder, and is warned when giving someone write access with no limit — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing`
9. [app] Giving someone write access to a folder that people outside it can already read tells the owner that this person can change what those people see — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
   - `tests/e2e-unified/tests/test_folders.py::test_folder_writer_warning_follows_whether_the_folder_is_published`
10. [app] Someone you remove from a folder cannot open anything added to it afterwards — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_removed_member_cannot_open_what_is_added_afterwards`
11. [app] Sharing a folder with a second person leaves the first person's access as it was, and everything already in the folder stays readable for all of them — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_second_share_keeps_the_first_members_access_and_everything_readable`
12. [app] When the owner takes your write access away, your app stops syncing that folder and says so, and leaves your own files and unsent changes alone — `docs/goal/behavior/file-sync.md` § Multi-writer shared sets
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files`
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_on_the_documents_provider_is_told_and_keeps_the_unsent_edit`
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_on_the_ios_file_provider_domain_is_told_and_keeps_the_unsent_file`
13. [nest] What a member stores counts against the owner's storage, and a member who would pass the limit the owner set for them is refused — `docs/goal/behavior/file-sync.md` § Multi-writer shared sets
    - `tests/e2e-unified/tests/api/test_shared_folder_member_cap.py::test_a_members_bytes_charge_the_owner_and_their_cap_refuses_them`
14. [app] Declining a shared folder takes you off the owner's list of people it is shared with — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline`
15. [app] Removing someone is finished even if your app stops partway through: the next time it starts, the person loses access with nothing more for you to do — `docs/goal/architecture/mls-group-key-material.md` § Audience: an MLS group at a specific epoch (a defined membership set that can change)
    - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_owner_between_a_member_removals_stage_and_publish_relaunch_finishes_it`
16. [app] A folder shared with you shows in your Media folder filter with its owner's name, apart from a folder of your own with the same name; you can upload into it when you may write, and are told why not when you may only read — `docs/goal/ui/media.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_a_same_named_shared_set_is_its_own_option_and_upload_target`
    - `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_a_namesake_made_before_the_accept_does_not_hide_the_shared_set`
    - `tests/e2e-unified/tests/test_media_cross_nest_writer_upload.py::test_a_writer_member_on_another_nest_uploads_and_deletes_and_a_reader_is_refused`
17. [app] A file added to a shared folder while the nest cannot be reached reaches the other people in it straight from the device it was added on — `docs/goal/behavior/p2p-shared-set.md` § Cross-user shared-set transfer
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_member_pulls_offline_authored_file_from_owner_peer`
18. [app] A person the folder was never shared with gets nothing readable from your device, even on the same network — `docs/goal/behavior/p2p-shared-set.md` § Cross-user shared-set transfer
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable`
19. [app] A transfer between two members' devices, interrupted part-way, picks up where it stopped, without sending again what already arrived — `docs/goal/behavior/p2p-shared-set.md` § Cross-user shared-set transfer
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`
20. [app] After you remove someone from a folder, the folder's sync status reads that keys are pending until your own sync agent has picked up the new key, and clears once it has — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_after_a_removal_the_sync_status_reads_keys_pending_until_the_agent_catches_up`
21. [app] The owner shares a folder with a person on another nest by typing their handle with that nest's domain, and the invitation reaches them on their own nest — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folder_cross_nest_share_picker.py::test_owner_shares_a_folder_with_a_person_on_another_nest`
22. [app] After you take someone's write access away or remove them, the files they wrote earlier stay in the folder for you and the other members — `docs/goal/architecture/writer-signed-change-records.md` § Implementation status today
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files`
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_former_writers_files_stay_listed_and_open_for_a_web_owner`
    - `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_former_writers_files_stay_listed_and_open_for_an_ios_owner`
23. [app] After the owner takes their account back, a member still opens the folder's files from before, and a file the stolen key writes afterwards is refused — `docs/goal/architecture/succession-cut.md` § Implementation status today
   - `tests/e2e-unified/tests/test_folder_succession_member_reader.py::test_after_the_owner_takes_the_account_back_a_member_keeps_the_old_files_and_refuses_the_stolen_key`
24. [app] A folder shared with you from another nest names who shared it by their full handle, on the pending share and on the folder — `docs/goal/architecture/federation-shared-folders.md` § Implementation status today
   - `tests/e2e-unified/tests/test_folder_cross_nest_foreign_row.py::test_a_cross_nest_share_names_the_sharer_by_full_handle`
25. [app] Someone on another nest who was given a folder whose nest keeps no file bytes opens its files, served from a member's device through the owner's nest, and loses them when removed — `docs/goal/architecture/federation-shared-folders.md` § Implementation status today
   - `tests/e2e-unified/tests/test_folder_cross_nest_member_reads.py::test_a_member_on_another_nest_opens_a_metadata_only_file_and_loses_it_when_removed`
26. [app] When a folder shared with you has the same name as one of your own, the Folders list shows both, and deleting, listing the versions of or restoring one of its files acts on the folder you chose, never on your same-named one — `docs/goal/behavior/set-addressing.md` § The row form
   - `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_delete_versions_and_restore_act_on_the_chosen_row_never_the_namesake`
27. [app] The folder's sync status shows what is moving between members' devices, each transfer with its progress, and a pull that failed keeps its row saying whether their device could not be reached or the connection was lost and is being retried — `docs/goal/behavior/p2p-shared-set.md` § The transfer row of a pull that failed
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_pull_from_a_device_that_went_away_keeps_its_row_and_says_why`
28. [app] A file over the direct-transfer size cap arrives through the nest instead, and its transfer row says it was limited — `docs/goal/behavior/p2p-shared-set.md` § Wormability walk
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_file_over_the_per_file_cap_arrives_through_the_nest_and_its_row_says_limited`
29. [app] A member who left or was removed drops off the transfer list, and their devices are no longer contacted — `docs/goal/behavior/p2p-shared-set.md` § A departed member's cached endpoint
    - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_removed_member_drops_off_the_owners_transfer_list`
30. [app] Someone on another nest who was given a folder sees every earlier version of its files in the file's history, not just the latest — `docs/goal/ui/media.md` § Cross-nest shared sets
    - `tests/e2e-unified/tests/test_tui_media_foreign_versions.py::test_a_member_on_another_nest_reads_a_shared_files_whole_history`
31. [app] A member whose copy of the folder could not follow the owner's new identity is told so and drops off the owner's list, and the owner sharing the folder again catches them up — `docs/goal/architecture/federation-shared-folders.md` § Cross-nest shared folders + channel append
   - `tests/e2e-unified/tests/test_folder_succession_member_reader.py::test_a_member_whose_copy_could_not_follow_the_owners_new_identity_is_re_shared_and_catches_up`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| linux | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| windows | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| macos | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| ios | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5-dev+8755c14e standalone |
| android-kids |  no run recorded | |
| ios-kids | 🚧 unbuilt | 0.1.5-dev+8755c14e standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_owner_side_sharing_affordances` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_full_share_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_decrypts_shared_set_content_through_their_own_media_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_re_ingests_custody_across_owner_rotation_and_decrypts_gen2` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_writer_member_decrypts_owner_upload` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_macos_writer_member_decrypts_owner_upload` | macos (macos): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_windows_writer_member_decrypts_owner_upload` | windows (windows): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_cross_nest_foreign_row.py::test_cross_nest_shared_folder_renders_as_a_foreign_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_serves_and_unserves_a_sync_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_is_disabled_until_mail_is_set_up` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_folders.py::test_a_known_persons_share_appears_and_only_a_strangers_waits` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_writer_warning_follows_whether_the_folder_is_published` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | absent by design on android-kids, ios-kids | — |
| 10 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_removed_member_cannot_open_what_is_added_afterwards` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_second_share_keeps_the_first_members_access_and_everything_readable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_on_the_documents_provider_is_told_and_keeps_the_unsent_edit` | — |
| 12 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_on_the_ios_file_provider_domain_is_told_and_keeps_the_unsent_file` | ios (macos): passed, tui (macos): passed |
| 12 | app | absent by design on web | — |
| 13 | nest | `tests/e2e-unified/tests/api/test_shared_folder_member_cap.py::test_a_members_bytes_charge_the_owner_and_their_cap_refuses_them` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_owner_between_a_member_removals_stage_and_publish_relaunch_finishes_it` — requires `unclean-kill` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_a_same_named_shared_set_is_its_own_option_and_upload_target` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_a_namesake_made_before_the_accept_does_not_hide_the_shared_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): failed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_media_cross_nest_writer_upload.py::test_a_writer_member_on_another_nest_uploads_and_deletes_and_a_reader_is_refused` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_member_pulls_offline_authored_file_from_owner_peer` — requires `share-offline-landing` (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 17 | app | absent by design on web | — |
| 18 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 18 | app | absent by design on web | — |
| 19 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived` — requires `share-offline-landing` (not built: web, ios, android, android-kids, ios-kids); `share-serve-tally` (test harness) (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 19 | app | absent by design on web | — |
| 20 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_after_a_removal_the_sync_status_reads_keys_pending_until_the_agent_catches_up` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 20 | app | absent by design on web, ios, android, android-kids, ios-kids | — |
| 21 | app | `tests/e2e-unified/tests/test_folder_cross_nest_share_picker.py::test_owner_shares_a_folder_with_a_person_on_another_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 22 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 22 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_former_writers_files_stay_listed_and_open_for_a_web_owner` | web (linux): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_former_writers_files_stay_listed_and_open_for_an_ios_owner` | ios (macos): passed, tui (macos): passed |
| 23 | app | `tests/e2e-unified/tests/test_folder_succession_member_reader.py::test_after_the_owner_takes_the_account_back_a_member_keeps_the_old_files_and_refuses_the_stolen_key` — requires `succession-witness-member-seat` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
| 24 | app | `tests/e2e-unified/tests/test_folder_cross_nest_foreign_row.py::test_a_cross_nest_share_names_the_sharer_by_full_handle` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 25 | app | `tests/e2e-unified/tests/test_folder_cross_nest_member_reads.py::test_a_member_on_another_nest_opens_a_metadata_only_file_and_loses_it_when_removed` | tui (linux): passed, tui (macos): passed |
| 26 | app | `tests/e2e-unified/tests/test_media_shared_set_identity.py::test_delete_versions_and_restore_act_on_the_chosen_row_never_the_namesake` | tui (linux): passed, tui (macos): passed |
| 27 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_pull_from_a_device_that_went_away_keeps_its_row_and_says_why` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 27 | app | absent by design on web | — |
| 28 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_file_over_the_per_file_cap_arrives_through_the_nest_and_its_row_says_limited` — requires `share-cap-nest-hold` (test harness) (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, macos (macos): passed, ios (macos): skipped, tui (linux): passed, tui (macos): passed |
| 28 | app | absent by design on web | — |
| 29 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_removed_member_drops_off_the_owners_transfer_list` | linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): failed |
| 29 | app | absent by design on web | — |
| 30 | app | `tests/e2e-unified/tests/test_tui_media_foreign_versions.py::test_a_member_on_another_nest_reads_a_shared_files_whole_history` | tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_folder_succession_member_reader.py::test_a_member_whose_copy_could_not_follow_the_owners_new_identity_is_re_shared_and_catches_up` — requires `succession-witness-member-seat` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
<!-- features-render:end -->
