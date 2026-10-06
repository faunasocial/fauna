---
slug: files-on-demand
title: Files on demand
section: your data and devices
goal: docs/goal/behavior/on-demand-files.md § On-Demand Files (Placeholders)
guide: docs/guides/cloud-sync.md § On-demand files ("free up space")
absences:
  web: "docs/goal/behavior/on-demand-files.md § On-Demand Files (Placeholders)"
---

## What a user gets

Where the operating system has cloud files, a synced folder can show every file
as a placeholder that downloads when you open it, so a big folder takes no space
until you need it. It is still two-way: a file you change uploads all the same.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

1. [app] Whether a folder is kept in full on this device or fetched on demand is your call per folder, and the app remembers it — `docs/goal/behavior/on-demand-files.md` § On-Demand Files (Placeholders)
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_defaults_on_and_a_flip_survives_re_entry`
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_yields_to_a_bound_folder`
   - `tests/e2e-unified/tests/test_folder_location_mode_toggle.py::test_folder_location_mode_toggle_flip_survives_re_entry`
2. [nest] Placeholders list, then download their bytes on open — `docs/goal/behavior/on-demand-files.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path`
3. [app] A file you change inside an on-demand folder uploads like any other, and a file you put there joins the folder — `docs/goal/behavior/on-demand-files.md` § Sync direction
   - `tests/e2e-unified/tests/test_folder_on_demand_read_path_linux.py::test_on_demand_folder_is_read_and_written_through_its_mount`
4. [app] Freeing a file's space keeps the file listed where it was and never deletes it from your nest or your other devices — `docs/goal/behavior/on-demand-files.md` § A placeholder is
   - (none)
5. [app] A change that has not reached your nest yet cannot have its space freed: your copy stays until it is safely recorded — `docs/goal/behavior/on-demand-files.md` § Apple File Provider binding
   - (none)
6. [app] You can keep several folders on demand at once, and switching one on or off leaves the others working — `docs/goal/behavior/on-demand-files.md` § On-Demand Files (Placeholders)
   - (none)
7. [nest] A file added from another device shows up in your on-demand folder by itself — `docs/goal/behavior/on-demand-files.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path`
8. [app] Turning on-demand off for a folder never destroys a change that had not uploaded yet — `docs/goal/behavior/on-demand-files.md` § Apple File Provider binding
   - (none)
9. [app] Showing a folder on demand on a device the folder did not include adds that device to the folder's devices once, doing everything a device does by default — `docs/goal/behavior/on-demand-files.md` § Apple File Provider binding
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once`

10. [app] A folder someone shared with you is shown on demand as well, and its switch only hides or shows it on this device: it never changes the folder's devices — `docs/goal/behavior/on-demand-files.md` § Shared sets on a capability host
   - `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_a_member_row_is_hide_only`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux |  no run recorded | |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_defaults_on_and_a_flip_survives_re_entry` | macos (macos): passed, ios (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_yields_to_a_bound_folder` | macos (macos): passed, ios (macos): skipped |
| 1 | app | `tests/e2e-unified/tests/test_folder_location_mode_toggle.py::test_folder_location_mode_toggle_flip_survives_re_entry` | linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path` | — |
| 3 | app | `tests/e2e-unified/tests/test_folder_on_demand_read_path_linux.py::test_on_demand_folder_is_read_and_written_through_its_mount` | linux (linux): passed |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | nest | `tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py::test_file_provider_read_path` | — |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once` | macos (macos): passed, ios (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_a_member_row_is_hide_only` | — |
<!-- features-render:end -->
