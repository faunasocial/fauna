---
slug: sessions
title: Where you are signed in
section: your data and devices
goal: docs/goal/ui/sessions.md § Goal
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

The Sessions page shows every place your account is signed in right now, with
this app's own sign-in first, and lets you end any other one, or all of them at
once, while saying plainly that ending a sign-in does not sign a device out and
where the real remedies are. It also offers the 24-hour emergency lock.

## Coverage contract

Stamped 2026-10-09 at a0eef3f66d.

1. [app] The page lists every live sign-in with this app's own first and not revocable, ends one you pick, and signs out everywhere else while keeping this app signed in — `docs/goal/ui/sessions.md` § Layout & flow
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
2. [app] Each sign-in says what it is: an app sign-in, one of your devices by name, or a device key that is not in your device list, the last shown plainly and never as an error — `docs/goal/ui/sessions.md` § State & data shape
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
3. [app] Each sign-in shows when it signed in, when it was last active and when it expires, and its address, or says plainly that the address was not recorded — `docs/goal/ui/sessions.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
4. [app] After this app's own sign-in come this device's other sign-ins, marked as this device's, then the rest with the most recently active first — `docs/goal/ui/sessions.md` § Layout & flow
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
5. [app] This app's own earlier sign-ins that have not yet expired show inside its one row, never as a stranger's sign-in to be ended — `docs/goal/behavior/devices.md` § The client's own session
   - `tests/e2e-unified/tests/test_sessions_page.py::test_own_renewals_fold_into_one_row_and_survive_sign_out_everywhere_else`
6. [app] Ending every other sign-in keeps this app signed in even when the app renewed its own sign-in after the page was shown — `docs/goal/behavior/devices.md` § The client's own session
   - `tests/e2e-unified/tests/test_sessions_page.py::test_own_renewals_fold_into_one_row_and_survive_sign_out_everywhere_else`
7. [app] Ending every other sign-in asks for a second press to confirm and can be cancelled, while ending one sign-in asks nothing — `docs/goal/ui/sessions.md` § The ruling
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
8. [app] Beside the controls the page says plainly that ending a sign-in does not sign a device out — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
9. [app] The same note says where the real remedies are: removing the device under Devices ends a device for good, and only the recovery kit ends someone who holds your secret key — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
10. [nest] Ending a sign-in at once closes every connection that was open with it — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_ending_a_sign_in_closes_its_connections_at_once_and_leaves_the_others_working`
11. [nest] Ending one sign-in leaves your other sign-ins connected and working — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_ending_a_sign_in_closes_its_connections_at_once_and_leaves_the_others_working`
12. [nest] You can end only your own sign-ins: naming someone else's ends nothing — `docs/goal/behavior/devices.md` § Session Management
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_naming_someone_elses_sign_in_ends_nothing`
13. [app] The emergency lock's warning is shown in full before you confirm, never only after — `docs/goal/ui/sessions.md` § Layout & flow
    - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
14. [app] The lock's warning says what the lock does: every device is signed out, this one too, and nobody can sign in for 24 hours, you included, with no way to unlock — `docs/goal/behavior/devices.md` § The two panic buttons
    - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
15. [app] The lock's warning says what the lock does not do: it does not remove someone who holds your secret key, who can lock you out the same way, and your recovery kit still works while the account is locked — `docs/goal/behavior/devices.md` § The two panic buttons
    - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
16. [app] Locking needs the word LOCK typed exactly, the same word in every language, and trying with the wrong word is refused out loud and locks nothing — `docs/goal/ui/sessions.md` § The ruling
    - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
17. [nest] A lock always lasts exactly 24 hours: no request can make it shorter or longer — `docs/goal/behavior/devices.md` § Emergency lockout
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_a_lock_lasts_exactly_24_hours_whatever_the_request_asks`
18. [nest] Locking ends every sign-in of the account at once, this app's included, and closes every open connection — `docs/goal/behavior/devices.md` § Emergency lockout
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_locking_ends_every_sign_in_and_closes_every_connection_the_pressers_included`
19. [nest] While the account is locked nobody can sign in to it, the owner included, and the refusal says when the lock ends — `docs/goal/behavior/login.md` § Silent Challenge
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_while_locked_nobody_signs_in_and_the_refusal_says_when_the_lock_ends`
20. [nest] While the account is locked, nothing still connected as it can do anything: every request is refused and no new connection opens — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_while_locked_nothing_still_connected_can_act_and_no_new_connection_opens`
21. [app] An app that meets a locked account shows a standing notice with the time the lock ends, and says that a lock you did not set means somebody holds your secret key — `docs/goal/behavior/devices.md` § The locked state
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
22. [app] From a locked app you can start taking your account back with your recovery kit, without signing in — `docs/goal/behavior/devices.md` § The two panic buttons
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
23. [app] Locking from this app takes it straight to the same locked notice — `docs/goal/behavior/devices.md` § The locked state
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
24. [app] A locked app stops retrying, and signs back in by itself once the lock ends — `docs/goal/behavior/devices.md` § The locked state
    - `tests/e2e-unified/tests/test_locked_surface.py::test_a_locked_app_stops_retrying_and_signs_back_in_when_the_lock_ends`
25. [app] From the sign-in screen, without signing in, you can lock your account with your secret key and handle, for when your device was taken and you are on someone else's — `docs/goal/behavior/devices.md` § The signed-out door
    - `tests/e2e-unified/tests/test_emergency_lockout_door.py::test_the_door_locks_an_account_from_a_device_that_holds_nothing`
26. [app] Locking from the sign-in screen leaves nothing of your account on that device — `docs/goal/ui/sessions.md` § Layout & flow
    - `tests/e2e-unified/tests/test_emergency_lockout_door.py::test_the_door_locks_an_account_from_a_device_that_holds_nothing`
27. [nest] A lock request more than five minutes old is refused, so a captured one cannot be replayed later — `docs/goal/behavior/devices.md` § Emergency lockout
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_a_lock_request_more_than_five_minutes_old_is_refused`
28. [app] If the list or an action fails, the page shows the error and keeps the sign-ins it already showed — `docs/goal/ui/sessions.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_own_renewals_fold_into_one_row_and_survive_sign_out_everywhere_else` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_own_renewals_fold_into_one_row_and_survive_sign_out_everywhere_else` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_ending_a_sign_in_closes_its_connections_at_once_and_leaves_the_others_working` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_ending_a_sign_in_closes_its_connections_at_once_and_leaves_the_others_working` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_naming_someone_elses_sign_in_ends_nothing` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_a_lock_lasts_exactly_24_hours_whatever_the_request_asks` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_locking_ends_every_sign_in_and_closes_every_connection_the_pressers_included` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_while_locked_nobody_signs_in_and_the_refusal_says_when_the_lock_ends` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_while_locked_nothing_still_connected_can_act_and_no_new_connection_opens` | nest (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_a_locked_app_stops_retrying_and_signs_back_in_when_the_lock_ends` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_emergency_lockout_door.py::test_the_door_locks_an_account_from_a_device_that_holds_nothing` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_emergency_lockout_door.py::test_the_door_locks_an_account_from_a_device_that_holds_nothing` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_a_lock_request_more_than_five_minutes_old_is_refused` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | web (linux): failed, linux (linux): failed, tui (linux): passed |
<!-- features-render:end -->
