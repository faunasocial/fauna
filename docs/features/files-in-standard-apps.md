---
slug: files-in-standard-apps
title: Your folders as a network drive
section: mail, calendar and contacts
goal: docs/goal/behavior/webdav-server.md § Goal
guide: docs/guides/cloud-sync.md § Mount it like a network drive (WebDAV)
absences:
  web (outcome 4): "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
  ios (outcome 4): "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
  android (outcome 4): "docs/goal/architecture/apps/sync-agent.md § Scope per platform"
---

## What a user gets

Mount a served folder in your file manager over WebDAV with your app password:
list, read and write files, with the server sealing and unsealing them for you. A
folder you have not served stays unreachable, and the mail-app settings page shows
the address to mount.

## Coverage contract

Stamped 2026-09-23 at 6605debcb9.

1. [app] A file manager mounts, signs in, lists a served folder, and cannot reach an unserved one — `docs/goal/behavior/webdav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/test_webdav_mount_and_gate.py::test_webdav_mount_auth_and_served_set_gate`
2. [app] A file written through the mount reads back through it, and a folder without content keys refuses writes — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py::test_webdav_served_set_read_write_roundtrip`
3. [app] The mail-app settings page shows the WebDAV address once serving is on — `docs/goal/behavior/webdav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_mua_webdav_url_row_tracks_the_per_actor_serve_state`
4. [app] A file the app synced into a served folder reads back byte-identical through the mount, and a file written through the mount arrives byte-identical in the app's synced folder — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_engine_and_webdav_mda_are_one_chunk_writer`
5. [app] Deleting, renaming, moving or copying a file through the mount does the same to it in your folder — `docs/goal/behavior/webdav-server.md` § Protocol surface (v1) and deliberate deferrals
   - `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_mount_delete_rename_move_and_copy_reach_the_folder`
   - `tests/e2e-unified/tests/test_webdav_folder_view.py::test_mount_edits_show_in_the_apps_folder_view`
6. [app] If a file changed since your file app last looked, its save is refused instead of overwriting the newer copy — `docs/goal/behavior/webdav-server.md` § Protocol surface (v1) and deliberate deferrals
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive`
7. [app] Once you stop serving a folder, your file app no longer lists or opens it — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive`
8. [app] Files written through the mount stay readable in your apps after you stop serving the folder — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_mount_written_files_stay_readable_after_unserving`
   - `tests/e2e-unified/tests/test_webdav_folder_view.py::test_a_mount_written_file_still_opens_in_the_app_after_unserving`
9. [app] Serving a folder that already holds files makes them reachable through the mount, and nothing is lost on the way — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_serving_a_folder_that_already_holds_files_reaches_them`
   - `tests/e2e-unified/tests/test_webdav_folder_view.py::test_serving_a_folder_the_app_already_filled_reaches_its_files`
10. [app] A folder you share with other people can be served to your file apps as well, and both see the same files — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/test_webdav_shared_set.py::test_a_shared_and_served_folder_shows_owner_mount_and_member_the_same_files`
11. [app] Nothing in your served folders can be listed or read without signing in — `docs/goal/behavior/webdav-server.md` § Network exposure & discovery
   - `tests/e2e-unified/tests/test_webdav_mount_and_gate.py::test_webdav_mount_auth_and_served_set_gate`
12. [app] Your nest holds your served files only in sealed form — `docs/goal/behavior/webdav-server.md` § Threat model — inherited, with the per-set-scoped widening
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive`
13. [app] A file over your storage allowance is refused through the mount, and your file app can see how much space you have used and have left — `docs/goal/behavior/webdav-server.md` § Protocol surface (v1) and deliberate deferrals
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_the_mount_reports_space_and_refuses_a_file_over_the_allowance`
14. [app] Changes made through the mount show in your folder's activity as coming from the network drive — `docs/goal/behavior/webdav-server.md` § Protocol surface (v1) and deliberate deferrals
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive`
15. [nest] A file app pointed at your domain's standard WebDAV address is sent on to your files — `docs/goal/behavior/webdav-server.md` § Network exposure & discovery
   - `tests/e2e-unified/tests/api/test_webdav_apex_discovery.py::test_the_standard_webdav_address_sends_a_file_app_on_to_the_files`
16. [app] A file up to 512 MiB saves through the mount, and a bigger one is refused — `docs/goal/behavior/webdav-server.md` § Bulk-byte plane — the HTTP carve-out, not WS-RPC
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_the_mount_saves_a_512_mib_file_and_refuses_a_bigger_one`
17. [app] Your file app shows each file's size and when it last changed — `docs/goal/behavior/webdav-server.md` § Protocol surface (v1) and deliberate deferrals
   - `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+2cb6e915.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+f29a951a standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_webdav_mount_and_gate.py::test_webdav_mount_auth_and_served_set_gate` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py::test_webdav_served_set_read_write_roundtrip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_webdav_toggle.py::test_mua_webdav_url_row_tracks_the_per_actor_serve_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 4 | app | `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_engine_and_webdav_mda_are_one_chunk_writer` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | absent by design on web, ios, android | — |
| 5 | app | `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_mount_delete_rename_move_and_copy_reach_the_folder` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_webdav_folder_view.py::test_mount_edits_show_in_the_apps_folder_view` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_mount_written_files_stay_readable_after_unserving` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_webdav_folder_view.py::test_a_mount_written_file_still_opens_in_the_app_after_unserving` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_webdav_engine_cross_writer.py::test_serving_a_folder_that_already_holds_files_reaches_them` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_webdav_folder_view.py::test_serving_a_folder_the_app_already_filled_reaches_its_files` | — |
| 10 | app | `tests/e2e-unified/tests/test_webdav_shared_set.py::test_a_shared_and_served_folder_shows_owner_mount_and_member_the_same_files` | macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_webdav_mount_and_gate.py::test_webdav_mount_auth_and_served_set_gate` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_the_mount_reports_space_and_refuses_a_file_over_the_allowance` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_webdav_apex_discovery.py::test_the_standard_webdav_address_sends_a_file_app_on_to_the_files` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_the_mount_saves_a_512_mib_file_and_refuses_a_bigger_one` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_webdav_mount_behaviours.py::test_a_mounted_folder_behaves_like_a_network_drive` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
