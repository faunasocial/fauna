---
slug: factory-reset
title: Factory reset and set up again
section: admin area
goal: docs/goal/architecture/nest/common.md § Factory reset
guide: docs/guides/admin-tour.md § Nest
---

## What a user gets

Factory reset wipes the nest and hands you a fresh claim code, pre-filled in
the app, so you can set it up again with the same identity; mail and calendars
come back working. Whatever the app was doing when it crashed, the nest is left in
a state the app can recover from, with no command line involved.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] After the admin resets the nest from the app, the app reopens on the claim step with the nest's new claim code already filled in, and submitting it claims the nest again — `docs/goal/architecture/nest/common.md` § Factory reset
   - `tests/e2e-unified/tests/test_factory_reset_reonboard.py::test_factory_reset_navigate_prefills_claim_code`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_factory_reset_floor_holds`
2. [app] If the app is killed in the middle of claiming, resetting, adding or removing a domain, or turning mail on or off — or the nest is killed in the middle of adding a domain, turning mail off or revoking a helper's key — reopening the app finds the nest either finished or ready to do it again, with no command line involved — `docs/goal/architecture/nest/common.md` § Client-state recoverability (absolute invariant)
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_claim_box_recovers`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_factory_reset_floor_holds`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_failed_factory_reset_does_not_trap_the_client`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_domain_add_box_recovers`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_domain_remove_box_recovers`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_enable_box_recovers`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_disable_box_recovers`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_domain_add_boot_reconciles`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_admin_mail_disable_reconciles`
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_mta_revoke_leaves_dkim_key_untouched`
3. [app] Factory reset asks the admin to confirm before anything is wiped — `docs/goal/behavior/admin.md` § N. Nest
   - (none)
4. [nest] Only an admin can factory-reset the nest — `docs/goal/architecture/nest/common.md` § Factory reset
   - (none)
5. [app] After a reset the nest keeps its identity, so an app that trusted it reconnects without an identity-changed warning — `docs/goal/architecture/nest/common.md` § Factory reset
   - (none)
6. [nest] A reset erases every account, handle, admin grant, domain, address and message on the nest — `docs/goal/architecture/nest/common.md` § Factory reset
   - (none)
7. [nest] The nest stays reachable on its certificate through a reset, so the admin can claim it again — `docs/goal/architecture/nest/common.md` § Factory reset
   - (none)
8. [app] A member whose nest was reset is told the nest no longer recognises them and is offered a fresh start, never quietly signed in to an empty account — `docs/goal/architecture/nest/common.md` § Factory reset
   - (none)
9. [app] If the app cannot safely save the new claim code first, it refuses to reset and says so — `docs/goal/architecture/nest/common.md` § Client-state recoverability
   - (none)
10. [app] If a reset never went through, the pre-filled claim step says the nest is already claimed and lets the admin back out — `docs/goal/architecture/nest/common.md` § Client-state recoverability
    - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_within_grace_failed_reset_honors_fresh_slot_with_visible_exit`
11. [app] If the nest cannot be reached when the app reopens mid-reset, the pending re-claim is kept for the next launch — `docs/goal/architecture/nest/common.md` § Client-state recoverability
    - (none)
12. [app] Each account on a device keeps its own pending reset, so resetting one nest never loses another account's claim code — `docs/goal/architecture/nest/common.md` § Client-state recoverability
    - (none)
13. [app] After a reset and a fresh claim, the calendar works in the app again: an event can be made and is shown — `docs/goal/architecture/nest/common.md` § Factory reset
    - `tests/e2e-unified/tests/test_factory_reset_calendar_reclaim.py::test_calendar_works_after_factory_reset_reclaim`
    - `tests/e2e-unified/tests/platform/docker/test_factory_reset_calendar_reclaim_docker.py::test_calendar_works_after_factory_reset_reclaim_docker`
14. [nest] After a reset and a fresh claim with mail switched on, the nest accepts and delivers mail again — `docs/goal/architecture/nest/common.md` § Factory reset
    - (none)
15. [app] Once a reset has wiped the app's own sign-in, the next launch starts from the beginning — choose or create an identity — and never tries to sign the old identity back in — `docs/goal/behavior/onboarding.md` § App-launch routing
    - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch`
16. [nest] Straight after a reset the nest serves nothing — mail, calendar, contacts and files are all off — until it is claimed and they are switched on again — `docs/goal/architecture/nest/common.md` § Factory reset
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_factory_reset_reonboard.py::test_factory_reset_navigate_prefills_claim_code` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_factory_reset_floor_holds` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_claim_box_recovers` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_factory_reset_floor_holds` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_failed_factory_reset_does_not_trap_the_client` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_domain_add_box_recovers` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_domain_remove_box_recovers` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_enable_box_recovers` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_disable_box_recovers` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_domain_add_boot_reconciles` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_admin_mail_disable_reconciles` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_nest_mid_mta_revoke_leaves_dkim_key_untouched` | web (linux): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | (none) | — |
| 4 | nest | (none) | — |
| 5 | app | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_within_grace_failed_reset_honors_fresh_slot_with_visible_exit` | linux (linux): passed |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | `tests/e2e-unified/tests/test_factory_reset_calendar_reclaim.py::test_calendar_works_after_factory_reset_reclaim` | web (linux): failed, linux (linux): passed, windows (windows): skipped, macos (macos): failed, ios (macos): failed, tui (linux): skipped |
| 13 | app | `tests/e2e-unified/tests/platform/docker/test_factory_reset_calendar_reclaim_docker.py::test_calendar_works_after_factory_reset_reclaim_docker` | linux (linux): passed |
| 14 | nest | (none) | — |
| 15 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | nest | (none) | — |
<!-- features-render:end -->
