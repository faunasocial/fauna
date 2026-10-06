---
slug: follow-a-public-folder
title: Follow someone's public folder
section: your data and devices
goal: docs/goal/ui/folders.md § Following a public folder
guide: docs/guides/who-can-see-what.md § Folders you make public
---

## What a user gets

Follow a folder someone made public by its address and browse it in Media as
a read-only scope, even when it lives on another nest. If they take it private
again, your copy stops updating.

## Coverage contract

Stamped 2026-09-22 at 963ffb408a.

1. [app] Following a public folder lists it, and Media browses it read-only — `docs/goal/ui/folders.md` § Following a public folder
   - `tests/e2e-unified/tests/test_folder_follow_media_browse.py::test_follow_a_public_folder_and_browse_it_in_media`
2. [nest] A stranger reads a public folder, a private one refuses them, and a folder on another nest is fetched through the relay — `docs/goal/behavior/folders.md` § Publicly-synced follow
   - `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold`
   - `tests/e2e-unified/tests/api/test_public_folder_follow_cross_nest.py::test_a_follower_on_another_nest_reads_the_relay_and_fetches_the_bytes`
3. [app] You can stop following a folder, and it leaves your list with nothing to undo anywhere else — `docs/goal/ui/folders.md` § Following a public folder
   - `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_unfollowing_removes_the_row_with_nothing_to_undo_anywhere`
4. [app] When the owner stops publishing a folder you follow, its row says plainly that it is no longer available and stays until you remove it, and it resumes by itself if they publish again — `docs/goal/ui/folders.md` § Following a public folder
   - `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_follow_says_when_its_owner_stops_publishing_and_resumes_by_itself`
5. [nest] Nothing that was in a folder before it was made public is ever served to the people following it — `docs/goal/behavior/folders.md` § Publicly-synced follow
   - `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold`
6. [nest] Someone following a folder never learns which device wrote a file or who authored it — `docs/goal/behavior/folders.md` § Publicly-synced follow
   - `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold`
7. [nest] The owner's nest keeps no record of who follows a folder, so followers cannot be listed and following costs the owner nothing — `docs/goal/behavior/folders.md` § Publicly-synced follow
   - `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_the_owners_nest_keeps_no_record_of_who_follows`
8. [app] A follow address that is wrong, private or gone fails with one plain message, and a network problem never reads as the folder having been taken away — `docs/goal/ui/folders.md` § Following a public folder
   - `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_wrong_private_or_gone_address_fails_with_one_plain_message`
   - `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_network_problem_never_reads_as_the_folder_having_been_taken_away`
9. [app] A followed folder's row says whose folder it is — `docs/goal/ui/folders.md` § Following a public folder
   - `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_followed_row_says_whose_folder_it_is`
10. [app] A file in a folder you follow can be downloaded from the browse, with no account on the owner's nest — `docs/goal/ui/media.md` § Followed public folders
   - `tests/e2e-unified/tests/test_tui_media_external_open.py::test_a_followed_folders_file_on_another_nest_opens_with_no_account_there`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3743cd5.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+e5506940 standalone |
| ios | ⚠ partial | 0.1.2-dev+e5506940 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+e3bf6a64.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folder_follow_media_browse.py::test_follow_a_public_folder_and_browse_it_in_media` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_public_folder_follow_cross_nest.py::test_a_follower_on_another_nest_reads_the_relay_and_fetches_the_bytes` | nest (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_unfollowing_removes_the_row_with_nothing_to_undo_anywhere` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_follow_says_when_its_owner_stops_publishing_and_resumes_by_itself` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_a_stranger_reads_a_public_folder_and_the_floor_strip_and_revoke_hold` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_public_folder_fetch.py::test_the_owners_nest_keeps_no_record_of_who_follows` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_wrong_private_or_gone_address_fails_with_one_plain_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_network_problem_never_reads_as_the_folder_having_been_taken_away` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_folder_follow_outcomes.py::test_a_followed_row_says_whose_folder_it_is` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_tui_media_external_open.py::test_a_followed_folders_file_on_another_nest_opens_with_no_account_there` | tui (linux): passed |
<!-- features-render:end -->
