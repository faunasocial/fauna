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

Stamped 2026-10-01 at dfa27a899f.

1. [app] The page lists every live sign-in with this app's own first and not revocable, ends one you pick, and signs out everywhere else while keeping this app signed in — `docs/goal/ui/sessions.md` § Layout & flow
   - `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else`
2. [app] Each sign-in says what it is: an app sign-in, one of your devices by name, or a device key that is not in your device list, the last shown plainly and never as an error — `docs/goal/ui/sessions.md` § State & data shape
   - (none)
3. [app] Each sign-in shows when it signed in, when it was last active and when it expires, and its address, or says plainly that the address was not recorded — `docs/goal/ui/sessions.md` § Errors & edge cases
   - (none)
4. [app] After this app's own sign-in come this device's other sign-ins, marked as this device's, then the rest with the most recently active first — `docs/goal/ui/sessions.md` § Layout & flow
   - (none)
5. [app] This app's own earlier sign-ins that have not yet expired show inside its one row, never as a stranger's sign-in to be ended — `docs/goal/behavior/devices.md` § The client's own session
   - (none)
6. [app] Ending every other sign-in keeps this app signed in even when the app renewed its own sign-in after the page was shown — `docs/goal/behavior/devices.md` § The client's own session
   - (none)
7. [app] Ending every other sign-in asks for a second press to confirm and can be cancelled, while ending one sign-in asks nothing — `docs/goal/ui/sessions.md` § The ruling
   - (none)
8. [app] Beside the controls the page says plainly that ending a sign-in does not sign a device out — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
   - (none)
9. [app] The same note says where the real remedies are: removing the device under Devices ends a device for good, and only the recovery kit ends someone who holds your secret key — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
   - (none)
10. [nest] Ending a sign-in at once closes every connection that was open with it — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
    - (none)
11. [nest] Ending one sign-in leaves your other sign-ins connected and working — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
    - (none)
12. [nest] You can end only your own sign-ins: naming someone else's ends nothing — `docs/goal/behavior/devices.md` § Session Management
    - (none)
13. [app] The emergency lock's warning is shown in full before you confirm, never only after — `docs/goal/ui/sessions.md` § Layout & flow
    - (none)
14. [app] The lock's warning says what the lock does: every device is signed out, this one too, and nobody can sign in for 24 hours, you included, with no way to unlock — `docs/goal/behavior/devices.md` § The two panic buttons
    - (none)
15. [app] The lock's warning says what the lock does not do: it does not remove someone who holds your secret key, who can lock you out the same way, and your recovery kit still works while the account is locked — `docs/goal/behavior/devices.md` § The two panic buttons
    - (none)
16. [app] Locking needs the word LOCK typed exactly, the same word in every language, and trying with the wrong word is refused out loud and locks nothing — `docs/goal/ui/sessions.md` § The ruling
    - (none)
17. [nest] A lock always lasts exactly 24 hours: no request can make it shorter or longer — `docs/goal/behavior/devices.md` § Emergency lockout
    - (none)
18. [nest] Locking ends every sign-in of the account at once, this app's included, and closes every open connection — `docs/goal/behavior/devices.md` § Emergency lockout
    - (none)
19. [nest] While the account is locked nobody can sign in to it, the owner included, and the refusal says when the lock ends — `docs/goal/behavior/login.md` § Silent Challenge
    - (none)
20. [nest] While the account is locked, nothing still connected as it can do anything: every request is refused and no new connection opens — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
    - (none)
21. [app] An app that meets a locked account shows a standing notice with the time the lock ends, and says that a lock you did not set means somebody holds your secret key — `docs/goal/behavior/devices.md` § The locked state
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
22. [app] From a locked app you can start taking your account back with your recovery kit, without signing in — `docs/goal/behavior/devices.md` § The two panic buttons
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
23. [app] Locking from this app takes it straight to the same locked notice — `docs/goal/behavior/devices.md` § The locked state
    - `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor`
24. [app] A locked app stops retrying, and signs back in by itself once the lock ends — `docs/goal/behavior/devices.md` § The locked state
    - (none)
25. [app] From the sign-in screen, without signing in, you can lock your account with your secret key and handle, for when your device was taken and you are on someone else's — `docs/goal/behavior/devices.md` § The signed-out door
    - (none)
26. [app] Locking from the sign-in screen leaves nothing of your account on that device — `docs/goal/ui/sessions.md` § Layout & flow
    - (none)
27. [nest] A lock request more than five minutes old is refused, so a captured one cannot be replayed later — `docs/goal/behavior/devices.md` § Emergency lockout
    - (none)
28. [app] If the list or an action fails, the page shows the error and keeps the sign-ins it already showed — `docs/goal/ui/sessions.md` § Errors & edge cases
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ❌ failing | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_sessions_page.py::test_list_revoke_one_then_sign_out_everywhere_else` | linux (linux): skipped, tui (linux): passed |
| 2 | app | (none) | — |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | linux (linux): skipped, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | linux (linux): skipped, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_locked_surface.py::test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor` | linux (linux): skipped, tui (linux): passed |
| 24 | app | (none) | — |
| 25 | app | (none) | — |
| 26 | app | (none) | — |
| 27 | nest | (none) | — |
| 28 | app | (none) | — |
<!-- features-render:end -->
