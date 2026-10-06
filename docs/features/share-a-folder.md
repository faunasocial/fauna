---
slug: share-a-folder
title: Share a folder with people
section: your data and devices
goal: docs/goal/ui/folders.md § Sharing a folder (cross-user)
guide: docs/guides/cloud-sharing-family.md § Sharing a folder with someone
---

## What a user gets

Share a folder with someone, on your nest or another, as a reader or a writer;
they accept or decline, and a writer can add files. What they receive is sealed
with a key only members hold, and a member's own app opens it. A folder can also be
served to standard file apps over WebDAV.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

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
13. [nest] What a member stores counts against the owner's storage, and a member who would pass the limit the owner set for them is refused — `docs/goal/behavior/file-sync.md` § Multi-writer shared sets
    - `tests/e2e-unified/tests/api/test_shared_folder_member_cap.py::test_a_members_bytes_charge_the_owner_and_their_cap_refuses_them`
14. [app] Declining a shared folder takes you off the owner's list of people it is shared with — `docs/goal/ui/folders.md` § Sharing a folder (cross-user)
    - `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline`
15. [app] Removing someone is finished even if your app stops partway through: the next time it starts, the person loses access with nothing more for you to do — `docs/goal/architecture/mls-group-key-material.md` § Audience: an MLS group at a specific epoch (a defined membership set that can change)
    - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_owner_between_a_member_removals_stage_and_publish_relaunch_finishes_it`

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
| tui | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_owner_side_sharing_affordances` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_full_share_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_decrypts_shared_set_content_through_their_own_media_page` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_member_re_ingests_custody_across_owner_rotation_and_decrypts_gen2` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_writer_member_decrypts_owner_upload` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_macos_writer_member_decrypts_owner_upload` | — |
| 3 | app | `tests/e2e-unified/tests/test_folder_agent_content_sync.py::test_windows_writer_member_decrypts_owner_upload` | windows (windows): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_cross_nest_foreign_row.py::test_cross_nest_shared_folder_renders_as_a_foreign_row` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_serves_and_unserves_a_sync_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 5 | app | `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_folder_webdav_toggle_is_disabled_until_mail_is_set_up` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_folders.py::test_a_known_persons_share_appears_and_only_a_strangers_waits` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_member_role_and_cap_editing` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_writer_warning_follows_whether_the_folder_is_published` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_removed_member_cannot_open_what_is_added_afterwards` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_member_media_decrypt.py::test_a_second_share_keeps_the_first_members_access_and_everything_readable` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_folder_writer_revocation.py::test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_shared_folder_member_cap.py::test_a_members_bytes_charge_the_owner_and_their_cap_refuses_them` | nest (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_folders.py::test_folder_pending_share_accept_decline` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_owner_between_a_member_removals_stage_and_publish_relaunch_finishes_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
