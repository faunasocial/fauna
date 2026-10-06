---
slug: photo-backup
title: Back up your photos
section: your data and devices
goal: docs/goal/ui/folders.md § Photo backup (apple + android)
guide: docs/guides/cloud-photos.md § Automatic photo backup from your phone
absences:
  web: "docs/goal/ui/folders.md § Photo backup (apple + android)"
  linux: "docs/goal/ui/folders.md § Photo backup (apple + android)"
  windows: "docs/goal/ui/folders.md § Photo backup (apple + android)"
  tui: "docs/goal/architecture/apps/tui.md § Declared platform absences"
---

## What a user gets

On a phone, your photo library is a backup folder: new photos flow to your nest
by themselves and are there to browse from any device, without the phone ever giving
up its own copies.

## Coverage contract

Stamped 2026-09-19 at f62a5e4e1c.

1. [app] A photo taken on the phone reaches the nest and shows in Media — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
2. [app] Turning photo backup on gives you an ordinary "Photo Library" folder, made for you, that you manage like any folder you made yourself — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
3. [app] Once backup is on, new photos go up by themselves — you never have to open the app or press anything — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_unattended.py::test_a_new_photo_goes_up_with_nothing_pressed`
   - `tests/e2e-unified/tests/test_photo_backup_unattended.py::test_the_scheduled_background_pass_runs_the_real_ingest`
   - `tests/e2e-unified/tests/test_photo_backup_pass_coalescing.py::test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_new_photo_goes_up_with_nothing_pressed`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass`
4. [app] The phone says when the backup last finished — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media`
5. [app] While a backup pass runs, the phone shows how far it has got — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_pass_progress.py::test_a_running_pass_shows_how_far_it_has_got`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_running_pass_shows_how_far_it_has_got`
6. [app] You can make the backup run now instead of waiting for the next pass — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_pass_progress.py::test_sync_now_starts_a_pass_that_nothing_else_would_have_started`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_sync_now_starts_a_pass_that_nothing_else_would_have_started`
7. [app] Backing up never takes anything away from the phone: nothing that happens to a photo on the nest reaches back into the camera roll — `docs/goal/ui/folders.md` § Photo backup (apple + android)
   - `tests/e2e-unified/tests/test_photo_backup_one_way.py::test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone`
8. [app] The copy your nest keeps is the picture the phone took, not a re-encoded one — `docs/goal/behavior/sync-engine-deployments.md` § Apple apps — convergence design (ratified 2026-07-12)
   - `tests/e2e-unified/tests/test_photo_backup_byte_fidelity.py::test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone`
   - `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | — absent | |
| windows | — absent | |
| macos |  no run recorded | |
| ios | ✅ full | 0.1.2-dev+fe03cd4e standalone |
| android |  no run recorded | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | ios (macos): passed |
| 1 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | — |
| 2 | app | `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | — |
| 3 | app | `tests/e2e-unified/tests/test_photo_backup_unattended.py::test_a_new_photo_goes_up_with_nothing_pressed` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_photo_backup_unattended.py::test_the_scheduled_background_pass_runs_the_real_ingest` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_photo_backup_pass_coalescing.py::test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass` | ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_new_photo_goes_up_with_nothing_pressed` | — |
| 3 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass` | — |
| 4 | app | `tests/e2e-unified/tests/test_photo_backup_library_ingest.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media` | — |
| 5 | app | `tests/e2e-unified/tests/test_photo_backup_pass_progress.py::test_a_running_pass_shows_how_far_it_has_got` | ios (macos): passed |
| 5 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_a_running_pass_shows_how_far_it_has_got` | — |
| 6 | app | `tests/e2e-unified/tests/test_photo_backup_pass_progress.py::test_sync_now_starts_a_pass_that_nothing_else_would_have_started` | ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_sync_now_starts_a_pass_that_nothing_else_would_have_started` | — |
| 7 | app | `tests/e2e-unified/tests/test_photo_backup_one_way.py::test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone` | ios (macos): passed |
| 7 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone` | — |
| 8 | app | `tests/e2e-unified/tests/test_photo_backup_byte_fidelity.py::test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone` | ios (macos): passed |
| 8 | app | `tests/e2e-unified/tests/real_session/test_photo_backup_macos.py::test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone` | — |
<!-- features-render:end -->
