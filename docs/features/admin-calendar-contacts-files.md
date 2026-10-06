---
slug: admin-calendar-contacts-files
title: Calendar, Contacts and Files switches
section: admin area
goal: docs/goal/behavior/admin.md § 8. Calendar
guide: docs/guides/admin-tour.md § Calendar, Contacts, and Files
---

## What a user gets

Three switches turn calendar, contacts and file serving on or off for the whole
nest, independent of mail, plus the port calendars serve on. Changing the port moves
the live listener; a calendar app keeps working on the new one.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] Each of the three pages carries its switch, and flipping it changes the setting the nest holds, with no error — `docs/goal/behavior/admin.md` § 8. Calendar
   - `tests/e2e-unified/tests/test_admin_calendar.py::test_calendar_page_renders`
   - `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_enabled_toggle_round_trips`
   - `tests/e2e-unified/tests/test_admin_contacts.py::test_contacts_page_renders`
   - `tests/e2e-unified/tests/test_admin_contacts.py::test_carddav_enabled_toggle_round_trips`
   - `tests/e2e-unified/tests/test_admin_files.py::test_files_page_renders`
   - `tests/e2e-unified/tests/test_admin_files.py::test_webdav_enabled_toggle_round_trips`
2. [app] A calendar port saved on the page becomes the port the nest holds; a port that is not valid is refused on the page and the saved port stays — `docs/goal/behavior/caldav-server.md` § Network exposure
   - `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_port_round_trips`
   - `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_port_invalid_surfaces_error`
3. [nest] Until set, each of the three switches follows mail; once calendar is switched on explicitly it stays on when mail is turned off, while the other two still follow mail — `docs/goal/behavior/caldav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/api/test_dav_enable_independence.py::test_unset_dav_toggles_follow_mail_and_only_an_explicit_set_witnesses_intent`
4. [nest] When the admin switches calendar off, calendar apps can no longer connect and the standard calendar address answers that the service is unavailable; switching it back on lets them in again — `docs/goal/behavior/admin.md` § 8. Calendar
   - (none)
5. [nest] With files switched off, the standard files address answers that the service is unavailable — `docs/goal/behavior/webdav-server.md` § Network exposure & discovery
   - `tests/e2e-unified/tests/api/test_webdav_apex_discovery.py::test_the_standard_webdav_address_sends_a_file_app_on_to_the_files`
6. [nest] When the admin switches files off, file apps can no longer reach members' files — `docs/goal/behavior/webdav-server.md` § Independent enablement
   - (none)
7. [nest] Switching calendar off leaves mail working: each service has its own switch — `docs/goal/behavior/caldav-server.md` § Independent enablement
   - (none)
8. [app] The calendar-port field says it applies only to a nest with no domain — `docs/goal/behavior/admin.md` § 8. Calendar
   - (none)
9. [app] When the calendar port changes, the calendar server leaves the old port and answers on the new one, with events made before the change still there and new ones accepted — `docs/goal/behavior/caldav-server.md` § Network exposure
   - `tests/e2e-unified/tests/test_caldav_admin_port_rebind.py::test_admin_caldav_port_change_rebinds_listener`
10. [app] Each page shows its switch as the nest holds it, when the page opens and after a change — `docs/goal/behavior/admin.md` § 8. Calendar
    - (none)
11. [nest] An explicit setting of the contacts or files switch, or switching one of the three off while mail is on, is what the nest then reports — `docs/goal/behavior/caldav-server.md` § Independent enablement
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| linux | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| windows | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| macos | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+c438a386 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_calendar.py::test_calendar_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_enabled_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_contacts.py::test_contacts_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_contacts.py::test_carddav_enabled_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_files.py::test_files_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_files.py::test_webdav_enabled_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_port_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_calendar.py::test_caldav_port_invalid_surfaces_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_dav_enable_independence.py::test_unset_dav_toggles_follow_mail_and_only_an_explicit_set_witnesses_intent` | nest (linux): passed, nest (macos): passed |
| 4 | nest | (none) | — |
| 5 | nest | `tests/e2e-unified/tests/api/test_webdav_apex_discovery.py::test_the_standard_webdav_address_sends_a_file_app_on_to_the_files` | nest (linux): passed |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_caldav_admin_port_rebind.py::test_admin_caldav_port_change_rebinds_listener` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 10 | app | (none) | — |
| 11 | nest | (none) | — |
<!-- features-render:end -->
